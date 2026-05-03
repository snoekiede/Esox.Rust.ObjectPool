//! Health monitoring for object pools

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

    /// Whether the circuit breaker is currently open
    pub circuit_breaker_open: bool,

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
    pub fn new(
        available: usize,
        active: usize,
        capacity: usize,
        circuit_breaker_open: bool,
    ) -> Self {
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

        // Check circuit breaker
        if circuit_breaker_open {
            warnings.push("Circuit breaker is open".to_string());
            is_healthy = false;
        }

        Self {
            is_healthy,
            circuit_breaker_open,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn healthy_pool_has_no_warnings() {
        let h = HealthStatus::new(5, 2, 10, false);
        assert!(h.is_healthy);
        assert!(h.is_healthy());
        assert!(h.warnings.is_empty());
        assert_eq!(h.warning_count, 0);
        assert!(!h.circuit_breaker_open);
        assert_eq!(h.available_objects, 5);
        assert_eq!(h.active_objects, 2);
        assert_eq!(h.total_capacity, 10);
        assert!((h.utilization - 0.2).abs() < f64::EPSILON);
    }

    #[test]
    fn high_utilization_triggers_warning_and_unhealthy() {
        // 91 / 100 = 91 % > 90 %
        let h = HealthStatus::new(9, 91, 100, false);
        assert!(!h.is_healthy);
        assert!(h.warnings.iter().any(|w| w.contains("utilization") || w.contains("Utilization")));
    }

    #[test]
    fn exactly_90_percent_utilization_is_healthy() {
        // 90 / 100 = 0.90 — not *greater than* 0.9, so no warning
        let h = HealthStatus::new(10, 90, 100, false);
        assert!(h.is_healthy);
    }

    #[test]
    fn empty_pool_adds_warning_but_remains_healthy() {
        let h = HealthStatus::new(0, 0, 5, false);
        // No active objects → utilization = 0 → still healthy
        assert!(h.is_healthy);
        assert!(h.warnings.iter().any(|w| w.contains("empty") || w.contains("Empty")));
    }

    #[test]
    fn circuit_breaker_open_is_unhealthy() {
        let h = HealthStatus::new(3, 0, 5, true);
        assert!(!h.is_healthy);
        assert!(h.circuit_breaker_open);
        assert!(h.warnings.iter().any(|w| w.contains("Circuit") || w.contains("circuit")));
    }

    #[test]
    fn zero_capacity_gives_zero_utilization() {
        let h = HealthStatus::new(0, 0, 0, false);
        assert_eq!(h.utilization, 0.0);
        // "Pool is empty" warning only fires when capacity > 0
        assert!(h.warnings.iter().all(|w| !w.contains("empty") && !w.contains("Empty")));
    }

    #[test]
    fn multiple_issues_accumulate_warnings() {
        // 100% utilization AND circuit breaker open
        let h = HealthStatus::new(0, 10, 10, true);
        assert!(!h.is_healthy);
        assert!(h.warning_count >= 2);
    }

    #[test]
    fn warning_count_matches_warnings_vec_len() {
        let h = HealthStatus::new(0, 10, 10, true);
        assert_eq!(h.warning_count, h.warnings.len());
    }
}
