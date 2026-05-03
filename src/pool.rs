//! Core object pool implementations

use crate::config::PoolConfiguration;
use crate::errors::{PoolError, PoolResult};
use crate::health::HealthStatus;
use crate::metrics::{MetricsExporter, MetricsTracker, PoolMetrics};
use crate::eviction::{EvictionPolicy, EvictionTracker};
use crate::circuit_breaker::{CircuitBreaker, CircuitBreakerState};

use crossbeam::queue::ArrayQueue;
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
    
    /// Permanently remove the inner value from the pool and take ownership.
    ///
    /// The object is **not** returned to the pool. Pool capacity is permanently
    /// reduced by one. Use this only when you explicitly need to own `T` beyond
    /// the pool's lifetime.
    ///
    /// # Examples
    ///
    /// ```
    /// use esox_objectpool::{ObjectPool, PoolConfiguration};
    ///
    /// let pool = ObjectPool::new(vec![1], PoolConfiguration::default());
    /// let obj = pool.get_object().unwrap();
    ///
    /// // Permanently take the value out of the pool.
    /// let value = obj.into_detached();
    /// assert_eq!(value, 1);
    /// assert_eq!(pool.available_count(), 0); // capacity is gone
    /// ```
    pub fn into_detached(mut self) -> T {
        (self.detach_fn)(self.object_id);
        self.value.take().expect("Value already taken")
    }

    /// Get the inner value without returning to pool.
    ///
    /// # Deprecation
    ///
    /// This method permanently removes the object from the pool (it is **not**
    /// returned when the `PooledObject` is dropped), which silently reduces pool
    /// capacity. Use [`PooledObject::into_detached`] instead to make the intent
    /// explicit.
    #[deprecated(since = "1.1.0", note = "Use `into_detached()` to make permanent pool removal explicit")]
    pub fn unwrap(self) -> T {
        self.into_detached()
    }

    /// Borrow the inner value without affecting pool state.
    ///
    /// The object is **not** removed from the pool. When the `PooledObject` is
    /// dropped it will still be returned to the pool as normal.
    ///
    /// This is identical to `&**obj` via [`Deref`], but provides an explicit,
    /// readable call-site alternative.
    ///
    /// # Examples
    ///
    /// ```
    /// use esox_objectpool::{ObjectPool, PoolConfiguration};
    ///
    /// let pool = ObjectPool::new(vec![42], PoolConfiguration::default());
    /// let obj = pool.get_object().unwrap();
    ///
    /// // Inspect the value — obj is still in the pool when dropped.
    /// assert_eq!(*obj.get(), 42);
    /// drop(obj);
    /// assert_eq!(pool.available_count(), 1); // returned as normal
    /// ```
    pub fn get(&self) -> &T {
        self.value.as_ref().expect("Value already taken")
    }

    /// Mutably borrow the inner value without affecting pool state.
    ///
    /// The object is **not** removed from the pool. When the `PooledObject` is
    /// dropped it will still be returned to the pool as normal.
    ///
    /// This is identical to `&mut **obj` via [`DerefMut`], but provides an
    /// explicit, readable call-site alternative.
    ///
    /// # Examples
    ///
    /// ```
    /// use esox_objectpool::{ObjectPool, PoolConfiguration};
    ///
    /// let pool = ObjectPool::new(vec![0], PoolConfiguration::default());
    /// let mut obj = pool.get_object().unwrap();
    ///
    /// *obj.get_mut() = 99;
    /// assert_eq!(*obj.get(), 99);
    /// drop(obj);
    /// assert_eq!(pool.available_count(), 1); // returned as normal
    /// ```
    pub fn get_mut(&mut self) -> &mut T {
        self.value.as_mut().expect("Value already taken")
    }
}

impl<T> AsRef<T> for PooledObject<T> {
    fn as_ref(&self) -> &T {
        self.get()
    }
}

impl<T> AsMut<T> for PooledObject<T> {
    fn as_mut(&mut self) -> &mut T {
        self.get_mut()
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
    /// Number of objects currently checked out. Also acts as a CAS semaphore
    /// for `max_active_objects` enforcement so the check+increment is atomic.
    active_count: Arc<AtomicUsize>,
    config: Arc<PoolConfiguration<T>>,
    metrics: Arc<MetricsTracker>,
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
        assert!(capacity > 0, "ObjectPool capacity must be at least 1");
        let available = Arc::new(ArrayQueue::new(capacity));
        
        let eviction_policy = if let Some(ttl) = config.time_to_live {
            if let Some(idle) = config.idle_timeout {
                EvictionPolicy::Combined { ttl, idle_timeout: idle }
            } else {
                EvictionPolicy::TimeToLive(ttl)
            }
        } else if let Some(idle) = config.idle_timeout {
            EvictionPolicy::IdleTimeout(idle)
        } else {
            EvictionPolicy::None
        };
        
        let eviction = Arc::new(EvictionTracker::new(eviction_policy));
        
        // Add objects to pool; queue is sized to fit all of them, so push cannot fail.
        for (idx, obj) in objects.into_iter().enumerate() {
            eviction.track_object(idx);
            // Queue is sized to fit all objects; push can only fail if the queue is full,
            // which is impossible here.
            available.push((obj, idx)).unwrap_or_else(|_| {
                panic!("BUG: ObjectPool queue full during construction for object #{idx}")
            });
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
            active_count: Arc::new(AtomicUsize::new(0)),
            config: Arc::new(config),
            metrics: Arc::new(MetricsTracker::new()),
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
    #[must_use = "the pool object must be used or explicitly dropped"]
    pub fn get_object(&self) -> PoolResult<PooledObject<T>> {
        self.check_circuit_breaker()?;
        // Atomically reserve an active slot (enforces max_active_objects without a TOCTOU race).
        self.try_acquire_active_slot()?;

        // Try to get available object
        loop {
            match self.available.pop() {
                Some((obj, id)) => {
                    // Check if expired
                    if self.eviction.is_expired(id) {
                        self.eviction.remove_object(id);
                        continue;
                    }
                    
                    self.eviction.touch_object(id);
                    self.metrics.total_retrieved.fetch_add(1, Ordering::Relaxed);

                    if let Some(ref cb) = self.circuit_breaker {
                        cb.record_success();
                    }
                    
                    let return_fn = self.make_return_fn();
                    let detach_fn = self.make_detach_fn();
                    return Ok(PooledObject::new(obj, id, return_fn, detach_fn));
                }
                None => {
                    // Release the slot we reserved — no object was obtained.
                    self.active_count.fetch_sub(1, Ordering::AcqRel);
                    self.metrics.pool_empty_events.fetch_add(1, Ordering::Relaxed);

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
    #[must_use = "check Ok(None) to detect empty pool"]
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
            let mut attempt: u64 = 0;
            loop {
                match self.try_get_object() {
                    Ok(Some(obj)) => return Ok(obj),
                    Ok(None) => {
                        // Small jitter (5–20 ms) avoids a thundering-herd wake-up.
                        let delay = 5 + (attempt % 4) * 5;
                        tokio::time::sleep(Duration::from_millis(delay)).await;
                        attempt = attempt.wrapping_add(1);
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
    #[must_use]
    pub fn get_health_status(&self) -> HealthStatus {
        let available = self.available.len();
        let active = self.active_count.load(Ordering::Relaxed);
        let cb_open = self
            .circuit_breaker
            .as_ref()
            .map(|cb| matches!(cb.state(), CircuitBreakerState::Open))
            .unwrap_or(false);
        HealthStatus::new(available, active, self.capacity, cb_open)
    }
    
    /// Export metrics as a key-value map
    #[must_use]
    pub fn export_metrics(&self) -> HashMap<String, String> {
        let metrics = self.get_metrics();
        metrics.export()
    }
    
    /// Export metrics in Prometheus format
    #[must_use]
    pub fn export_metrics_prometheus(
        &self,
        pool_name: &str,
        tags: Option<&HashMap<String, String>>,
    ) -> String {
        let metrics = self.get_metrics();
        MetricsExporter::export_prometheus(&metrics, pool_name, tags)
    }
    
    /// Get pool metrics
    #[must_use]
    pub fn get_metrics(&self) -> PoolMetrics {
        self.metrics.get_metrics(
            self.active_count.load(Ordering::Relaxed),
            self.available.len(),
            self.capacity,
        )
    }
    
    /// Number of objects currently available in the queue
    #[must_use]
    pub fn available_count(&self) -> usize {
        self.available.len()
    }

    /// Number of objects currently checked out
    #[must_use]
    pub fn active_count(&self) -> usize {
        self.active_count.load(Ordering::Relaxed)
    }

    /// Maximum number of objects this pool can hold (set at construction time)
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Proactively remove all expired objects from the available queue.
    ///
    /// Returns the number of objects evicted. Call this periodically (e.g. from a
    /// `tokio::spawn` background task) when using TTL or idle-timeout eviction,
    /// because expiry is otherwise only enforced lazily on `get_object()`.
    #[must_use = "returns the count of evicted objects"]
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
                // Queue unexpectedly full (concurrent returns filled it while we
                // were scanning). Track this as a push failure — NOT as an eviction.
                self.metrics.queue_push_failures.fetch_add(1, Ordering::Relaxed);
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
    #[must_use = "returns the drained objects"]
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

    /// Atomically reserve an active slot.
    ///
    /// When `max_active_objects` is set this uses a CAS loop so that the
    /// check-and-increment is a single atomic operation — eliminating the TOCTOU
    /// race that existed when `active.len() >= max` was checked separately from
    /// the subsequent increment.
    fn try_acquire_active_slot(&self) -> PoolResult<()> {
        match self.config.max_active_objects {
            Some(max) => {
                let mut current = self.active_count.load(Ordering::Acquire);
                loop {
                    if current >= max {
                        return Err(PoolError::MaxActiveObjectsReached);
                    }
                    match self.active_count.compare_exchange_weak(
                        current,
                        current + 1,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => return Ok(()),
                        Err(actual) => current = actual,
                    }
                }
            }
            None => {
                self.active_count.fetch_add(1, Ordering::AcqRel);
                Ok(())
            }
        }
    }
    
    fn make_return_fn(&self) -> Arc<dyn Fn(T, usize) + Send + Sync> {
        let available = Arc::clone(&self.available);
        let active_count = Arc::clone(&self.active_count);
        let metrics = Arc::clone(&self.metrics);
        let eviction = Arc::clone(&self.eviction);
        let config = Arc::clone(&self.config);
        
        Arc::new(move |obj, id| {
            // Validate if configured
            if config.validate_on_return
                && let Some(validate) = config.validation_function
                && !validate(&obj)
            {
                metrics.validation_failures.fetch_add(1, Ordering::Relaxed);
                active_count.fetch_sub(1, Ordering::AcqRel);
                eviction.remove_object(id);
                return;
            }
            
            eviction.touch_object(id);
            active_count.fetch_sub(1, Ordering::AcqRel);
            match ObjectPool::<T>::push_available_with_retry(available.as_ref(), (obj, id)) {
                Ok(()) => {
                    metrics.total_returned.fetch_add(1, Ordering::Relaxed);
                }
                Err((_obj, failed_id)) => {
                    metrics.queue_push_failures.fetch_add(1, Ordering::Relaxed);
                    eviction.remove_object(failed_id);
                }
            }
        })
    }

    fn make_detach_fn(&self) -> Arc<dyn Fn(usize) + Send + Sync> {
        let active_count = Arc::clone(&self.active_count);
        let eviction = Arc::clone(&self.eviction);
        let metrics = Arc::clone(&self.metrics);

        Arc::new(move |id| {
            active_count.fetch_sub(1, Ordering::AcqRel);
            eviction.remove_object(id);
            metrics.total_detached.fetch_add(1, Ordering::Relaxed);
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
    
    #[must_use = "the pool object must be used or explicitly dropped"]
    pub fn get_object<F>(&self, query: F) -> PoolResult<PooledObject<T>>
    where
        F: Fn(&T) -> bool,
    {
        self.inner.check_circuit_breaker()?;
        self.inner.try_acquire_active_slot()?;

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
            self.inner.eviction.touch_object(id);
            self.inner.metrics.total_retrieved.fetch_add(1, Ordering::Relaxed);

            if let Some(ref cb) = self.inner.circuit_breaker {
                cb.record_success();
            }
            
            let return_fn = self.inner.make_return_fn();
            let detach_fn = self.inner.make_detach_fn();
            Ok(PooledObject::new(obj, id, return_fn, detach_fn))
        } else {
            // Release the slot we reserved — no match was found.
            self.inner.active_count.fetch_sub(1, Ordering::AcqRel);
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
            let mut attempt: u64 = 0;
            loop {
                match self.try_get_object(&query) {
                    Ok(Some(obj)) => return Ok(obj),
                    Ok(None) => {
                        let delay = 5 + (attempt % 4) * 5;
                        tokio::time::sleep(Duration::from_millis(delay)).await;
                        attempt = attempt.wrapping_add(1);
                    }
                    Err(err) => return Err(err),
                }
            }
        })
        .await
        .map_err(|_| PoolError::Timeout(timeout))?
    }
    
    // Delegate methods to inner pool
    #[must_use]
    pub fn get_health_status(&self) -> HealthStatus {
        self.inner.get_health_status()
    }

    #[must_use]
    pub fn available_count(&self) -> usize {
        self.inner.available_count()
    }

    #[must_use]
    pub fn active_count(&self) -> usize {
        self.inner.active_count()
    }

    #[must_use]
    pub fn capacity(&self) -> usize {
        self.inner.capacity()
    }

    /// Proactively remove expired objects. See [`ObjectPool::evict_expired`].
    #[must_use = "returns the count of evicted objects"]
    pub fn evict_expired(&self) -> usize {
        self.inner.evict_expired()
    }

    /// Drain all available objects. See [`ObjectPool::drain`].
    #[must_use = "returns the drained objects"]
    pub fn drain(&self) -> Vec<T> {
        self.inner.drain()
    }

    #[must_use]
    pub fn get_metrics(&self) -> PoolMetrics {
        self.inner.get_metrics()
    }

    #[must_use]
    pub fn export_metrics(&self) -> HashMap<String, String> {
        self.inner.export_metrics()
    }

    #[must_use]
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
    #[must_use = "the pool object must be used or explicitly dropped"]
    pub fn get_object(&self) -> PoolResult<PooledObject<T>> {
        match self.inner.get_object() {
            Ok(obj) => Ok(obj),
            Err(PoolError::PoolEmpty) => {
                // Serialise capacity check + creation to prevent TOCTOU race.
                let _guard = self.create_lock.lock().unwrap_or_else(|p| p.into_inner());

                // Re-check under the lock: a concurrent thread may have returned
                // an object between the PoolEmpty error and here.
                let total_live = self.inner.active_count.load(Ordering::Acquire)
                    + self.inner.available.len();
                if total_live >= self.inner.capacity {
                    return Err(PoolError::PoolFull);
                }

                // Also enforce max_active_objects in the dynamic creation path.
                // Use the same CAS semaphore to remain race-free.
                self.inner.try_acquire_active_slot()?;

                let obj = (self.factory)();
                let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);

                self.inner.eviction.track_object(id);
                self.inner.metrics.total_retrieved.fetch_add(1, Ordering::Relaxed);

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
            let mut attempt: u64 = 0;
            loop {
                match self.try_get_object() {
                    Ok(Some(obj)) => return Ok(obj),
                    Ok(None) => {
                        let delay = 5 + (attempt % 4) * 5;
                        tokio::time::sleep(Duration::from_millis(delay)).await;
                        attempt = attempt.wrapping_add(1);
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
                // Queue is full; remove the eviction entry we just registered
                // to avoid a leak.
                self.inner.eviction.remove_object(id);
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
                    eviction.remove_object(id);
                    break;
                }
            }
        })
        .await
        .map_err(|_| PoolError::Cancelled)?;
        
        Ok(())
    }
    
    // Delegate methods
    #[must_use]
    pub fn get_health_status(&self) -> HealthStatus {
        self.inner.get_health_status()
    }

    #[must_use]
    pub fn available_count(&self) -> usize {
        self.inner.available_count()
    }

    #[must_use]
    pub fn active_count(&self) -> usize {
        self.inner.active_count()
    }

    #[must_use]
    pub fn capacity(&self) -> usize {
        self.inner.capacity()
    }

    /// Proactively remove expired objects. See [`ObjectPool::evict_expired`].
    #[must_use = "returns the count of evicted objects"]
    pub fn evict_expired(&self) -> usize {
        self.inner.evict_expired()
    }

    /// Drain all available objects. See [`ObjectPool::drain`].
    #[must_use = "returns the drained objects"]
    pub fn drain(&self) -> Vec<T> {
        self.inner.drain()
    }

    #[must_use]
    pub fn get_metrics(&self) -> PoolMetrics {
        self.inner.get_metrics()
    }

    #[must_use]
    pub fn export_metrics(&self) -> HashMap<String, String> {
        self.inner.export_metrics()
    }

    #[must_use]
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
    fn test_into_detached_cleans_active_state() {
        let pool = ObjectPool::new(
            vec![1],
            PoolConfiguration::new().with_max_active_objects(1),
        );

        let obj = pool.get_object().unwrap();
        assert_eq!(pool.active_count(), 1);

        let value = obj.into_detached();
        assert_eq!(value, 1);

        assert_eq!(pool.active_count(), 0);
        assert_eq!(pool.available_count(), 0);

        // Active slot is released, so next error should be pool-empty, not max-active.
        let result = pool.get_object();
        assert!(matches!(result, Err(PoolError::PoolEmpty)));
    }

    #[test]
    fn test_into_detached_does_not_increment_returned_metrics() {
        let pool = ObjectPool::new(vec![1], PoolConfiguration::default());

        let obj = pool.get_object().unwrap();
        let _value = obj.into_detached();

        let metrics = pool.get_metrics();
        assert_eq!(metrics.total_retrieved, 1);
        assert_eq!(metrics.total_returned, 0);
        assert_eq!(metrics.total_detached, 1);
        assert_eq!(metrics.active_objects, 0);
        assert_eq!(metrics.available_objects, 0);
    }

    #[test]
    fn test_get_borrows_without_removing_from_pool() {
        let pool = ObjectPool::new(vec![42], PoolConfiguration::default());
        let obj = pool.get_object().unwrap();

        assert_eq!(*obj.get(), 42);
        assert_eq!(pool.active_count(), 1);

        drop(obj);

        // Object must be returned normally.
        assert_eq!(pool.available_count(), 1);
        let metrics = pool.get_metrics();
        assert_eq!(metrics.total_returned, 1);
        assert_eq!(metrics.total_detached, 0);
    }

    #[test]
    fn test_get_mut_mutates_without_removing_from_pool() {
        let pool = ObjectPool::new(vec![0], PoolConfiguration::default());
        let mut obj = pool.get_object().unwrap();

        *obj.get_mut() = 99;
        assert_eq!(*obj.get(), 99);

        drop(obj);

        // Object must be returned normally.
        assert_eq!(pool.available_count(), 1);
        let metrics = pool.get_metrics();
        assert_eq!(metrics.total_returned, 1);
        assert_eq!(metrics.total_detached, 0);
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

    // ── Validation on return ──────────────────────────────────────────────────

    #[test]
    fn test_validation_failure_removes_object_and_increments_metric() {
        // Validation rejects any value ≤ 0; we mutate the object to -1 before
        // returning it so the validator fires.
        let config = PoolConfiguration::new().with_validation(|x: &i32| *x > 0);
        let pool = ObjectPool::new(vec![1, 2, 3], config);

        {
            let mut obj = pool.get_object().unwrap();
            *obj.get_mut() = -1; // will fail validation on drop
        }

        // One object was rejected, so only 2 remain.
        assert_eq!(pool.available_count(), 2);

        let metrics = pool.get_metrics();
        assert_eq!(metrics.validation_failures, 1);
        assert_eq!(metrics.total_returned, 0); // failed validation ≠ returned
    }

    #[test]
    fn test_validation_success_returns_object() {
        let config = PoolConfiguration::new().with_validation(|x: &i32| *x > 0);
        let pool = ObjectPool::new(vec![1, 2, 3], config);

        {
            let _obj = pool.get_object().unwrap(); // value is > 0, passes
        }

        assert_eq!(pool.available_count(), 3);
        let metrics = pool.get_metrics();
        assert_eq!(metrics.validation_failures, 0);
        assert_eq!(metrics.total_returned, 1);
    }

    // ── PooledObject Debug impl ───────────────────────────────────────────────

    #[test]
    fn test_pooled_object_debug_does_not_panic() {
        let pool = ObjectPool::new(vec![42i32], PoolConfiguration::default());
        let obj = pool.get_object().unwrap();
        let dbg = format!("{:?}", obj);
        assert!(dbg.contains("PooledObject"));
        assert!(dbg.contains("42"));
    }

    // ── DynamicObjectPool::with_initial returns objects to pool ──────────────

    #[test]
    fn test_dynamic_with_initial_returns_on_drop() {
        let pool = DynamicObjectPool::with_initial(
            || 99,
            vec![1, 2, 3],
            PoolConfiguration::new().with_max_pool_size(5),
        );

        assert_eq!(pool.available_count(), 3);
        {
            let _obj = pool.get_object().unwrap();
            assert_eq!(pool.active_count(), 1);
        }
        assert_eq!(pool.available_count(), 3);
    }

    // ── QueryableObjectPool::try_get_object error propagation ────────────────

    #[test]
    fn test_queryable_try_get_propagates_max_active_error() {
        let pool = QueryableObjectPool::new(
            vec![1, 2, 3],
            PoolConfiguration::new().with_max_active_objects(1),
        );

        let _obj = pool.get_object(|_| true).unwrap();
        let result = pool.try_get_object(|_| true);
        assert!(matches!(result, Err(PoolError::MaxActiveObjectsReached)));
    }

    #[test]
    fn test_queryable_try_get_returns_none_on_no_match() {
        let pool = QueryableObjectPool::new(vec![1, 2, 3], PoolConfiguration::default());
        let result = pool.try_get_object(|x| *x == 99);
        assert!(matches!(result, Ok(None)));
    }

    // ── DynamicObjectPool::try_get_object full → Ok(None) ────────────────────

    #[test]
    fn test_dynamic_try_get_returns_none_when_full() {
        let pool = DynamicObjectPool::new(
            || 0,
            PoolConfiguration::new().with_max_pool_size(1),
        );

        let _obj = pool.get_object().unwrap(); // fills capacity
        let result = pool.try_get_object();
        assert!(matches!(result, Ok(None)));
    }

    // ── Idle-timeout eviction ─────────────────────────────────────────────────

    #[test]
    fn test_idle_timeout_eviction() {
        use std::thread;

        let config = PoolConfiguration::new()
            .with_idle_timeout(Duration::from_millis(50));

        let pool = ObjectPool::new(vec![1, 2, 3], config);
        thread::sleep(Duration::from_millis(80));

        // On checkout the expired objects are skipped.
        let result = pool.try_get_object().unwrap();
        assert!(result.is_none() || pool.available_count() < 3);
    }

    // ── Combined TTL + idle-timeout eviction ──────────────────────────────────

    #[test]
    fn test_combined_ttl_idle_eviction_via_evict_expired() {
        use std::thread;

        let config = PoolConfiguration::new()
            .with_ttl(Duration::from_millis(50))
            .with_idle_timeout(Duration::from_secs(300)); // idle won't fire

        let pool = ObjectPool::new(vec![1, 2, 3], config);
        thread::sleep(Duration::from_millis(80));

        let evicted = pool.evict_expired();
        assert_eq!(evicted, 3);
        assert_eq!(pool.available_count(), 0);
    }

    // ── evict_expired / drain on delegating pool types ────────────────────────

    #[test]
    fn test_queryable_evict_expired_and_drain() {
        use std::thread;

        let config = PoolConfiguration::new()
            .with_ttl(Duration::from_millis(50));
        let pool = QueryableObjectPool::new(vec![1, 2, 3], config);
        thread::sleep(Duration::from_millis(80));

        let evicted = pool.evict_expired();
        assert_eq!(evicted, 3);
        assert_eq!(pool.available_count(), 0);

        // Drain on empty pool returns empty vec without panic.
        let drained = pool.drain();
        assert!(drained.is_empty());
    }

    #[test]
    fn test_dynamic_evict_and_drain() {
        use std::thread;

        let config = PoolConfiguration::new()
            .with_ttl(Duration::from_millis(50))
            .with_max_pool_size(5);
        let pool = DynamicObjectPool::with_initial(|| 0, vec![1, 2, 3], config);
        thread::sleep(Duration::from_millis(80));

        let evicted = pool.evict_expired();
        assert_eq!(evicted, 3);

        let drained = pool.drain();
        assert!(drained.is_empty());
    }

    // ── metrics export on delegating pool types ───────────────────────────────

    #[test]
    fn test_queryable_export_metrics() {
        let pool = QueryableObjectPool::new(vec![1, 2, 3], PoolConfiguration::default());
        let _ = pool.get_object(|_| true).unwrap();

        let map = pool.export_metrics();
        assert!(map.contains_key("total_retrieved"));
        assert_eq!(map.get("total_retrieved").unwrap(), "1");
    }

    #[test]
    fn test_dynamic_export_metrics() {
        let pool = DynamicObjectPool::new(|| 1, PoolConfiguration::new().with_max_pool_size(5));
        let _ = pool.get_object().unwrap();

        let map = pool.export_metrics();
        assert!(map.contains_key("total_retrieved"));
        assert_eq!(map.get("total_retrieved").unwrap(), "1");
    }

    // ── total_detached in metrics map and Prometheus ──────────────────────────

    #[test]
    fn test_export_metrics_includes_total_detached() {
        let pool = ObjectPool::new(vec![1, 2, 3], PoolConfiguration::default());

        let obj = pool.get_object().unwrap();
        let _ = obj.into_detached();

        let map = pool.export_metrics();
        assert!(map.contains_key("total_detached"));
        assert_eq!(map.get("total_detached").unwrap(), "1");
    }

    #[test]
    fn test_prometheus_export_includes_detached_counter() {
        let pool = ObjectPool::new(vec![1], PoolConfiguration::default());
        let obj = pool.get_object().unwrap();
        let _ = obj.into_detached();

        let prom = pool.export_metrics_prometheus("detach_pool", None);
        assert!(prom.contains("objectpool_objects_detached_total"));
        assert!(prom.contains("detach_pool"));
    }

    // ── warmup capped at capacity ─────────────────────────────────────────────

    #[test]
    fn test_warmup_capped_at_capacity() {
        let pool = DynamicObjectPool::new(
            || 0,
            PoolConfiguration::new().with_max_pool_size(3),
        );

        // Request more than capacity; only 3 should be created.
        pool.warmup(100).unwrap();
        assert_eq!(pool.available_count(), 3);
    }

    // ── concurrent DynamicObjectPool creation stays within capacity ──────────

    #[tokio::test]
    async fn test_dynamic_pool_concurrent_creation_does_not_exceed_capacity() {
        use std::sync::Arc;

        let pool = Arc::new(DynamicObjectPool::new(
            || 0u32,
            PoolConfiguration::new().with_max_pool_size(5),
        ));

        let mut handles = vec![];
        for _ in 0..20 {
            let p = Arc::clone(&pool);
            handles.push(tokio::spawn(async move { p.get_object() }));
        }

        let mut objects = vec![];
        for h in handles {
            if let Ok(Ok(obj)) = h.await {
                objects.push(obj);
            }
        }

        // Total live objects (active + available) must never exceed capacity=5.
        assert!(pool.active_count() + pool.available_count() <= 5);
        // All successfully obtained objects came from within capacity.
        assert!(objects.len() <= 5);
    }

    // ── QueryableObjectPool::get_object_async fails fast on errors ────────────

    #[tokio::test]
    async fn test_queryable_async_fails_fast_on_max_active() {
        use std::time::Instant;

        let pool = QueryableObjectPool::new(
            vec![1, 2],
            PoolConfiguration::new()
                .with_timeout(Duration::from_secs(2))
                .with_max_active_objects(1),
        );
        let _obj = pool.get_object(|_| true).unwrap();

        let start = Instant::now();
        let result = pool.get_object_async(|_| true).await;
        assert!(start.elapsed() < Duration::from_millis(200));
        assert!(matches!(result, Err(PoolError::MaxActiveObjectsReached)));
    }

    // ── DynamicObjectPool::get_object_async timeout ───────────────────────────

    #[tokio::test]
    async fn test_dynamic_async_timeout_when_full() {
        let pool = DynamicObjectPool::new(
            || 0,
            PoolConfiguration::new()
                .with_max_pool_size(1)
                .with_timeout(Duration::from_millis(60)),
        );

        let _obj = pool.get_object().unwrap(); // fills capacity

        let result = pool.get_object_async().await;
        assert!(matches!(result, Err(PoolError::Timeout(_))));
    }

    // ── New regression / feature tests ───────────────────────────────────────

    #[test]
    fn test_capacity_method() {
        let pool = ObjectPool::new(vec![1, 2, 3], PoolConfiguration::new().with_max_pool_size(10));
        assert_eq!(pool.capacity(), 10);

        let q = QueryableObjectPool::new(vec![1, 2], PoolConfiguration::default());
        assert_eq!(q.capacity(), 100); // default max_pool_size

        let d = DynamicObjectPool::new(|| 0, PoolConfiguration::new().with_max_pool_size(7));
        assert_eq!(d.capacity(), 7);
    }

    #[test]
    fn test_get_metrics_on_delegate_pools() {
        let q = QueryableObjectPool::new(vec![1, 2], PoolConfiguration::default());
        let _ = q.get_object(|_| true).unwrap();
        assert_eq!(q.get_metrics().total_retrieved, 1);

        let d = DynamicObjectPool::new(|| 0, PoolConfiguration::new().with_max_pool_size(5));
        let _ = d.get_object().unwrap();
        assert_eq!(d.get_metrics().total_retrieved, 1);
    }

    #[test]
    fn test_as_ref_as_mut_for_pooled_object() {
        let pool = ObjectPool::new(vec![42i32], PoolConfiguration::default());
        let mut obj = pool.get_object().unwrap();

        // AsRef
        let r: &i32 = obj.as_ref();
        assert_eq!(*r, 42);

        // AsMut
        *obj.as_mut() = 99;
        assert_eq!(*obj.as_ref(), 99);
    }

    #[test]
    #[should_panic(expected = "ObjectPool capacity must be at least 1")]
    fn test_zero_capacity_panics() {
        ObjectPool::new(vec![] as Vec<i32>, PoolConfiguration::new().with_max_pool_size(0));
    }

    #[tokio::test]
    async fn test_max_active_not_exceeded_under_concurrency() {
        use std::sync::Arc;

        let max = 3usize;
        let pool = Arc::new(ObjectPool::new(
            vec![0u32; 10],
            PoolConfiguration::new()
                .with_max_pool_size(10)
                .with_max_active_objects(max),
        ));
        let peak = Arc::new(AtomicUsize::new(0));

        let mut handles = vec![];
        for _ in 0..20 {
            let p = Arc::clone(&pool);
            let pk = Arc::clone(&peak);
            handles.push(tokio::spawn(async move {
                if let Ok(Some(_obj)) = p.try_get_object() {
                    let current = p.active_count();
                    let mut prev = pk.load(Ordering::Relaxed);
                    while current > prev {
                        match pk.compare_exchange_weak(prev, current, Ordering::Relaxed, Ordering::Relaxed) {
                            Ok(_) => break,
                            Err(v) => prev = v,
                        }
                    }
                }
            }));
        }
        for h in handles { h.await.unwrap(); }

        assert!(
            peak.load(Ordering::Relaxed) <= max,
            "active count peaked at {} > max {}",
            peak.load(Ordering::Relaxed),
            max
        );
    }
}


