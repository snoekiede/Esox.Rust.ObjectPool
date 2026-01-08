//! Async usage examples

use esox_objectpool::{ObjectPool, DynamicObjectPool, PoolConfiguration};
use std::time::Duration;
use tokio::time::sleep;

#[tokio::main]
async fn main() {
    println!("=== EsoxSolutions.ObjectPool - Async Examples ===\n");
    
    // Example 1: Async get
    async_get().await;
    
    // Example 2: Async with timeout
    async_with_timeout().await;
    
    // Example 3: Dynamic pool with warmup
    dynamic_warmup().await;
    
    // Example 4: Concurrent access
    concurrent_access().await;
}

async fn async_get() {
    println!("1. Async Get:");
    let pool = ObjectPool::new(vec![1, 2, 3], PoolConfiguration::default());
    
    {
        let obj = pool.get_object_async().await.unwrap();
        println!("   Got object asynchronously: {}", *obj);
    }
    
    println!();
}

async fn async_with_timeout() {
    println!("2. Async with Timeout:");
    
    let config = PoolConfiguration::new()
        .with_timeout(Duration::from_millis(100));
    
    let pool = ObjectPool::new(vec![42], config);
    
    // Get the only object
    let _obj = pool.get_object().unwrap();
    
    // Try to get another (should timeout)
    let result = pool.get_object_async().await;
    match result {
        Ok(_) => println!("   Got object"),
        Err(e) => println!("   Error: {}", e),
    }
    
    println!();
}

async fn dynamic_warmup() {
    println!("3. Dynamic Pool with Warmup:");
    
    let pool = DynamicObjectPool::new(
        || {
            println!("   Creating new object...");
            42
        },
        PoolConfiguration::new().with_max_pool_size(10),
    );
    
    // Warm up the pool
    println!("   Warming up pool with 5 objects...");
    pool.warmup_async(5).await.unwrap();
    println!("   Available after warmup: {}", pool.get_health_status().available_objects);
    
    // Get object (should not create new one)
    {
        let obj = pool.get_object().unwrap();
        println!("   Got pre-created object: {}", *obj);
    }
    
    println!();
}

async fn concurrent_access() {
    println!("4. Concurrent Access:");
    
    let pool = std::sync::Arc::new(
        ObjectPool::new(vec![1, 2, 3, 4, 5], PoolConfiguration::default())
    );
    
    let mut handles = vec![];
    
    for i in 0..10 {
        let pool_clone = std::sync::Arc::clone(&pool);
        let handle = tokio::spawn(async move {
            if let Some(obj) = pool_clone.try_get_object_async().await {
                println!("   Task {} got object: {}", i, *obj);
                sleep(Duration::from_millis(50)).await;
            } else {
                println!("   Task {} couldn't get object", i);
            }
        });
        handles.push(handle);
    }
    
    for handle in handles {
        handle.await.unwrap();
    }
    
    println!("   Final available: {}", pool.available_count());
}
