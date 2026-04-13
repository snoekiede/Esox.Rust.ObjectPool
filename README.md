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

fn main() {
    let config = PoolConfiguration::new()
        .with_max_pool_size(100)
        .with_max_active_objects(50)
        .with_validation(|x| *x > 0)
        .with_timeout(Duration::from_secs(30));

    let pool = ObjectPool::new(vec![1, 2, 3, 4, 5], config);
}
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

fn main() {
    let connections = vec![
        Connection { id: 1, name: "DB1".to_string() },
        Connection { id: 2, name: "DB2".to_string() },
    ];

    let pool = QueryableObjectPool::new(connections, PoolConfiguration::default());

    // Find specific connection
    let conn = pool.get_object(|c| c.id == 2).unwrap();
    println!("Found: {}", conn.name);
}
```

### Dynamic Pool with Factory

```rust
use objectpool::{DynamicObjectPool, PoolConfiguration};

fn main() {
    let pool = DynamicObjectPool::new(
        || {
            println!("Creating new object");
            42
        },
        PoolConfiguration::new().with_max_pool_size(10),
    );

    // Objects created on demand
    let obj = pool.get_object().unwrap();
    println!("Got: {}", *obj);
}
```

### Pool Warm-up

```rust
use objectpool::{DynamicObjectPool, PoolConfiguration};

fn main() {
    let pool = DynamicObjectPool::new(
        || vec![0u8; 1024], // simulate an expensive object
        PoolConfiguration::new().with_max_pool_size(10),
    );

    // Pre-populate pool to avoid cold-start latency
    pool.warmup(10).unwrap();
    println!("Warmed up: {} objects ready", pool.get_health_status().available_objects);
}
```

```rust
use objectpool::{DynamicObjectPool, PoolConfiguration};

#[tokio::main]
async fn main() {
    let pool = DynamicObjectPool::new(
        || vec![0u8; 1024],
        PoolConfiguration::new().with_max_pool_size(10),
    );

    // Async pre-population
    pool.warmup_async(10).await.unwrap();
    println!("Warmed up: {} objects ready", pool.get_health_status().available_objects);
}
```

### Eviction / TTL

```rust
use objectpool::{ObjectPool, PoolConfiguration};
use std::time::Duration;

fn main() {
    let config = PoolConfiguration::new()
        .with_ttl(Duration::from_secs(3600))        // Objects expire after 1 hour
        .with_idle_timeout(Duration::from_secs(600)); // Or 10 minutes idle

    let pool = ObjectPool::new(vec![1, 2, 3], config);
    // Expired objects automatically filtered on retrieval
}
```

### Circuit Breaker

The circuit breaker tracks *consecutive* pool failures. A success in the `Closed` state resets the failure counter to zero, so only an unbroken run of failures opens the circuit.

State transitions:
- **Closed → Open**: failure count reaches `threshold` without an intervening success
- **Open → Half-Open**: after `timeout` elapses, one probe request is allowed through
- **Half-Open → Closed**: 3 consecutive successes close the circuit
- **Half-Open → Open**: any single failure immediately re-opens the circuit

> **Note:** Pool-empty events are recorded as failures. A legitimately busy pool that exhausts its objects will increment the failure counter. If the pool empties `threshold` times in a row (with no successful checkout in between), the circuit will open. Size your pool and threshold accordingly.

```rust
use objectpool::{ObjectPool, PoolConfiguration};
use std::time::Duration;

fn main() {
    let config = PoolConfiguration::new()
        .with_circuit_breaker(
            5,                           // Consecutive-failure threshold
            Duration::from_secs(60)      // How long to stay Open before probing
        );

    let pool = ObjectPool::new(vec![1, 2, 3], config);
}
```

### Health Monitoring

```rust
fn main() {
    let pool = objectpool::ObjectPool::new(vec![1, 2, 3], Default::default());
    let health = pool.get_health_status();
    println!("Healthy: {}", health.is_healthy);
    println!("Utilization: {:.1}%", health.utilization * 100.0);
    println!("Active: {}, Available: {}",
        health.active_objects, health.available_objects);
}
```

### Metrics Export

```rust
use objectpool::ObjectPool;
use std::collections::HashMap;

fn main() {
    let pool = ObjectPool::new(vec![1, 2, 3], Default::default());

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
}
```

## Core Types

### `PooledObject<T>`

A smart pointer that automatically returns objects to the pool when dropped (RAII pattern).

```rust
use objectpool::ObjectPool;

fn main() {
    let pool = ObjectPool::new(vec![1, 2, 3], Default::default());
    {
        let obj = pool.get_object().unwrap();
        println!("{}", *obj);  // Deref to access value
        // Automatically returned here
    }
}
```

**`unwrap()`** — Consumes the `PooledObject` and returns the inner value *without* returning it to the pool. The object's active slot is released (so `max_active_objects` and metrics are updated correctly), but the object itself is permanently removed from pool capacity. Use this only when you intentionally want to take ownership and discard the object.

```rust
use objectpool::ObjectPool;

fn main() {
    let pool = ObjectPool::new(vec![1], Default::default());
    let obj = pool.get_object().unwrap();
    let value: i32 = obj.unwrap(); // Active slot freed; value is gone from the pool
    println!("{}", value);
}
```

### `ObjectPool<T>`

Fixed-size pool with pre-allocated objects.

**Methods:**
- `new(objects, config)` - Create pool with initial objects
- `get_object()` - Get object (non-blocking; returns `Err(PoolError::PoolEmpty)` if empty, or `Err(PoolError::CircuitBreakerOpen)` / `Err(PoolError::MaxActiveObjectsReached)` for operational guards)
- `try_get_object()` - Try to get object; returns `Ok(None)` **only** for an empty pool — operational errors (`CircuitBreakerOpen`, `MaxActiveObjectsReached`) are still returned as `Err`
- `get_object_async()` - Async get with timeout; **non-retryable errors (`CircuitBreakerOpen`, `MaxActiveObjectsReached`) are returned immediately** without waiting for the timeout
- `try_get_object_async()` - Thin async wrapper around `try_get_object()`; performs a single non-blocking attempt (no polling loop, no timeout)
- `get_health_status()` - Get health status
- `export_metrics()` - Export metrics as HashMap
- `export_metrics_prometheus()` - Export in Prometheus format

### `QueryableObjectPool<T>`

Pool that supports finding objects by predicate.

**Methods:**
- `new(objects, config)` - Create queryable pool
- `get_object(predicate)` - Find object matching predicate; O(n) worst case
- `try_get_object(predicate)` - Returns `Ok(None)` only when no match is found; propagates operational errors as `Err`
- `get_object_async(predicate)` - Async find with timeout; non-retryable errors fail fast

### `DynamicObjectPool<T>`

Pool that creates objects on-demand using a factory function.

**Methods:**
- `new(factory, config)` - Create with factory function
- `with_initial(factory, objects, config)` - Create with initial objects and factory
- `get_object()` - Returns an available pooled object if one exists; calls the factory to create a new one **only** when the pool is empty *and* the active count is below capacity. `CircuitBreakerOpen` and `MaxActiveObjectsReached` are propagated immediately — the factory is **not** called.
- `warmup(count)` - Pre-populate pool (capped at pool capacity)
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
| `try_get_object()` | O(1) | Non-blocking variant with error propagation |

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

**Known Limitations:**
- `PooledObject::unwrap()` permanently removes the object from pool capacity; the active slot is freed but the object is never returned. Avoid in hot paths where maintaining pool size matters.
- `try_get_object_async()` is a thin async wrapper around the synchronous `try_get_object()` — it performs one non-blocking attempt and returns immediately. It does **not** poll or apply a timeout.
- The circuit breaker uses a consecutive-failure counter. Pool-empty events count as failures, so a legitimately busy pool can trip the breaker if the threshold is set too low relative to pool size.
- When the return-to-pool queue push fails after retries (e.g. under extreme contention with a full queue), the object is discarded and the `queue_push_failures` metric is incremented. This permanently reduces pool capacity.
- No built-in integration with web frameworks (e.g. Actix, Axum, Rocket).
- Health checks and metrics endpoints must be manually wired up.
- Async operations are not cancelable via the pool API (use `tokio::time::timeout` externally if needed).

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
