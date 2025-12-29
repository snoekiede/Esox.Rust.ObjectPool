//! Pool configuration options

use std::time::Duration;

/// Configuration for object pool behavior
#[derive(Debug, Clone)]
pub struct PoolConfiguration<T> {
    /// Maximum number of objects that can exist in the pool
    pub max_pool_size: usize,
    
    /// Maximum number of objects that can be active (checked out) simultaneously
    pub max_active_objects: Option<usize>,
    
    /// Whether to validate objects when they are returned to the pool
    pub validate_on_return: bool,
    
    /// Custom validation function
    pub validation_function: Option<fn(&T) -> bool>,
    
    /// Timeout for async operations
    pub operation_timeout: Option<Duration>,
    
    /// Time-to-live for objects (eviction policy)
    pub time_to_live: Option<Duration>,
    
    /// Idle timeout for objects (eviction policy)
    pub idle_timeout: Option<Duration>,
    
    /// Whether to pre-populate the pool on creation
    pub warmup_size: Option<usize>,
    
    /// Enable circuit breaker protection
    pub enable_circuit_breaker: bool,
    
    /// Circuit breaker failure threshold
    pub circuit_breaker_threshold: usize,
    
    /// Circuit breaker reset timeout
    pub circuit_breaker_timeout: Duration,
}

impl<T> Default for PoolConfiguration<T> {
    fn default() -> Self {
        Self {
            max_pool_size: 100,
            max_active_objects: None,
            validate_on_return: false,
            validation_function: None,
            operation_timeout: Some(Duration::from_secs(30)),
            time_to_live: None,
            idle_timeout: None,
            warmup_size: None,
            enable_circuit_breaker: false,
            circuit_breaker_threshold: 5,
            circuit_breaker_timeout: Duration::from_secs(60),
        }
    }
}

impl<T> PoolConfiguration<T> {
    /// Create a new configuration with default values
    pub fn new() -> Self {
        Self::default()
    }
    
    /// Set the maximum pool size
    pub fn with_max_pool_size(mut self, size: usize) -> Self {
        self.max_pool_size = size;
        self
    }
    
    /// Set the maximum active objects
    pub fn with_max_active_objects(mut self, count: usize) -> Self {
        self.max_active_objects = Some(count);
        self
    }
    
    /// Enable validation on return
    pub fn with_validation(mut self, func: fn(&T) -> bool) -> Self {
        self.validate_on_return = true;
        self.validation_function = Some(func);
        self
    }
    
    /// Set operation timeout
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.operation_timeout = Some(timeout);
        self
    }
    
    /// Set time-to-live for objects
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.time_to_live = Some(ttl);
        self
    }
    
    /// Set idle timeout for objects
    pub fn with_idle_timeout(mut self, timeout: Duration) -> Self {
        self.idle_timeout = Some(timeout);
        self
    }
    
    /// Set warm-up size
    pub fn with_warmup(mut self, size: usize) -> Self {
        self.warmup_size = Some(size);
        self
    }
    
    /// Enable circuit breaker
    pub fn with_circuit_breaker(mut self, threshold: usize, timeout: Duration) -> Self {
        self.enable_circuit_breaker = true;
        self.circuit_breaker_threshold = threshold;
        self.circuit_breaker_timeout = timeout;
        self
    }
}
