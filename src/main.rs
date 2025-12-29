// EsoxSolutions.ObjectPool - Rust Port
// High-performance, thread-safe object pool with async support
// Version 4.0.0

// This is just a binary wrapper - the actual library is in lib.rs
// Run examples with: cargo run --example basic

use objectpool::{ObjectPool, PoolConfiguration};

fn main() {
    println!("=== EsoxSolutions.ObjectPool v4.0.0 ===");
    println!("See examples/ directory for usage examples");
    println!("Run: cargo run --example basic");
    println!();
    
    // Quick demo
    println!("Quick Demo:");
    let pool = ObjectPool::new(vec![1, 2, 3], PoolConfiguration::default());
    
    {
        let obj = pool.get_object().unwrap();
        println!("  Got object: {}", *obj);
    }
    
    println!("  Available after return: {}", pool.available_count());
}


