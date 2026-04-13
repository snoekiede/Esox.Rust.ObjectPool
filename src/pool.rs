//! Core object pool implementations

use crate::config::PoolConfiguration;
use crate::errors::{PoolError, PoolResult};
use crate::health::{HealthStatus, HealthTracker};
use crate::metrics::{MetricsExporter, MetricsTracker, PoolMetrics};
use crate::eviction::{EvictionPolicy, EvictionTracker};
use crate::circuit_breaker::{CircuitBreaker, CircuitBreakerState};

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
/// use esox_objectpool::{ObjectPool, PoolConfiguration};
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
    detach_fn: Arc<dyn Fn(usize) + Send + Sync>,
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
    fn new(
        value: T,
        object_id: usize,
        return_fn: Arc<dyn Fn(T, usize) + Send + Sync>,
        detach_fn: Arc<dyn Fn(usize) + Send + Sync>,
    ) -> Self {
        Self {
            value: Some(value),
            object_id,
            return_fn,
            detach_fn,
        }
    }
    
    /// Get the inner value without returning to pool
    pub fn unwrap(mut self) -> T {
        (self.detach_fn)(self.object_id);
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
/// use esox_objectpool::{ObjectPool, PoolConfiguration};
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
    const PUSH_RETRY_LIMIT: usize = 8;

    /// Create a new object pool with initial objects
    ///
    /// # Examples
    ///
    /// ```
    /// use esox_objectpool::{ObjectPool, PoolConfiguration};
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
    
    /// Get an object from the pool (non-blocking)
    ///
    /// Returns immediately. If no object is available, this returns
    /// `PoolError::PoolEmpty`.
    ///
    /// Also returns an error if the circuit breaker is open or max-active
    /// limits are reached.
    ///
    /// # Examples
    ///
    /// ```
    /// use esox_objectpool::{ObjectPool, PoolConfiguration};
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
                    let detach_fn = self.make_detach_fn();
                    return Ok(PooledObject::new(obj, id, return_fn, detach_fn));
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
    
    /// Try to get an object without throwing an error for an empty pool
    ///
    /// Returns `Ok(None)` if pool is empty.
    ///
    /// Operational failures (for example circuit breaker or max-active limits)
    /// are returned as errors.
    ///
    /// # Examples
    ///
    /// ```
    /// use esox_objectpool::{ObjectPool, PoolConfiguration};
    ///
    /// let pool = ObjectPool::new(vec![1], PoolConfiguration::default());
    /// 
    /// let obj1 = pool.try_get_object().unwrap();
    /// assert!(obj1.is_some());
    /// 
    /// let obj2 = pool.try_get_object().unwrap();
    /// assert!(obj2.is_none()); // Pool empty
    /// ```
    pub fn try_get_object(&self) -> PoolResult<Option<PooledObject<T>>> {
        match self.get_object() {
            Ok(obj) => Ok(Some(obj)),
            Err(PoolError::PoolEmpty) => Ok(None),
            Err(err) => Err(err),
        }
    }
    
    /// Get an object asynchronously with timeout
    pub async fn get_object_async(&self) -> PoolResult<PooledObject<T>> {
        let timeout = self.config.operation_timeout.unwrap_or(Duration::from_secs(30));
        
        tokio::time::timeout(timeout, async {
            loop {
                match self.try_get_object() {
                    Ok(Some(obj)) => return Ok(obj),
                    Ok(None) => {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    Err(err) => return Err(err),
                }
            }
        })
        .await
        .map_err(|_| PoolError::Timeout(timeout))?
    }
    
    /// Try to get an object asynchronously
    pub async fn try_get_object_async(&self) -> PoolResult<Option<PooledObject<T>>> {
        self.try_get_object()
    }
    
    /// Get health status
    pub fn get_health_status(&self) -> HealthStatus {
        let available = self.available.len();
        let active = self.active.len();
        let cb_open = self
            .circuit_breaker
            .as_ref()
            .map(|cb| matches!(cb.state(), CircuitBreakerState::Open))
            .unwrap_or(false);
        HealthStatus::new(available, active, self.capacity, cb_open)
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

    /// Proactively remove all expired objects from the available queue.
    ///
    /// Returns the number of objects evicted. Call this periodically (e.g. from a
    /// `tokio::spawn` background task) when using TTL or idle-timeout eviction,
    /// because expiry is otherwise only enforced lazily on `get_object()`.
    pub fn evict_expired(&self) -> usize {
        let mut evicted = 0;
        let mut keep = Vec::new();

        while let Some((obj, id)) = self.available.pop() {
            if self.eviction.is_expired(id) {
                self.eviction.remove_object(id);
                evicted += 1;
            } else {
                keep.push((obj, id));
            }
        }

        for item in keep {
            if Self::push_available_with_retry(&self.available, item).is_err() {
                // Queue is unexpectedly full (concurrent returns filled it while we
                // were scanning). Count the object as lost and update the metric.
                self.metrics.queue_push_failures.fetch_add(1, Ordering::Relaxed);
                evicted += 1;
            }
        }

        evicted
    }

    /// Drain all *available* (not currently checked-out) objects from the pool
    /// and return them. Active objects are unaffected.
    ///
    /// Useful for graceful shutdown: drain the pool, then wait for active
    /// `PooledObject`s to be dropped (which will attempt to return to an empty
    /// queue; those objects are silently discarded).
    pub fn drain(&self) -> Vec<T> {
        let mut objects = Vec::new();
        while let Some((obj, id)) = self.available.pop() {
            self.eviction.remove_object(id);
            objects.push(obj);
        }
        objects
    }

    /// Record a circuit-breaker success from an external caller (used by
    /// `DynamicObjectPool` to offset the failure recorded when the inner queue
    /// was empty but the request was ultimately served via dynamic creation).
    pub(crate) fn record_circuit_breaker_success(&self) {
        if let Some(ref cb) = self.circuit_breaker {
            cb.record_success();
        }
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
            match ObjectPool::<T>::push_available_with_retry(available.as_ref(), (obj, id)) {
                Ok(()) => {
                    metrics.total_returned.fetch_add(1, Ordering::Relaxed);
                    health.increment_returned();
                }
                Err((_obj, failed_id)) => {
                    metrics.queue_push_failures.fetch_add(1, Ordering::Relaxed);
                    eviction.remove_object(failed_id);
                }
            }
        })
    }

    fn make_detach_fn(&self) -> Arc<dyn Fn(usize) + Send + Sync> {
        let active = Arc::clone(&self.active);
        let eviction = Arc::clone(&self.eviction);

        Arc::new(move |id| {
            active.remove(&id);
            eviction.remove_object(id);
        })
    }

    fn push_available_with_retry(
        available: &ArrayQueue<(T, usize)>,
        mut item: (T, usize),
    ) -> Result<(), (T, usize)> {
        for _ in 0..Self::PUSH_RETRY_LIMIT {
            match available.push(item) {
                Ok(()) => return Ok(()),
                Err(returned_item) => {
                    item = returned_item;
                    std::thread::yield_now();
                }
            }
        }

        Err(item)
    }
}

/// Queryable object pool - find objects matching a predicate
///
/// # Examples
///
/// ```
/// use esox_objectpool::{QueryableObjectPool, PoolConfiguration};
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
            if let Err((_obj, failed_id)) = ObjectPool::<T>::push_available_with_retry(
                self.inner.available.as_ref(),
                item,
            ) {
                self.inner.metrics.queue_push_failures.fetch_add(1, Ordering::Relaxed);
                self.inner.eviction.remove_object(failed_id);
            }
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
            let detach_fn = self.inner.make_detach_fn();
            Ok(PooledObject::new(obj, id, return_fn, detach_fn))
        } else {
            if let Some(ref cb) = self.inner.circuit_breaker {
                cb.record_failure();
            }
            Err(PoolError::NoMatchFound)
        }
    }
    
    /// Try to get an object matching query
    pub fn try_get_object<F>(&self, query: F) -> PoolResult<Option<PooledObject<T>>>
    where
        F: Fn(&T) -> bool,
    {
        match self.get_object(query) {
            Ok(obj) => Ok(Some(obj)),
            Err(PoolError::NoMatchFound) => Ok(None),
            Err(err) => Err(err),
        }
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
                    Ok(Some(obj)) => return Ok(obj),
                    Ok(None) => {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    Err(err) => return Err(err),
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

    pub fn available_count(&self) -> usize {
        self.inner.available_count()
    }

    pub fn active_count(&self) -> usize {
        self.inner.active_count()
    }

    /// Proactively remove expired objects. See [`ObjectPool::evict_expired`].
    pub fn evict_expired(&self) -> usize {
        self.inner.evict_expired()
    }

    /// Drain all available objects. See [`ObjectPool::drain`].
    pub fn drain(&self) -> Vec<T> {
        self.inner.drain()
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
/// use esox_objectpool::{DynamicObjectPool, PoolConfiguration};
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
    /// Serialises dynamic object creation to prevent TOCTOU over-creation.
    create_lock: std::sync::Mutex<()>,
}

impl<T: Send + Sync + 'static> DynamicObjectPool<T> {
    /// Create a new dynamic pool with factory function
    pub fn new<F>(factory: F, config: PoolConfiguration<T>) -> Self
    where
        F: Fn() -> T + Send + Sync + 'static,
    {
        Self {
            inner: ObjectPool::new(Vec::new(), config),
            factory: Arc::new(factory),
            create_lock: std::sync::Mutex::new(()),
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
            create_lock: std::sync::Mutex::new(()),
        }
    }
    
    /// Get an object, creating one via the factory if the pool is empty.
    ///
    /// Dynamic creation only proceeds when the pool is empty **and** the total
    /// live object count (active + available) is below `max_pool_size`.
    /// `CircuitBreakerOpen` and `MaxActiveObjectsReached` are propagated
    /// immediately — the factory is **never** called in those cases.
    ///
    /// A `Mutex` serialises the capacity check + creation step, preventing the
    /// TOCTOU race where two concurrent callers both see room and both create an
    /// object, exceeding the configured capacity.
    pub fn get_object(&self) -> PoolResult<PooledObject<T>> {
        match self.inner.get_object() {
            Ok(obj) => Ok(obj),
            Err(PoolError::PoolEmpty) => {
                // Serialise capacity check + creation to prevent TOCTOU race.
                let _guard = self.create_lock.lock().unwrap_or_else(|p| p.into_inner());

                // Re-check under the lock: a concurrent thread may have returned
                // an object or hit capacity between the PoolEmpty error and here.
                let total_live = self.inner.active.len() + self.inner.available.len();
                if total_live >= self.inner.capacity {
                    return Err(PoolError::PoolFull);
                }

                let obj = (self.factory)();
                let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);

                self.inner.eviction.track_object(id);
                self.inner.active.insert(id, ());
                self.inner.metrics.total_retrieved.fetch_add(1, Ordering::Relaxed);
                self.inner.health.increment_retrieved();

                // The inner `get_object()` recorded a CB failure for the empty
                // queue. Since we successfully served the request, offset it with
                // a success so routine dynamic creation doesn't trip the breaker.
                self.inner.record_circuit_breaker_success();

                let return_fn = self.inner.make_return_fn();
                let detach_fn = self.inner.make_detach_fn();
                Ok(PooledObject::new(obj, id, return_fn, detach_fn))
            }
            Err(err) => Err(err),
        }
    }
    
    /// Try to get an object
    pub fn try_get_object(&self) -> PoolResult<Option<PooledObject<T>>> {
        match self.get_object() {
            Ok(obj) => Ok(Some(obj)),
            Err(PoolError::PoolFull) => Ok(None),
            Err(err) => Err(err),
        }
    }
    
    /// Get an object asynchronously
    pub async fn get_object_async(&self) -> PoolResult<PooledObject<T>> {
        let timeout = self.inner.config.operation_timeout.unwrap_or(Duration::from_secs(30));
        
        tokio::time::timeout(timeout, async {
            loop {
                match self.try_get_object() {
                    Ok(Some(obj)) => return Ok(obj),
                    Ok(None) => {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    Err(err) => return Err(err),
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
    /// use esox_objectpool::{DynamicObjectPool, PoolConfiguration};
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

    pub fn available_count(&self) -> usize {
        self.inner.available_count()
    }

    pub fn active_count(&self) -> usize {
        self.inner.active_count()
    }

    /// Proactively remove expired objects. See [`ObjectPool::evict_expired`].
    pub fn evict_expired(&self) -> usize {
        self.inner.evict_expired()
    }

    /// Drain all available objects. See [`ObjectPool::drain`].
    pub fn drain(&self) -> Vec<T> {
        self.inner.drain()
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
        
        let obj1 = pool.try_get_object().unwrap();
        assert!(obj1.is_some());
        if let Some(ref obj) = obj1 {
            assert_eq!(**obj, 42);
        }
        
        let obj2 = pool.try_get_object().unwrap();
        assert!(obj2.is_none());
        
        drop(obj1);
        
        let obj3 = pool.try_get_object().unwrap();
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
    fn test_dynamic_pool_propagates_max_active_without_creating() {
        let created = Arc::new(AtomicUsize::new(0));
        let created_clone = Arc::clone(&created);

        let pool = DynamicObjectPool::with_initial(
            move || {
                created_clone.fetch_add(1, Ordering::Relaxed);
                42
            },
            vec![1, 2],
            PoolConfiguration::new().with_max_active_objects(1),
        );

        let _obj = pool.get_object().unwrap();
        let result = pool.get_object();

        assert!(matches!(result, Err(PoolError::MaxActiveObjectsReached)));
        assert_eq!(created.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_dynamic_pool_propagates_circuit_breaker_without_extra_creation() {
        let created = Arc::new(AtomicUsize::new(0));
        let created_clone = Arc::clone(&created);

        let pool = DynamicObjectPool::new(
            move || {
                created_clone.fetch_add(1, Ordering::Relaxed);
                42
            },
            PoolConfiguration::new()
                .with_max_pool_size(2)
                .with_circuit_breaker(1, Duration::from_secs(60)),
        );

        // First call creates because the inner pool is empty.
        let _obj = pool.get_object().unwrap();
        assert_eq!(created.load(Ordering::Relaxed), 1);

        // The first empty inner read opened the breaker, so this must fail fast.
        let result = pool.get_object();
        assert!(matches!(result, Err(PoolError::CircuitBreakerOpen)));
        assert_eq!(created.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_eviction_ttl() {
        use std::thread;
        
        let config = PoolConfiguration::new()
            .with_ttl(Duration::from_millis(100));
        
        let pool = ObjectPool::new(vec![1, 2, 3], config);
        
        thread::sleep(Duration::from_millis(150));
        
        // Objects should be expired and filtered out
        let obj = pool.try_get_object().unwrap();
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
    async fn test_async_get_fails_fast_on_max_active() {
        use std::time::Instant;

        let config = PoolConfiguration::new()
            .with_timeout(Duration::from_secs(2))
            .with_max_active_objects(1);

        let pool = ObjectPool::new(vec![1, 2], config);
        let _obj = pool.get_object().unwrap();

        let start = Instant::now();
        let result = pool.get_object_async().await;
        let elapsed = start.elapsed();

        assert!(matches!(result, Err(PoolError::MaxActiveObjectsReached)));
        assert!(elapsed < Duration::from_millis(200));
    }

    #[tokio::test]
    async fn test_async_get_fails_fast_on_circuit_breaker_open() {
        use std::time::Instant;

        let config = PoolConfiguration::new()
            .with_timeout(Duration::from_secs(2))
            .with_circuit_breaker(1, Duration::from_secs(60));

        let pool = ObjectPool::new(vec![1], config);
        let _obj = pool.get_object().unwrap();

        // First empty attempt records failure and opens the breaker.
        assert!(matches!(pool.try_get_object(), Ok(None)));

        let start = Instant::now();
        let result = pool.get_object_async().await;
        let elapsed = start.elapsed();

        assert!(matches!(result, Err(PoolError::CircuitBreakerOpen)));
        assert!(elapsed < Duration::from_millis(200));
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
                if let Ok(Some(obj)) = pool_clone.try_get_object() {
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
        assert!(metrics_map.contains_key("queue_push_failures"));
        assert_eq!(metrics_map.get("total_retrieved").unwrap(), "1");
        assert_eq!(metrics_map.get("total_returned").unwrap(), "1");
    }

    #[test]
    fn test_return_push_failure_is_tracked() {
        let config = PoolConfiguration::new().with_max_pool_size(1);
        let pool = ObjectPool::new(vec![1], config);

        let obj = pool.get_object().unwrap();

        // Fill the queue while the checked-out object is still active.
        pool.available.push((2, 999)).unwrap();

        drop(obj);

        let metrics = pool.get_metrics();
        assert_eq!(metrics.queue_push_failures, 1);
        assert_eq!(metrics.total_returned, 0);
        assert_eq!(pool.active_count(), 0);
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

    #[test]
    fn test_unwrap_cleans_active_state() {
        let pool = ObjectPool::new(
            vec![1],
            PoolConfiguration::new().with_max_active_objects(1),
        );

        let obj = pool.get_object().unwrap();
        assert_eq!(pool.active_count(), 1);

        let value = obj.unwrap();
        assert_eq!(value, 1);

        assert_eq!(pool.active_count(), 0);
        assert_eq!(pool.available_count(), 0);

        // Active slot is released, so next error should be pool-empty, not max-active.
        let result = pool.get_object();
        assert!(matches!(result, Err(PoolError::PoolEmpty)));
    }

    #[test]
    fn test_unwrap_does_not_increment_returned_metrics() {
        let pool = ObjectPool::new(vec![1], PoolConfiguration::default());

        let obj = pool.get_object().unwrap();
        let _value = obj.unwrap();

        let metrics = pool.get_metrics();
        assert_eq!(metrics.total_retrieved, 1);
        assert_eq!(metrics.total_returned, 0);
        assert_eq!(metrics.active_objects, 0);
        assert_eq!(metrics.available_objects, 0);
    }

    #[test]
    fn test_try_get_object_propagates_max_active_error() {
        let pool = ObjectPool::new(
            vec![1, 2],
            PoolConfiguration::new().with_max_active_objects(1),
        );

        let _obj = pool.get_object().unwrap();

        let result = pool.try_get_object();
        assert!(matches!(result, Err(PoolError::MaxActiveObjectsReached)));
    }

    #[test]
    fn test_try_get_object_propagates_circuit_breaker_error() {
        let pool = ObjectPool::new(
            vec![1],
            PoolConfiguration::new().with_circuit_breaker(1, Duration::from_secs(60)),
        );

        let _obj = pool.get_object().unwrap();

        // First empty attempt records a failure and opens the breaker.
        let first = pool.try_get_object();
        assert!(matches!(first, Ok(None)));

        let second = pool.try_get_object();
        assert!(matches!(second, Err(PoolError::CircuitBreakerOpen)));
    }

    // ── DynamicObjectPool: circuit breaker does not trip on successful dynamic creation ──

    #[test]
    fn test_dynamic_pool_cb_does_not_trip_on_routine_creation() {
        // Threshold = 3: the CB must NOT open when every empty-pool event is
        // immediately followed by a successful dynamic creation.
        let pool = DynamicObjectPool::new(
            || 42,
            PoolConfiguration::new()
                .with_max_pool_size(10)
                .with_circuit_breaker(3, Duration::from_secs(60)),
        );

        // Create 4 objects (all via dynamic creation); the CB should remain closed.
        let _a = pool.get_object().unwrap();
        let _b = pool.get_object().unwrap();
        let _c = pool.get_object().unwrap();
        let _d = pool.get_object().unwrap();

        let health = pool.get_health_status();
        assert!(!health.circuit_breaker_open, "CB must stay closed during normal dynamic creation");
    }

    // ── evict_expired ──────────────────────────────────────────────────────────────────

    #[test]
    fn test_evict_expired_removes_stale_objects() {
        use std::thread;

        let config = PoolConfiguration::new()
            .with_ttl(Duration::from_millis(50));

        let pool = ObjectPool::new(vec![1, 2, 3], config);

        thread::sleep(Duration::from_millis(80));

        let evicted = pool.evict_expired();
        assert_eq!(evicted, 3, "all three objects should have been evicted");
        assert_eq!(pool.available_count(), 0);
    }

    #[test]
    fn test_evict_expired_keeps_fresh_objects() {
        let config = PoolConfiguration::new()
            .with_ttl(Duration::from_secs(300));

        let pool = ObjectPool::new(vec![1, 2, 3], config);

        let evicted = pool.evict_expired();
        assert_eq!(evicted, 0);
        assert_eq!(pool.available_count(), 3);
    }

    // ── drain ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_drain_returns_available_objects() {
        let pool = ObjectPool::new(vec![1, 2, 3], PoolConfiguration::default());

        let drained = pool.drain();
        assert_eq!(drained.len(), 3);
        assert_eq!(pool.available_count(), 0);
    }

    #[test]
    fn test_drain_does_not_affect_active_objects() {
        let pool = ObjectPool::new(vec![1, 2, 3], PoolConfiguration::default());

        let _obj = pool.get_object().unwrap(); // 1 active
        let drained = pool.drain();
        assert_eq!(drained.len(), 2); // only the 2 available ones
        assert_eq!(pool.active_count(), 1);
    }

    // ── HealthStatus includes circuit-breaker state ───────────────────────────────────

    #[test]
    fn test_health_status_includes_cb_state() {
        let pool = ObjectPool::new(
            vec![1],
            PoolConfiguration::new().with_circuit_breaker(1, Duration::from_secs(60)),
        );

        // Before the breaker opens the pool should be healthy.
        assert!(!pool.get_health_status().circuit_breaker_open);

        let _obj = pool.get_object().unwrap();
        let _ = pool.try_get_object(); // opens the breaker (threshold = 1)

        let health = pool.get_health_status();
        assert!(health.circuit_breaker_open);
        assert!(!health.is_healthy);
        assert!(health.warnings.iter().any(|w| w.contains("Circuit breaker")));
    }

    // ── DynamicObjectPool: observable counts and eviction ────────────────────────────

    #[test]
    fn test_dynamic_pool_available_and_active_counts() {
        let pool = DynamicObjectPool::new(
            || 0u8,
            PoolConfiguration::new().with_max_pool_size(5),
        );

        let _a = pool.get_object().unwrap();
        let _b = pool.get_object().unwrap();

        assert_eq!(pool.active_count(), 2);

        drop(_a);
        assert_eq!(pool.available_count(), 1);
    }
}
