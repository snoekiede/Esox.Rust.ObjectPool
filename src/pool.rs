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
pub struct PooledObject<T> {
    value: Option<T>,
    object_id: usize,
    return_fn: Arc<dyn Fn(T, usize) + Send + Sync>,
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
        if let Some(ref cb) = self.circuit_breaker {
            if !cb.allow_request() {
                return Err(PoolError::CircuitBreakerOpen);
            }
        }
        Ok(())
    }
    
    fn check_max_active(&self) -> PoolResult<()> {
        if let Some(max) = self.config.max_active_objects {
            if self.active.len() >= max {
                return Err(PoolError::MaxActiveObjectsReached);
            }
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
            if config.validate_on_return {
                if let Some(validate) = config.validation_function {
                    if !validate(&obj) {
                        metrics.validation_failures.fetch_add(1, Ordering::Relaxed);
                        active.remove(&id);
                        eviction.remove_object(id);
                        return;
                    }
                }
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
}
