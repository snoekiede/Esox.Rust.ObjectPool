//! Health monitoring for object pools

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

/// Health status of an object pool
///
/// # Examples
///
/// ```
/// use esox_objectpool::{ObjectPool, PoolConfiguration};
///
/// let pool = ObjectPool::new(vec![1, 2, 3], PoolConfiguration::default());
///
/// let health = pool.get_health_status();
/// assert!(health.is_healthy());
/// assert_eq!(health.available_objects, 3);
/// ```
#[derive(Debug, Clone)]
pub struct HealthStatus {
    /// Whether the pool is healthy
    pub is_healthy: bool,
    
    /// Number of warnings detected
    pub warning_count: usize,
    
    /// Current pool utilization (0.0 to 1.0)
    pub utilization: f64,
    
    /// Available objects count
    pub available_objects: usize,
    
    /// Active objects count
    pub active_objects: usize,
    
    /// Total capacity
    pub total_capacity: usize,
    
    /// Warning messages
    pub warnings: Vec<String>,
}

impl HealthStatus {
    /// Create a new health status
    pub fn new(available: usize, active: usize, capacity: usize) -> Self {
        let utilization = if capacity > 0 {
            active as f64 / capacity as f64
        } else {
            0.0
        };
        
        let mut warnings = Vec::new();
        let mut is_healthy = true;
        
        // Check for high utilization
        if utilization > 0.9 {
            warnings.push(format!("High utilization: {:.1}%", utilization * 100.0));
            is_healthy = false;
        }
        
        // Check if pool is empty
        if available == 0 && capacity > 0 {
            warnings.push("Pool is empty".to_string());
        }
        
        Self {
            is_healthy,
            warning_count: warnings.len(),
            utilization,
            available_objects: available,
            active_objects: active,
            total_capacity: capacity,
            warnings,
        }
    }
    
    /// Check if the pool is healthy
    pub fn is_healthy(&self) -> bool {
        self.is_healthy
    }
}

/// Internal health tracker for pools
#[allow(dead_code)]
pub(crate) struct HealthTracker {
    pub total_retrieved: Arc<AtomicUsize>,
    pub total_returned: Arc<AtomicUsize>,
    pub pool_empty_count: Arc<AtomicUsize>,
    pub validation_failures: Arc<AtomicUsize>,
    pub is_healthy: Arc<AtomicBool>,
}

impl HealthTracker {
    pub fn new() -> Self {
        Self {
            total_retrieved: Arc::new(AtomicUsize::new(0)),
            total_returned: Arc::new(AtomicUsize::new(0)),
            pool_empty_count: Arc::new(AtomicUsize::new(0)),
            validation_failures: Arc::new(AtomicUsize::new(0)),
            is_healthy: Arc::new(AtomicBool::new(true)),
        }
    }
    
    pub fn increment_retrieved(&self) {
        self.total_retrieved.fetch_add(1, Ordering::Relaxed);
    }
    
    pub fn increment_returned(&self) {
        self.total_returned.fetch_add(1, Ordering::Relaxed);
    }
    
    pub fn increment_empty(&self) {
        self.pool_empty_count.fetch_add(1, Ordering::Relaxed);
    }
    
    #[allow(dead_code)]
    pub fn increment_validation_failure(&self) {
        self.validation_failures.fetch_add(1, Ordering::Relaxed);
    }
    
    #[allow(dead_code)]
    pub fn set_health(&self, healthy: bool) {
        self.is_healthy.store(healthy, Ordering::Relaxed);
    }
}

impl Default for HealthTracker {
    fn default() -> Self {
        Self::new()
    }
}
