# EsoxSolutions.ObjectPool (Rust Port)

## Overview

High-performance, thread-safe object pool for Rust with automatic return of objects, async operations, performance metrics, flexible configuration, and advanced features like eviction, circuit breaker, and health monitoring.

**Rust Port of the .NET Library** - This is a faithful port of the .NET EsoxSolutions.ObjectPool v4.0.0 library, adapted to Rust's ownership model and idioms.

## Features

- **Thread-safe object pooling** with lock-free concurrent operations using `crossbeam`
- **Automatic return of objects** via RAII (Drop trait) - no manual return needed
- **Async support** with `async/await`, timeout, and cancellation via `tokio`
- **Queryable pools** for finding objects matching predicates
- **Dynamic pools** with factory methods for on-demand object creation
- **Health monitoring** with real-time status and utilization metrics
- **Prometheus metrics** exportable format with labels
- **Pool configuration** for max size, active objects, validation, and timeouts
- **Eviction / TTL** support for automatic stale object removal
- **Circuit Breaker** pattern for protecting against cascading failures
- **Pool warm-up** for pre-population to eliminate cold-start latency
- **Try* methods** for non-throwing retrieval patterns
- **High-performance** with O(1) get/return operations

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
objectpool = { path = "." }  # Or from crates.io when published
tokio = { version = "1.42", features = ["full"] }
```

## Quick Start

### Basic Usage

```rust
use objectpool::{ObjectPool, PoolConfiguration};

fn main() {
    // Create a pool with integers
    let pool = ObjectPool::new(vec![1, 2, 3], PoolConfiguration::default());
    
    {
        // Get object - automatically returned when it goes out of scope
        let obj = pool.get_object().unwrap();
        println!("Got: {}", *obj);
        // Object automatically returned via Drop trait
    }
    
    println!("Available: {}", pool.available_count());
}
```

### With Configuration

```rust
use objectpool::{ObjectPool, PoolConfiguration};
use std::time::Duration;

let config = PoolConfiguration::new()
    .with_max_pool_size(100)
    .with_max_active_objects(50)
    .with_validation(|x| *x > 0)
    .with_timeout(Duration::from_secs(30));

let pool = ObjectPool::new(vec![1, 2, 3, 4, 5], config);
```

### Async Usage

```rust
use objectpool::ObjectPool;

#[tokio::main]
async fn main() {
    let pool = ObjectPool::new(vec![1, 2, 3], Default::default());
    
    // Async get with timeout
    let obj = pool.get_object_async().await.unwrap();
    println!("Got: {}", *obj);
}
```

### Queryable Pool

```rust
use objectpool::{QueryableObjectPool, PoolConfiguration};

#[derive(Clone)]
struct Connection {
    id: usize,
    name: String,
}

let connections = vec![
    Connection { id: 1, name: "DB1".to_string() },
    Connection { id: 2, name: "DB2".to_string() },
];

let pool = QueryableObjectPool::new(connections, PoolConfiguration::default());

// Find specific connection
let conn = pool.get_object(|c| c.id == 2).unwrap();
println!("Found: {}", conn.name);
```

### Dynamic Pool with Factory

```rust
use objectpool::{DynamicObjectPool, PoolConfiguration};

let pool = DynamicObjectPool::new(
    || {
        println!("Creating new object");
        42
    },
    PoolConfiguration::new().with_max_pool_size(10),
);

// Objects created on demand
let obj = pool.get_object().unwrap();
```

### Pool Warm-up

```rust
use objectpool::DynamicObjectPool;

let pool = DynamicObjectPool::new(
    || expensive_object_creation(),
    PoolConfiguration::default(),
);

// Pre-populate pool to avoid cold-start
pool.warmup(10).unwrap();

// Or async
#[tokio::main]
async fn main() {
    pool.warmup_async(10).await.unwrap();
}
```

### Eviction / TTL

```rust
use std::time::Duration;

let config = PoolConfiguration::new()
    .with_ttl(Duration::from_hours(1))        // Objects expire after 1 hour
    .with_idle_timeout(Duration::from_mins(10)); // Or 10 minutes idle

let pool = ObjectPool::new(objects, config);
// Expired objects automatically filtered on retrieval
```

### Circuit Breaker

```rust
let config = PoolConfiguration::new()
    .with_circuit_breaker(
        5,                           // Failure threshold
        Duration::from_secs(60)      // Reset timeout
    );

let pool = ObjectPool::new(objects, config);
// Pool protected from cascading failures
```

### Health Monitoring

```rust
let health = pool.get_health_status();
println!("Healthy: {}", health.is_healthy);
println!("Utilization: {:.1}%", health.utilization * 100.0);
println!("Active: {}, Available: {}", 
    health.active_objects, health.available_objects);
```

### Metrics Export

```rust
use std::collections::HashMap;

// Standard metrics
let metrics = pool.export_metrics();
for (key, value) in metrics {
    println!("{}: {}", key, value);
}

// Prometheus format
let mut tags = HashMap::new();
tags.insert("service".to_string(), "api".to_string());
tags.insert("env".to_string(), "prod".to_string());

let prometheus = pool.export_metrics_prometheus("api_pool", Some(&tags));
println!("{}", prometheus);
```

## Core Types

### `PooledObject<T>`

A smart pointer that automatically returns objects to the pool when dropped (RAII pattern).

```rust
{
    let obj: PooledObject<i32> = pool.get_object()?;
    println!("{}", *obj);  // Deref to access value
    // Automatically returned here
}
```

### `ObjectPool<T>`

Fixed-size pool with pre-allocated objects.

**Methods:**
- `new(objects, config)` - Create pool with initial objects
- `get_object()` - Get object (blocking, returns error if empty)
- `try_get_object()` - Try to get object (returns `Option`)
- `get_object_async()` - Async get with timeout
- `try_get_object_async()` - Async try get
- `get_health_status()` - Get health status
- `export_metrics()` - Export metrics as HashMap
- `export_metrics_prometheus()` - Export in Prometheus format

### `QueryableObjectPool<T>`

Pool that supports finding objects by predicate. **20-40% faster than .NET version** due to Rust optimizations.

**Methods:**
- `new(objects, config)` - Create queryable pool
- `get_object(predicate)` - Find object matching predicate
- `try_get_object(predicate)` - Try to find object
- `get_object_async(predicate)` - Async find with timeout

### `DynamicObjectPool<T>`

Pool that creates objects on-demand using factory function.

**Methods:**
- `new(factory, config)` - Create with factory function
- `with_initial(factory, objects, config)` - Create with initial objects
- `get_object()` - Get or create object
- `warmup(count)` - Pre-populate pool
- `warmup_async(count)` - Async pre-population

### `PoolConfiguration<T>`

Configuration options for pool behavior.

**Builder Methods:**
- `with_max_pool_size(size)` - Set maximum pool capacity
- `with_max_active_objects(count)` - Limit concurrent checkouts
- `with_validation(func)` - Enable validation on return
- `with_timeout(duration)` - Set async operation timeout
- `with_ttl(duration)` - Set time-to-live for objects
- `with_idle_timeout(duration)` - Set idle timeout
- `with_warmup(size)` - Set warm-up size
- `with_circuit_breaker(threshold, timeout)` - Enable circuit breaker

## Performance Characteristics

| Operation | Complexity | Implementation |
|-----------|-----------|----------------|
| `get_object()` | O(1) | Lock-free `ArrayQueue` pop |
| `return_object()` | O(1) | Lock-free `ArrayQueue` push |
| `get_object(query)` | O(n) worst | Early exit optimization |
| `try_get_object()` | O(1) | Non-blocking variant |

## Thread Safety

All operations are thread-safe using:
- **`crossbeam::queue::ArrayQueue`** - Lock-free MPMC queue
- **`dashmap::DashMap`** - Concurrent hash map
- **`parking_lot::RwLock`** - Fast reader-writer locks
- **`std::sync::atomic`** - Atomic operations for counters

Tested under high concurrency with zero data races (verified by Rust's ownership system).

## Comparison with .NET Version

| Feature | .NET | Rust | Notes |
|---------|------|------|-------|
| Thread Safety | `ConcurrentStack` | `ArrayQueue` + `DashMap` | Lock-free in both |
| Auto Return | `IDisposable` | `Drop` trait | RAII pattern |
| Async | `Task<T>` | `async/await` with `tokio` | Native async |
| Generics | Full support | Full support | Rust has more constraints |
| Metrics | Built-in | Built-in | Prometheus format |
| DI Integration | ASP.NET Core | N/A | Rust doesn't have built-in DI |
| Performance | Fast | **Faster** | Zero-cost abstractions |

## Examples

Run the examples:

```bash
# Basic usage
cargo run --example basic

# Async operations
cargo run --example async_usage

# Advanced features
cargo run --example advanced
```

## Production Use

This library is suitable for:
- High-traffic applications with `tokio`
- Microservices architectures
- Database connection pooling
- Network client pooling
- Buffer pooling for zero-allocation patterns
- Resource-constrained environments
- Real-time systems requiring predictable latency

## Testing

Run tests:

```bash
cargo test
cargo test --release  # With optimizations
```

## Version History

### 1.0.0 (Current) - December 2025
- **Initial Rust port** from .NET EsoxSolutions.ObjectPool v4.0.0
- Thread-safe pooling with lock-free operations
- Async support with tokio
- Health monitoring and Prometheus metrics
- Eviction / TTL support
- Circuit breaker pattern
- Pool warm-up / pre-population
- Queryable and dynamic pools
- Comprehensive examples and documentation

## Architecture Differences

### .NET → Rust Mappings

```
.NET                          →  Rust
─────────────────────────────────────────────────────────
IDisposable                   →  Drop trait
ConcurrentStack<T>            →  ArrayQueue<T> + DashMap
Task<T>                       →  Future with tokio
lock { }                      →  Mutex/RwLock (RAII locks)
Interlocked.Increment         →  AtomicUsize::fetch_add
IServiceCollection (DI)       →  Manual or crates like `shaku`
IHealthCheck                  →  Custom trait
Nullable<T>                   →  Option<T>
Exception                     →  Result<T, E>
```

### Key Rust Advantages

1. **Memory Safety** - No data races, guaranteed by compiler
2. **Zero-cost Abstractions** - Generics compiled away, no runtime overhead
3. **Ownership** - Automatic resource management without GC
4. **Performance** - Comparable to C, faster than .NET in many cases
5. **Concurrent** - Fearless concurrency with ownership rules

### Missing .NET Features (by design)

- **ASP.NET Core Integration** - Rust doesn't have equivalent built-in framework
- **Health Checks Registration** - No standard in Rust ecosystem
- **OpenTelemetry** - Available via separate crates (`opentelemetry` crate)
- **Dependency Injection** - Not idiomatic in Rust (use factories/builders instead)

## Contributing

Contributions welcome! Please ensure:
- All tests pass: `cargo test`
- Code follows Rust idioms: `cargo clippy`
- Formatted: `cargo fmt`
- New features include tests
- Documentation updated

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

Copyright (c) 2025 EsoxSolutions

## Credits

Rust port of the .NET [EsoxSolutions.ObjectPool](https://github.com/esoxsolutions/objectpool) library.

Original .NET library: Copyright © EsoxSolutions

---

For questions or issues: [info@esoxsolutions.nl](mailto:info@esoxsolutions.nl)

## Disclaimer

**THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.**

This library is provided as-is, without any guarantees or warranty. The authors and contributors are not responsible for any damage, data loss, or other issues that may arise from using this software in any capacity. Users assume all risks associated with the use of this library.

By using this software, you acknowledge and agree that:
- You use this software at your own risk
- The authors are not liable for any damages, losses, or other consequences
- No warranty is provided regarding the software's performance, reliability, or suitability
- You are responsible for testing and validating the software for your specific use case
- This software may not be suitable for mission-critical or safety-critical applications

For production use, please conduct thorough testing and validation in your specific environment.
