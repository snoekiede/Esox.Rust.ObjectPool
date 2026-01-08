//! Advanced features: eviction, circuit breaker, queryable pools

use esox_objectpool::{
    ObjectPool, QueryableObjectPool, DynamicObjectPool, 
    PoolConfiguration, CircuitBreaker
};
use std::time::Duration;
use std::thread;

#[derive(Debug, Clone)]
struct Connection {
    id: usize,
    data: String,
}

impl Connection {
    fn new(id: usize) -> Self {
        Self {
            id,
            data: format!("Connection-{}", id),
        }
    }
}

fn main() {
    println!("=== EsoxSolutions.ObjectPool - Advanced Features ===\n");
    
    // Example 1: Queryable pool
    queryable_pool();
    
    // Example 2: Eviction / TTL
    eviction_ttl();
    
    // Example 3: Circuit breaker
    circuit_breaker_demo();
    
    // Example 4: Prometheus metrics
    prometheus_export();
    
    // Example 5: Complex objects
    complex_objects();
}

fn queryable_pool() {
    println!("1. Queryable Pool:");
    
    let connections = vec![
        Connection::new(1),
        Connection::new(2),
        Connection::new(3),
    ];
    
    let pool = QueryableObjectPool::new(connections, PoolConfiguration::default());
    
    // Find specific connection
    {
        let conn = pool.get_object(|c| c.id == 2).unwrap();
        println!("   Found: {:?}", *conn);
    }
    
    // Try to find non-existent
    let result = pool.try_get_object(|c| c.id == 99);
    println!("   Find ID 99: {}", if result.is_some() { "Found" } else { "Not found" });
    
    println!();
}

fn eviction_ttl() {
    println!("2. Eviction / TTL:");
    
    let config = PoolConfiguration::new()
        .with_ttl(Duration::from_secs(2))
        .with_idle_timeout(Duration::from_secs(1));
    
    let pool = ObjectPool::new(vec![1, 2, 3], config);
    
    println!("   Initial available: {}", pool.available_count());
    
    // Get and return object quickly
    {
        let obj = pool.get_object().unwrap();
        println!("   Got: {}", *obj);
    }
    
    println!("   Available after quick return: {}", pool.available_count());
    
    // Wait for idle timeout
    println!("   Waiting for idle timeout...");
    thread::sleep(Duration::from_millis(1100));
    
    // Try to get object (expired ones will be filtered)
    let obj = pool.try_get_object();
    println!("   After timeout: {}", if obj.is_some() { "Got object" } else { "Object expired" });
    
    println!();
}

fn circuit_breaker_demo() {
    println!("3. Circuit Breaker:");
    
    let config = PoolConfiguration::new()
        .with_circuit_breaker(3, Duration::from_secs(5));
    
    let pool = ObjectPool::new(vec![1], config);
    
    // Get the only object
    let _obj = pool.get_object().unwrap();
    
    // Try to get more (will fail and trigger circuit breaker)
    println!("   Attempting to get from empty pool...");
    for i in 0..5 {
        match pool.try_get_object() {
            Some(_) => println!("   Attempt {}: Success", i + 1),
            None => println!("   Attempt {}: Failed", i + 1),
        }
    }
    
    println!();
}

fn prometheus_export() {
    println!("4. Prometheus Metrics Export:");
    
    let pool = ObjectPool::new(vec![1, 2, 3, 4, 5], PoolConfiguration::default());
    
    // Use some objects
    {
        let _obj1 = pool.get_object().unwrap();
        let _obj2 = pool.get_object().unwrap();
        
        let mut tags = std::collections::HashMap::new();
        tags.insert("service".to_string(), "example".to_string());
        tags.insert("env".to_string(), "dev".to_string());
        
        let prometheus_text = pool.export_metrics_prometheus("example_pool", Some(&tags));
        println!("{}", prometheus_text);
    }
}

fn complex_objects() {
    println!("5. Complex Objects (Dynamic Pool):");
    
    let pool = DynamicObjectPool::new(
        || Connection::new(rand::random()),
        PoolConfiguration::new().with_max_pool_size(10),
    );
    
    // Warm up
    pool.warmup(3).unwrap();
    println!("   Warmed up with 3 connections");
    
    {
        let conn = pool.get_object().unwrap();
        println!("   Using: {:?}", *conn);
    }
    
    let health = pool.get_health_status();
    println!("   Health - Available: {}, Active: {}", 
        health.available_objects, health.active_objects);
}

// Simple random number generator for example
mod rand {
    use std::time::{SystemTime, UNIX_EPOCH};
    
    pub fn random() -> usize {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        nanos as usize
    }
}
