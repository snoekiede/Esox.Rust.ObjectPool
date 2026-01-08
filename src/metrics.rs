//! Metrics collection and export for object pools

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Metrics data for a pool
///
/// # Examples
///
/// ```
/// use esox_objectpool::{ObjectPool, PoolConfiguration};
///
/// let pool = ObjectPool::new(vec![1, 2, 3], PoolConfiguration::default());
///
/// {
///     let _obj = pool.get_object().unwrap();
///     let metrics = pool.get_metrics();
///     assert_eq!(metrics.total_retrieved, 1);
///     assert_eq!(metrics.active_objects, 1);
/// }
/// ```
#[derive(Debug, Clone)]
pub struct PoolMetrics {
    /// Total objects retrieved from pool
    pub total_retrieved: usize,
    
    /// Total objects returned to pool
    pub total_returned: usize,
    
    /// Current active objects
    pub active_objects: usize,
    
    /// Current available objects
    pub available_objects: usize,
    
    /// Number of times pool was empty
    pub pool_empty_events: usize,
    
    /// Validation failures
    pub validation_failures: usize,
    
    /// Pool utilization ratio (0.0 to 1.0)
    pub utilization: f64,
    
    /// Maximum pool capacity
    pub max_capacity: usize,
}

impl PoolMetrics {
    /// Export metrics as a HashMap
    pub fn export(&self) -> HashMap<String, String> {
        let mut metrics = HashMap::new();
        metrics.insert("total_retrieved".to_string(), self.total_retrieved.to_string());
        metrics.insert("total_returned".to_string(), self.total_returned.to_string());
        metrics.insert("active_objects".to_string(), self.active_objects.to_string());
        metrics.insert("available_objects".to_string(), self.available_objects.to_string());
        metrics.insert("pool_empty_events".to_string(), self.pool_empty_events.to_string());
        metrics.insert("validation_failures".to_string(), self.validation_failures.to_string());
        metrics.insert("utilization".to_string(), format!("{:.2}", self.utilization));
        metrics.insert("max_capacity".to_string(), self.max_capacity.to_string());
        metrics
    }
}

/// Metrics exporter for Prometheus format
pub struct MetricsExporter;

impl MetricsExporter {
    /// Export metrics in Prometheus exposition format
    ///
    /// # Examples
    ///
    /// ```
    /// use esox_objectpool::{ObjectPool, PoolConfiguration};
    /// use std::collections::HashMap;
    ///
    /// let pool = ObjectPool::new(vec![1, 2, 3], PoolConfiguration::default());
    ///
    /// let mut tags = HashMap::new();
    /// tags.insert("service".to_string(), "api".to_string());
    ///
    /// let output = pool.export_metrics_prometheus("my_pool", Some(&tags));
    /// assert!(output.contains("objectpool_objects_active"));
    /// assert!(output.contains("service=\"api\""));
    /// ```
    pub fn export_prometheus(
        metrics: &PoolMetrics,
        pool_name: &str,
        tags: Option<&HashMap<String, String>>,
    ) -> String {
        let mut output = String::new();
        let labels = Self::format_labels(pool_name, tags);
        
        // Gauge metrics
        output.push_str("# HELP objectpool_objects_active Current active objects\n");
        output.push_str("# TYPE objectpool_objects_active gauge\n");
        output.push_str(&format!("objectpool_objects_active{{{}}} {}\n", labels, metrics.active_objects));
        
        output.push_str("# HELP objectpool_objects_available Current available objects\n");
        output.push_str("# TYPE objectpool_objects_available gauge\n");
        output.push_str(&format!("objectpool_objects_available{{{}}} {}\n", labels, metrics.available_objects));
        
        output.push_str("# HELP objectpool_utilization Pool utilization ratio\n");
        output.push_str("# TYPE objectpool_utilization gauge\n");
        output.push_str(&format!("objectpool_utilization{{{}}} {:.2}\n", labels, metrics.utilization));
        
        // Counter metrics
        output.push_str("# HELP objectpool_objects_retrieved_total Total objects retrieved\n");
        output.push_str("# TYPE objectpool_objects_retrieved_total counter\n");
        output.push_str(&format!("objectpool_objects_retrieved_total{{{}}} {}\n", labels, metrics.total_retrieved));
        
        output.push_str("# HELP objectpool_objects_returned_total Total objects returned\n");
        output.push_str("# TYPE objectpool_objects_returned_total counter\n");
        output.push_str(&format!("objectpool_objects_returned_total{{{}}} {}\n", labels, metrics.total_returned));
        
        output.push_str("# HELP objectpool_events_empty_total Pool empty events\n");
        output.push_str("# TYPE objectpool_events_empty_total counter\n");
        output.push_str(&format!("objectpool_events_empty_total{{{}}} {}\n", labels, metrics.pool_empty_events));
        
        output.push_str("# HELP objectpool_validation_failures_total Validation failures\n");
        output.push_str("# TYPE objectpool_validation_failures_total counter\n");
        output.push_str(&format!("objectpool_validation_failures_total{{{}}} {}\n", labels, metrics.validation_failures));
        
        output
    }
    
    fn format_labels(pool_name: &str, tags: Option<&HashMap<String, String>>) -> String {
        let mut labels = vec![format!("pool=\"{}\"", pool_name)];
        
        if let Some(tags) = tags {
            for (key, value) in tags {
                labels.push(format!("{}=\"{}\"", key, value));
            }
        }
        
        labels.join(",")
    }
}

/// Internal metrics tracker
pub(crate) struct MetricsTracker {
    pub total_retrieved: Arc<AtomicUsize>,
    pub total_returned: Arc<AtomicUsize>,
    pub pool_empty_events: Arc<AtomicUsize>,
    pub validation_failures: Arc<AtomicUsize>,
}

impl MetricsTracker {
    pub fn new() -> Self {
        Self {
            total_retrieved: Arc::new(AtomicUsize::new(0)),
            total_returned: Arc::new(AtomicUsize::new(0)),
            pool_empty_events: Arc::new(AtomicUsize::new(0)),
            validation_failures: Arc::new(AtomicUsize::new(0)),
        }
    }
    
    pub fn get_metrics(&self, active: usize, available: usize, capacity: usize) -> PoolMetrics {
        let utilization = if capacity > 0 {
            active as f64 / capacity as f64
        } else {
            0.0
        };
        
        PoolMetrics {
            total_retrieved: self.total_retrieved.load(Ordering::Relaxed),
            total_returned: self.total_returned.load(Ordering::Relaxed),
            active_objects: active,
            available_objects: available,
            pool_empty_events: self.pool_empty_events.load(Ordering::Relaxed),
            validation_failures: self.validation_failures.load(Ordering::Relaxed),
            utilization,
            max_capacity: capacity,
        }
    }
}

impl Default for MetricsTracker {
    fn default() -> Self {
        Self::new()
    }
}
