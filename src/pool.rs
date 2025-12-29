//! Core object pool implementations

use crate::config::PoolConfiguration;
use crate::errors::{PoolError, PoolResult};
use crate::health::{HealthStatus, HealthTracker};
use crate::metrics::{MetricsExporter, MetricsTracker, PoolMetrics};
use crate::eviction::{EvictionPolicy, EvictionTracker};
use crate::circuit_breaker::CircuitBreaker;

use crossbeam::queue::ArrayQueue;
use dashmap::DashMap;
use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// A pooled object that automatically returns to the pool when dropped
///
/// Objects are automatically returned when they go out of scope (RAII pattern).
///
/// # Examples
///
/// ```
/// use objectpool::{ObjectPool, PoolConfiguration};
///
/// let pool = ObjectPool::new(vec![1, 2, 3], PoolConfiguration::default());
///
/// {
///     let obj = pool.get_object().unwrap();
///     println!("Using: {}", *obj);
///     // Object automatically returned here when `obj` is dropped
/// }
///
/// assert_eq!(pool.available_count(), 3);
/// ```
pub struct PooledObject<T> {
    value: Option<T>,
    object_id: usize,
    return_fn: Arc<dyn Fn(T, usize) + Send + Sync>,
}

impl<T: std::fmt::Debug> std::fmt::Debug for PooledObject<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PooledObject")
            .field("value", &self.value)
            .field("object_id", &self.object_id)
            .finish()
    }
}

impl<T> PooledObject<T> {
    fn new(value: T, object_id: usize, return_fn: Arc<dyn Fn(T, usize) + Send + Sync>) -> Self {
        Self {
            value: Some(value),
            object_id,
            return_fn,
        }
    }
    
    /// Get the inner value without returning to pool
    pub fn unwrap(mut self) -> T {
        self.value.take().expect("Value already taken")
    }
}

impl<T> Deref for PooledObject<T> {
    type Target = T;
    
    fn deref(&self) -> &Self::Target {
        self.value.as_ref().expect("Value already taken")
    }
}

impl<T> DerefMut for PooledObject<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.value.as_mut().expect("Value already taken")
    }
}

impl<T> Drop for PooledObject<T> {
    fn drop(&mut self) {
        if let Some(value) = self.value.take() {
            (self.return_fn)(value, self.object_id);
        }
    }
}

/// Thread-safe object pool with fixed set of objects
///
/// # Examples
///
/// ```
/// use objectpool::{ObjectPool, PoolConfiguration};
///
/// let pool = ObjectPool::new(vec![1, 2, 3], PoolConfiguration::default());
/// 
/// // Get an object - automatically returned when dropped
/// {
///     let obj = pool.get_object().unwrap();
///     assert!(vec![1, 2, 3].contains(&*obj));
/// }
/// 
/// // Object returned, pool refilled
/// assert_eq!(pool.available_count(), 3);
/// ```
pub struct ObjectPool<T: Send> {
    available: Arc<ArrayQueue<(T, usize)>>,
    active: Arc<DashMap<usize, ()>>,
    config: Arc<PoolConfiguration<T>>,
    metrics: Arc<MetricsTracker>,
    health: Arc<HealthTracker>,
    eviction: Arc<EvictionTracker<T>>,
    circuit_breaker: Option<Arc<CircuitBreaker>>,
    next_id: Arc<AtomicUsize>,
    capacity: usize,
}

impl<T: Send + Sync + 'static> ObjectPool<T> {
    /// Create a new object pool with initial objects
    ///
    /// # Examples
    ///
    /// ```
    /// use objectpool::{ObjectPool, PoolConfiguration};
    ///
    /// let config = PoolConfiguration::new().with_max_pool_size(10);
    /// let pool = ObjectPool::new(vec![1, 2, 3], config);
    /// assert_eq!(pool.available_count(), 3);
    /// ```
    pub fn new(objects: Vec<T>, config: PoolConfiguration<T>) -> Self {
        let capacity = objects.len().max(config.max_pool_size);
        let available = Arc::new(ArrayQueue::new(capacity));
        
        let eviction_policy = if let Some(ttl) = config.time_to_live {
            if let Some(idle) = config.idle_timeout {
                EvictionPolicy::Combined {
                    ttl,
                    idle_timeout: idle,
                }
            } else {
                EvictionPolicy::TimeToLive(ttl)
            }
        } else if let Some(idle) = config.idle_timeout {
            EvictionPolicy::IdleTimeout(idle)
        } else {
            EvictionPolicy::None
        };
        
        let eviction = Arc::new(EvictionTracker::new(eviction_policy));
        
        // Add objects to pool
        for (idx, obj) in objects.into_iter().enumerate() {
            eviction.track_object(idx);
            let _ = available.push((obj, idx));
        }
        
        let circuit_breaker = if config.enable_circuit_breaker {
            Some(Arc::new(CircuitBreaker::new(
                config.circuit_breaker_threshold,
                config.circuit_breaker_timeout,
            )))
        } else {
            None
        };
        
        Self {
            available,
            active: Arc::new(DashMap::new()),
            config: Arc::new(config),
            metrics: Arc::new(MetricsTracker::new()),
            health: Arc::new(HealthTracker::new()),
            eviction,
            circuit_breaker,
            next_id: Arc::new(AtomicUsize::new(capacity)),
            capacity,
        }
    }
    
    /// Get an object from the pool (blocking)
    ///
    /// Returns an error if the pool is empty or circuit breaker is open.
    ///
    /// # Examples
    ///
    /// ```
    /// use objectpool::{ObjectPool, PoolConfiguration};
    ///
    /// let pool = ObjectPool::new(vec![42], PoolConfiguration::default());
    /// let obj = pool.get_object().unwrap();
    /// assert_eq!(*obj, 42);
    /// ```
    pub fn get_object(&self) -> PoolResult<PooledObject<T>> {
        self.check_circuit_breaker()?;
        self.check_max_active()?;
        
        // Try to get available object
        loop {
            match self.available.pop() {
                Some((obj, id)) => {
                    // Check if expired
                    if self.eviction.is_expired(id) {
                        self.eviction.remove_object(id);
                        continue;
                    }
                    
                    self.active.insert(id, ());
                    self.eviction.touch_object(id);
                    self.metrics.total_retrieved.fetch_add(1, Ordering::Relaxed);
                    self.health.increment_retrieved();
                    
                    if let Some(ref cb) = self.circuit_breaker {
                        cb.record_success();
                    }
                    
                    let return_fn = self.make_return_fn();
                    return Ok(PooledObject::new(obj, id, return_fn));
                }
                None => {
                    self.metrics.pool_empty_events.fetch_add(1, Ordering::Relaxed);
                    self.health.increment_empty();
                    
                    if let Some(ref cb) = self.circuit_breaker {
                        cb.record_failure();
                    }
                    
                    return Err(PoolError::PoolEmpty);
                }
            }
        }
    }
    
    /// Try to get an object without throwing error
    ///
    /// Returns `None` if pool is empty instead of an error.
    ///
    /// # Examples
    ///
    /// ```
    /// use objectpool::{ObjectPool, PoolConfiguration};
    ///
    /// let pool = ObjectPool::new(vec![1], PoolConfiguration::default());
    /// 
    /// let obj1 = pool.try_get_object();
    /// assert!(obj1.is_some());
    /// 
    /// let obj2 = pool.try_get_object();
    /// assert!(obj2.is_none()); // Pool empty
    /// ```
    pub fn try_get_object(&self) -> Option<PooledObject<T>> {
        self.get_object().ok()
    }
    
    /// Get an object asynchronously with timeout
    pub async fn get_object_async(&self) -> PoolResult<PooledObject<T>> {
        let timeout = self.config.operation_timeout.unwrap_or(Duration::from_secs(30));
        
        tokio::time::timeout(timeout, async {
            loop {
                match self.try_get_object() {
                    Some(obj) => return Ok(obj),
                    None => {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }
            }
        })
        .await
        .map_err(|_| PoolError::Timeout(timeout))?
    }
    
    /// Try to get an object asynchronously
    pub async fn try_get_object_async(&self) -> Option<PooledObject<T>> {
        self.get_object_async().await.ok()
    }
    
    /// Get health status
    pub fn get_health_status(&self) -> HealthStatus {
        let available = self.available.len();
        let active = self.active.len();
        HealthStatus::new(available, active, self.capacity)
    }
    
    /// Export metrics
    pub fn export_metrics(&self) -> HashMap<String, String> {
        let metrics = self.get_metrics();
        metrics.export()
    }
    
    /// Export metrics in Prometheus format
    pub fn export_metrics_prometheus(
        &self,
        pool_name: &str,
        tags: Option<&HashMap<String, String>>,
    ) -> String {
        let metrics = self.get_metrics();
        MetricsExporter::export_prometheus(&metrics, pool_name, tags)
    }
    
    /// Get pool metrics
    pub fn get_metrics(&self) -> PoolMetrics {
        self.metrics.get_metrics(
            self.active.len(),
            self.available.len(),
            self.capacity,
        )
    }
    
    /// Get available count
    pub fn available_count(&self) -> usize {
        self.available.len()
    }
    
    /// Get active count
    pub fn active_count(&self) -> usize {
        self.active.len()
    }
    
    fn check_circuit_breaker(&self) -> PoolResult<()> {
        if let Some(ref cb) = self.circuit_breaker
            && !cb.allow_request()
        {
            return Err(PoolError::CircuitBreakerOpen);
        }
        Ok(())
    }
    
    fn check_max_active(&self) -> PoolResult<()> {
        if let Some(max) = self.config.max_active_objects
            && self.active.len() >= max
        {
            return Err(PoolError::MaxActiveObjectsReached);
        }
        Ok(())
    }
    
    fn make_return_fn(&self) -> Arc<dyn Fn(T, usize) + Send + Sync> {
        let available = Arc::clone(&self.available);
        let active = Arc::clone(&self.active);
        let metrics = Arc::clone(&self.metrics);
        let health = Arc::clone(&self.health);
        let eviction = Arc::clone(&self.eviction);
        let config = Arc::clone(&self.config);
        
        Arc::new(move |obj, id| {
            // Validate if configured
            if config.validate_on_return
                && let Some(validate) = config.validation_function
                && !validate(&obj)
            {
                metrics.validation_failures.fetch_add(1, Ordering::Relaxed);
                active.remove(&id);
                eviction.remove_object(id);
                return;
            }
            
            eviction.touch_object(id);
            active.remove(&id);
            let _ = available.push((obj, id));
            metrics.total_returned.fetch_add(1, Ordering::Relaxed);
            health.increment_returned();
        })
    }
}

/// Queryable object pool - find objects matching a predicate
///
/// # Examples
///
/// ```
/// use objectpool::{QueryableObjectPool, PoolConfiguration};
///
/// #[derive(Clone)]
/// struct Connection { id: u32 }
///
/// let pool = QueryableObjectPool::new(
///     vec![Connection { id: 1 }, Connection { id: 2 }],
///     PoolConfiguration::default()
/// );
///
/// let conn = pool.get_object(|c| c.id == 2).unwrap();
/// assert_eq!(conn.id, 2);
/// ```
pub struct QueryableObjectPool<T: Send> {
    inner: ObjectPool<T>,
}

impl<T: Send + Sync + Clone + 'static> QueryableObjectPool<T> {
    /// Create a new queryable pool
    pub fn new(objects: Vec<T>, config: PoolConfiguration<T>) -> Self {
        Self {
            inner: ObjectPool::new(objects, config),
        }
    }
    
    /// Get an object matching the query predicate
    pub fn get_object<F>(&self, query: F) -> PoolResult<PooledObject<T>>
    where
        F: Fn(&T) -> bool,
    {
        self.inner.check_circuit_breaker()?;
        self.inner.check_max_active()?;
        
        // Collect all available objects temporarily
        let mut temp_storage = Vec::new();
        let mut found = None;
        
        while let Some((obj, id)) = self.inner.available.pop() {
            if self.inner.eviction.is_expired(id) {
                self.inner.eviction.remove_object(id);
                continue;
            }
            
            if found.is_none() && query(&obj) {
                found = Some((obj, id));
            } else {
                temp_storage.push((obj, id));
            }
        }
        
        // Return non-matching objects
        for item in temp_storage {
            let _ = self.inner.available.push(item);
        }
        
        if let Some((obj, id)) = found {
            self.inner.active.insert(id, ());
            self.inner.eviction.touch_object(id);
            self.inner.metrics.total_retrieved.fetch_add(1, Ordering::Relaxed);
            self.inner.health.increment_retrieved();
            
            if let Some(ref cb) = self.inner.circuit_breaker {
                cb.record_success();
            }
            
            let return_fn = self.inner.make_return_fn();
            Ok(PooledObject::new(obj, id, return_fn))
        } else {
            if let Some(ref cb) = self.inner.circuit_breaker {
                cb.record_failure();
            }
            Err(PoolError::NoMatchFound)
        }
    }
    
    /// Try to get an object matching query
    pub fn try_get_object<F>(&self, query: F) -> Option<PooledObject<T>>
    where
        F: Fn(&T) -> bool,
    {
        self.get_object(query).ok()
    }
    
    /// Get an object matching query asynchronously
    pub async fn get_object_async<F>(&self, query: F) -> PoolResult<PooledObject<T>>
    where
        F: Fn(&T) -> bool + Send + Sync + 'static,
    {
        let timeout = self.inner.config.operation_timeout.unwrap_or(Duration::from_secs(30));
        
        tokio::time::timeout(timeout, async {
            loop {
                match self.try_get_object(&query) {
                    Some(obj) => return Ok(obj),
                    None => {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }
            }
        })
        .await
        .map_err(|_| PoolError::Timeout(timeout))?
    }
    
    // Delegate methods to inner pool
    pub fn get_health_status(&self) -> HealthStatus {
        self.inner.get_health_status()
    }
    
    pub fn export_metrics(&self) -> HashMap<String, String> {
        self.inner.export_metrics()
    }
    
    pub fn export_metrics_prometheus(
        &self,
        pool_name: &str,
        tags: Option<&HashMap<String, String>>,
    ) -> String {
        self.inner.export_metrics_prometheus(pool_name, tags)
    }
}

/// Dynamic object pool - creates objects on demand
///
/// # Examples
///
/// ```
/// use objectpool::{DynamicObjectPool, PoolConfiguration};
///
/// let pool = DynamicObjectPool::new(
///     || 42,
///     PoolConfiguration::new().with_max_pool_size(10)
/// );
///
/// let obj = pool.get_object().unwrap();
/// assert_eq!(*obj, 42);
/// ```
pub struct DynamicObjectPool<T: Send> {
    inner: ObjectPool<T>,
    factory: Arc<dyn Fn() -> T + Send + Sync>,
}

impl<T: Send + Sync + 'static> DynamicObjectPool<T> {
    /// Create a new dynamic pool with factory function
    pub fn new<F>(factory: F, config: PoolConfiguration<T>) -> Self
    where
        F: Fn() -> T + Send + Sync + 'static,
    {
        let initial_objects = Vec::new();
        Self {
            inner: ObjectPool::new(initial_objects, config),
            factory: Arc::new(factory),
        }
    }
    
    /// Create a dynamic pool with initial objects and factory
    pub fn with_initial<F>(factory: F, initial_objects: Vec<T>, config: PoolConfiguration<T>) -> Self
    where
        F: Fn() -> T + Send + Sync + 'static,
    {
        Self {
            inner: ObjectPool::new(initial_objects, config),
            factory: Arc::new(factory),
        }
    }
    
    /// Get an object, creating one if pool is empty
    pub fn get_object(&self) -> PoolResult<PooledObject<T>> {
        match self.inner.try_get_object() {
            Some(obj) => Ok(obj),
            None => {
                // Create new object if under capacity
                if self.inner.active.len() < self.inner.capacity {
                    let obj = (self.factory)();
                    let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
                    
                    self.inner.eviction.track_object(id);
                    self.inner.active.insert(id, ());
                    self.inner.metrics.total_retrieved.fetch_add(1, Ordering::Relaxed);
                    self.inner.health.increment_retrieved();
                    
                    let return_fn = self.inner.make_return_fn();
                    Ok(PooledObject::new(obj, id, return_fn))
                } else {
                    Err(PoolError::PoolFull)
                }
            }
        }
    }
    
    /// Try to get an object
    pub fn try_get_object(&self) -> Option<PooledObject<T>> {
        self.get_object().ok()
    }
    
    /// Get an object asynchronously
    pub async fn get_object_async(&self) -> PoolResult<PooledObject<T>> {
        let timeout = self.inner.config.operation_timeout.unwrap_or(Duration::from_secs(30));
        
        tokio::time::timeout(timeout, async {
            loop {
                match self.try_get_object() {
                    Some(obj) => return Ok(obj),
                    None => {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }
            }
        })
        .await
        .map_err(|_| PoolError::Timeout(timeout))?
    }
    
    /// Warm up the pool by pre-creating objects
    ///
    /// Pre-populates the pool to avoid cold-start latency.
    ///
    /// # Examples
    ///
    /// ```
    /// use objectpool::{DynamicObjectPool, PoolConfiguration};
    ///
    /// let pool = DynamicObjectPool::new(
    ///     || 42,
    ///     PoolConfiguration::default()
    /// );
    ///
    /// pool.warmup(5).unwrap();
    /// 
    /// let health = pool.get_health_status();
    /// assert_eq!(health.available_objects, 5);
    /// ```
    pub fn warmup(&self, count: usize) -> PoolResult<()> {
        for _ in 0..count.min(self.inner.capacity) {
            let obj = (self.factory)();
            let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
            self.inner.eviction.track_object(id);
            
            if self.inner.available.push((obj, id)).is_err() {
                break;
            }
        }
        Ok(())
    }
    
    /// Warm up asynchronously
    pub async fn warmup_async(&self, count: usize) -> PoolResult<()> {
        let factory = Arc::clone(&self.factory);
        let available = Arc::clone(&self.inner.available);
        let next_id = Arc::clone(&self.inner.next_id);
        let eviction = Arc::clone(&self.inner.eviction);
        let capacity = self.inner.capacity;
        
        tokio::task::spawn_blocking(move || {
            for _ in 0..count.min(capacity) {
                let obj = factory();
                let id = next_id.fetch_add(1, Ordering::Relaxed);
                eviction.track_object(id);
                
                if available.push((obj, id)).is_err() {
                    break;
                }
            }
        })
        .await
        .map_err(|_| PoolError::Cancelled)?;
        
        Ok(())
    }
    
    // Delegate methods
    pub fn get_health_status(&self) -> HealthStatus {
        self.inner.get_health_status()
    }
    
    pub fn export_metrics(&self) -> HashMap<String, String> {
        self.inner.export_metrics()
    }
    
    pub fn export_metrics_prometheus(
        &self,
        pool_name: &str,
        tags: Option<&HashMap<String, String>>,
    ) -> String {
        self.inner.export_metrics_prometheus(pool_name, tags)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_object_pool_basic() {
        let pool = ObjectPool::new(vec![1, 2, 3], PoolConfiguration::default());
        
        {
            let obj = pool.get_object().unwrap();
            assert!(vec![1, 2, 3].contains(&*obj));
        }
        
        assert_eq!(pool.available_count(), 3);
    }
    
    #[test]
    fn test_queryable_pool() {
        let pool = QueryableObjectPool::new(vec![1, 2, 3], PoolConfiguration::default());
        
        {
            let obj = pool.get_object(|x| *x == 2).unwrap();
            assert_eq!(*obj, 2);
        }
    }
    
    #[test]
    fn test_dynamic_pool() {
        let pool = DynamicObjectPool::new(|| 42, PoolConfiguration::default());
        
        {
            let obj = pool.get_object().unwrap();
            assert_eq!(*obj, 42);
        }
    }
    
    #[tokio::test]
    async fn test_async_get() {
        let pool = ObjectPool::new(vec![1, 2, 3], PoolConfiguration::default());
        
        {
            let obj = pool.get_object_async().await.unwrap();
            assert!(vec![1, 2, 3].contains(&*obj));
        }
    }
    
    #[test]
    fn test_pool_empty_error() {
        let pool = ObjectPool::new(vec![1], PoolConfiguration::default());
        
        let _obj1 = pool.get_object().unwrap();
        let result = pool.get_object();
        
        assert!(result.is_err());
        if let Err(e) = result {
            assert!(matches!(e, PoolError::PoolEmpty));
        }
    }
    
    #[test]
    fn test_try_methods() {
        let pool = ObjectPool::new(vec![42], PoolConfiguration::default());
        
        let obj1 = pool.try_get_object();
        assert!(obj1.is_some());
        if let Some(ref obj) = obj1 {
            assert_eq!(**obj, 42);
        }
        
        let obj2 = pool.try_get_object();
        assert!(obj2.is_none());
        
        drop(obj1);
        
        let obj3 = pool.try_get_object();
        assert!(obj3.is_some());
    }
    
    #[test]
    fn test_max_active_objects() {
        let config = PoolConfiguration::new()
            .with_max_active_objects(2);
        
        let pool = ObjectPool::new(vec![1, 2, 3, 4, 5], config);
        
        let _obj1 = pool.get_object().unwrap();
        let _obj2 = pool.get_object().unwrap();
        
        let result = pool.get_object();
        assert!(result.is_err());
        if let Err(e) = result {
            assert!(matches!(e, PoolError::MaxActiveObjectsReached));
        }
    }
    
    #[test]
    fn test_validation_on_return() {
        let config = PoolConfiguration::new()
            .with_validation(|x| *x > 0);
        
        let pool = ObjectPool::new(vec![1, 2, 3], config);
        
        {
            let obj = pool.get_object().unwrap();
            assert_eq!(*obj, 1);
        }
        
        assert_eq!(pool.available_count(), 3);
    }
    
    #[test]
    fn test_metrics_tracking() {
        let pool = ObjectPool::new(vec![1, 2, 3], PoolConfiguration::default());
        
        {
            let _obj1 = pool.get_object().unwrap();
            let _obj2 = pool.get_object().unwrap();
            
            let metrics = pool.get_metrics();
            assert_eq!(metrics.total_retrieved, 2);
            assert_eq!(metrics.active_objects, 2);
            assert_eq!(metrics.available_objects, 1);
        }
        
        let metrics = pool.get_metrics();
        assert_eq!(metrics.total_returned, 2);
        assert_eq!(metrics.active_objects, 0);
        assert_eq!(metrics.available_objects, 3);
    }
    
    #[test]
    fn test_health_status() {
        let config = PoolConfiguration::new()
            .with_max_pool_size(5);
        
        let pool = ObjectPool::new(vec![1, 2, 3, 4, 5], config);
        
        {
            let _obj1 = pool.get_object().unwrap();
            let _obj2 = pool.get_object().unwrap();
            
            let health = pool.get_health_status();
            assert!(health.is_healthy);
            assert_eq!(health.active_objects, 2);
            assert_eq!(health.available_objects, 3);
            assert_eq!(health.total_capacity, 5);
        }
    }
    
    #[test]
    fn test_prometheus_export() {
        let pool = ObjectPool::new(vec![1, 2, 3], PoolConfiguration::default());
        
        {
            let _obj = pool.get_object().unwrap();
            
            let prometheus = pool.export_metrics_prometheus("test_pool", None);
            
            assert!(prometheus.contains("objectpool_objects_active"));
            assert!(prometheus.contains("objectpool_objects_available"));
            assert!(prometheus.contains("objectpool_utilization"));
            assert!(prometheus.contains("test_pool"));
        }
    }
    
    #[test]
    fn test_prometheus_with_tags() {
        let pool = ObjectPool::new(vec![1, 2, 3], PoolConfiguration::default());
        
        let mut tags = HashMap::new();
        tags.insert("env".to_string(), "test".to_string());
        tags.insert("service".to_string(), "api".to_string());
        
        let prometheus = pool.export_metrics_prometheus("test_pool", Some(&tags));
        
        assert!(prometheus.contains("env=\"test\""));
        assert!(prometheus.contains("service=\"api\""));
    }
    
    #[test]
    fn test_queryable_no_match() {
        let pool = QueryableObjectPool::new(vec![1, 2, 3], PoolConfiguration::default());
        
        let result = pool.get_object(|x| *x == 99);
        
        assert!(result.is_err());
        if let Err(e) = result {
            assert!(matches!(e, PoolError::NoMatchFound));
        }
    }
    
    #[test]
    fn test_queryable_multiple_matches() {
        let pool = QueryableObjectPool::new(vec![1, 2, 3, 2, 4], PoolConfiguration::default());
        
        let obj = pool.get_object(|x| *x == 2).unwrap();
        assert_eq!(*obj, 2);
        
        // Should still find the other 2
        let obj2 = pool.get_object(|x| *x == 2).unwrap();
        assert_eq!(*obj2, 2);
    }
    
    #[test]
    fn test_dynamic_pool_creation() {
        let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count_clone = call_count.clone();
        
        let pool = DynamicObjectPool::new(
            move || {
                count_clone.fetch_add(1, Ordering::Relaxed);
                42
            },
            PoolConfiguration::new().with_max_pool_size(5),
        );
        
        let _obj1 = pool.get_object().unwrap();
        let _obj2 = pool.get_object().unwrap();
        
        assert_eq!(call_count.load(Ordering::Relaxed), 2);
    }
    
    #[test]
    fn test_dynamic_pool_warmup() {
        let pool = DynamicObjectPool::new(
            || 42,
            PoolConfiguration::new().with_max_pool_size(10),
        );
        
        pool.warmup(5).unwrap();
        
        let health = pool.get_health_status();
        assert_eq!(health.available_objects, 5);
    }
    
    #[test]
    fn test_dynamic_pool_max_capacity() {
        let pool = DynamicObjectPool::new(
            || 42,
            PoolConfiguration::new().with_max_pool_size(2),
        );
        
        let _obj1 = pool.get_object().unwrap();
        let _obj2 = pool.get_object().unwrap();
        let result = pool.get_object();
        
        assert!(result.is_err());
        if let Err(e) = result {
            assert!(matches!(e, PoolError::PoolFull));
        }
    }
    
    #[test]
    fn test_eviction_ttl() {
        use std::thread;
        
        let config = PoolConfiguration::new()
            .with_ttl(Duration::from_millis(100));
        
        let pool = ObjectPool::new(vec![1, 2, 3], config);
        
        thread::sleep(Duration::from_millis(150));
        
        // Objects should be expired and filtered out
        let obj = pool.try_get_object();
        assert!(obj.is_none() || pool.available_count() < 3);
    }
    
    #[test]
    fn test_circuit_breaker_opens() {
        let config = PoolConfiguration::new()
            .with_circuit_breaker(3, Duration::from_secs(60));
        
        let pool = ObjectPool::new(vec![1], config);
        
        let _obj = pool.get_object().unwrap();
        
        // Cause failures
        for _ in 0..3 {
            let _ = pool.try_get_object();
        }
        
        // Circuit breaker should be open now
        let result = pool.get_object();
        assert!(result.is_err());
    }
    
    #[tokio::test]
    async fn test_async_timeout() {
        let config = PoolConfiguration::new()
            .with_timeout(Duration::from_millis(50));
        
        let pool = ObjectPool::new(vec![1], config);
        
        let _obj = pool.get_object().unwrap();
        
        let result = pool.get_object_async().await;
        
        assert!(result.is_err());
        if let Err(e) = result {
            assert!(matches!(e, PoolError::Timeout(_)));
        }
    }
    
    #[tokio::test]
    async fn test_concurrent_access() {
        use std::sync::Arc;
        
        let pool = Arc::new(ObjectPool::new(
            vec![1, 2, 3, 4, 5],
            PoolConfiguration::default(),
        ));
        
        let mut handles = vec![];
        
        for _ in 0..10 {
            let pool_clone = Arc::clone(&pool);
            let handle = tokio::spawn(async move {
                if let Some(obj) = pool_clone.try_get_object() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    drop(obj);
                    true
                } else {
                    false
                }
            });
            handles.push(handle);
        }
        
        let mut success_count = 0;
        for handle in handles {
            if handle.await.unwrap() {
                success_count += 1;
            }
        }
        
        // At least 5 should succeed (pool size)
        assert!(success_count >= 5);
        assert_eq!(pool.available_count(), 5);
    }
    
    #[tokio::test]
    async fn test_queryable_async() {
        let pool = QueryableObjectPool::new(vec![1, 2, 3, 4, 5], PoolConfiguration::default());
        
        let obj = pool.get_object_async(|x| *x > 3).await.unwrap();
        assert!(*obj > 3);
    }
    
    #[tokio::test]
    async fn test_dynamic_warmup_async() {
        let pool = DynamicObjectPool::new(
            || 42,
            PoolConfiguration::new().with_max_pool_size(10),
        );
        
        pool.warmup_async(7).await.unwrap();
        
        assert_eq!(pool.get_health_status().available_objects, 7);
    }
    
    #[test]
    fn test_pool_reuse_after_drop() {
        let pool = ObjectPool::new(vec![1, 2, 3], PoolConfiguration::default());
        
        for _ in 0..100 {
            {
                let obj = pool.get_object().unwrap();
                assert!(vec![1, 2, 3].contains(&*obj));
            }
            assert_eq!(pool.available_count(), 3);
        }
    }
    
    #[test]
    fn test_multiple_pools() {
        let pool1 = ObjectPool::new(vec![1, 2], PoolConfiguration::default());
        let pool2 = ObjectPool::new(vec![3, 4], PoolConfiguration::default());
        
        let obj1 = pool1.get_object().unwrap();
        let obj2 = pool2.get_object().unwrap();
        
        assert!(vec![1, 2].contains(&*obj1));
        assert!(vec![3, 4].contains(&*obj2));
    }
    
    #[test]
    fn test_export_metrics_map() {
        let pool = ObjectPool::new(vec![1, 2, 3], PoolConfiguration::default());
        
        {
            let _obj = pool.get_object().unwrap();
        }
        
        let metrics_map = pool.export_metrics();
        
        assert!(metrics_map.contains_key("total_retrieved"));
        assert!(metrics_map.contains_key("total_returned"));
        assert!(metrics_map.contains_key("active_objects"));
        assert_eq!(metrics_map.get("total_retrieved").unwrap(), "1");
        assert_eq!(metrics_map.get("total_returned").unwrap(), "1");
    }
    
    #[test]
    fn test_health_warnings() {
        let config = PoolConfiguration::new()
            .with_max_pool_size(2);
        
        let pool = ObjectPool::new(vec![1, 2], config);
        
        // Use both objects to get high utilization
        let _obj1 = pool.get_object().unwrap();
        let _obj2 = pool.get_object().unwrap();
        
        let health = pool.get_health_status();
        assert_eq!(health.utilization, 1.0); // 100% utilization
    }
    
    #[test]
    fn test_configuration_builder() {
        let config = PoolConfiguration::<i32>::new()
            .with_max_pool_size(50)
            .with_max_active_objects(25)
            .with_timeout(Duration::from_secs(10))
            .with_ttl(Duration::from_secs(300))
            .with_idle_timeout(Duration::from_secs(60))
            .with_warmup(10)
            .with_circuit_breaker(5, Duration::from_secs(30));
        
        assert_eq!(config.max_pool_size, 50);
        assert_eq!(config.max_active_objects, Some(25));
        assert_eq!(config.warmup_size, Some(10));
        assert!(config.enable_circuit_breaker);
    }
}
