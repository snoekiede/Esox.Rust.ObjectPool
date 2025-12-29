//! Basic usage examples for ObjectPool

use objectpool::{ObjectPool, PoolConfiguration};

fn main() {
    println!("=== EsoxSolutions.ObjectPool - Basic Examples ===\n");
    
    // Example 1: Simple pool with integers
    simple_pool();
    
    // Example 2: Pool with configuration
    configured_pool();
    
    // Example 3: Try methods
    try_methods();
    
    // Example 4: Metrics and health
    metrics_and_health();
}

fn simple_pool() {
    println!("1. Simple Pool:");
    let pool = ObjectPool::new(vec![1, 2, 3], PoolConfiguration::default());
    
    {
        let obj = pool.get_object().unwrap();
        println!("   Got object: {}", *obj);
        // Object automatically returned when dropped
    }
    
    println!("   Available after return: {}\n", pool.available_count());
}

fn configured_pool() {
    println!("2. Configured Pool:");
    
    let config = PoolConfiguration::new()
        .with_max_pool_size(10)
        .with_max_active_objects(5)
        .with_validation(|x| *x > 0);
    
    let pool = ObjectPool::new(vec![1, 2, 3, 4, 5], config);
    
    {
        let obj1 = pool.get_object().unwrap();
        let obj2 = pool.get_object().unwrap();
        println!("   Active objects: {}", pool.active_count());
        println!("   Available objects: {}", pool.available_count());
    }
    
    println!("   After return - Available: {}\n", pool.available_count());
}

fn try_methods() {
    println!("3. Try Methods:");
    let pool = ObjectPool::new(vec![42], PoolConfiguration::default());
    
    // Get the only object
    let obj1 = pool.try_get_object();
    assert!(obj1.is_some());
    println!("   First try: Success");
    
    // Try again while object is checked out
    let obj2 = pool.try_get_object();
    assert!(obj2.is_none());
    println!("   Second try: None (pool empty)");
    
    drop(obj1); // Return object
    
    // Try again after return
    let obj3 = pool.try_get_object();
    assert!(obj3.is_some());
    println!("   Third try: Success\n");
}

fn metrics_and_health() {
    println!("4. Metrics and Health:");
    let pool = ObjectPool::new(vec![1, 2, 3, 4, 5], PoolConfiguration::default());
    
    // Use some objects
    {
        let _obj1 = pool.get_object().unwrap();
        let _obj2 = pool.get_object().unwrap();
        
        let health = pool.get_health_status();
        println!("   Health: {}", if health.is_healthy { "Healthy" } else { "Unhealthy" });
        println!("   Utilization: {:.1}%", health.utilization * 100.0);
        println!("   Active: {}, Available: {}", health.active_objects, health.available_objects);
    }
    
    let metrics = pool.export_metrics();
    println!("\n   Metrics:");
    for (key, value) in metrics {
        println!("     {}: {}", key, value);
    }
}
