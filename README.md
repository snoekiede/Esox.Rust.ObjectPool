# EsoxSolutions.ObjectPool (Rust Port)

## Overview

High-performance, thread-safe object pool for Rust with automatic return of objects, async operations, performance metrics, flexible configuration, and advanced features like eviction, circuit breaker, and health monitoring.

**Rust Port of the .NET Library** - This is a faithful port of the .NET EsoxSolutions.ObjectPool v4.0.0 library, adapted to Rust's ownership model and idioms.

## Features

- **Thread-safe object pooling** with lock-free concurrent operations using `crossbeam`
- **Atomic active-slot accounting** — `max_active_objects` is enforced via a CAS semaphore; the check and increment are a single atomic operation (no TOCTOU race)
- **Automatic return of objects** via RAII (Drop trait) - no manual return needed
- **Async support** with `async/await`, timeout, and jittered retry via `tokio`
- **Queryable pools** for finding objects matching predicates
- **Dynamic pools** with factory methods for on-demand object creation
- **Health monitoring** with real-time status and utilization metrics
- **Prometheus metrics** exportable format with labels
- **Pool configuration** for max size, active objects, validation, and timeouts
- **Eviction / TTL** support for automatic stale object removal
- **Circuit Breaker** pattern for protecting against cascading failures
- **Pool warm-up** for pre-population to eliminate cold-start latency
- **Try* methods** for non-throwing retrieval patterns
- **`#[must_use]`** on all query/observability methods — misuse caught at compile time
- **High-performance** with O(1) get/return operations

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
esox_objectpool = "1"
tokio = { version = "1", features = ["full"] }
```

If you prefer the shorter `objectpool::` path in your code, rename the dependency:

```toml
[dependencies]
objectpool = { package = "esox_objectpool", version = "1" }
tokio = { version = "1", features = ["full"] }
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
    println!("Capacity:  {}", pool.capacity());
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
    
    // Async get with timeout (uses jittered retry internally)
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

Expired objects are filtered out lazily on each `get_object()` call. For strict TTL
enforcement, call `evict_expired()` periodically — for example from a background task:

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

```rust
use objectpool::ObjectPool;
use std::sync::Arc;
use std::time::Duration;

#[tokio::main]
async fn main() {
    let pool = Arc::new(ObjectPool::new(vec![1, 2, 3],
        objectpool::PoolConfiguration::new().with_ttl(Duration::from_secs(60))));

    // Background eviction sweep every 30 seconds
    let pool_sweep = Arc::clone(&pool);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            let n = pool_sweep.evict_expired();
            if n > 0 { eprintln!("evicted {} expired objects", n); }
        }
    });
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

The health status includes the circuit breaker state. A pool with an open circuit breaker
is reported as unhealthy.

```rust
fn main() {
    let pool = objectpool::ObjectPool::new(vec![1, 2, 3], Default::default());
    let health = pool.get_health_status();
    println!("Healthy: {}", health.is_healthy);
    println!("CB open: {}", health.circuit_breaker_open);
    println!("Utilization: {:.1}%", health.utilization * 100.0);
    println!("Capacity: {}", pool.capacity());
    println!("Active: {}, Available: {}",
        health.active_objects, health.available_objects);
    for warning in &health.warnings {
        eprintln!("WARN: {}", warning);
    }
}
```

### Metrics Export

```rust
use objectpool::ObjectPool;
use std::collections::HashMap;

fn main() {
    let pool = ObjectPool::new(vec![1, 2, 3], Default::default());

    // Typed metrics struct
    let metrics = pool.get_metrics();
    println!("Retrieved: {}", metrics.total_retrieved);
    println!("Detached:  {}", metrics.total_detached);

    // Key-value map
    let map = pool.export_metrics();
    for (key, value) in &map {
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

`PooledObject<T>` implements `Deref<Target = T>`, `DerefMut`, `AsRef<T>`, and `AsMut<T>`,
so it works transparently wherever `&T` or `&mut T` are expected.
The following explicit accessor methods are also available:

| Method | Signature | Effect on pool |
|--------|-----------|----------------|
| `get()` | `&self -> &T` | None — object returned on drop |
| `get_mut()` | `&mut self -> &mut T` | None — object returned on drop |
| `into_detached()` | `self -> T` | **Permanently removes** from pool capacity |
| ~~`unwrap()`~~ | ~~`self -> T`~~ | *Deprecated since 1.1.0* — use `into_detached()` |

**Borrowing without removing from pool** — `get()` and `get_mut()` let you read or
mutate the value while the object remains tracked. The object is returned to the pool
when the `PooledObject` is dropped as normal:

```rust
use objectpool::ObjectPool;

fn main() {
    let pool = ObjectPool::new(vec![0], Default::default());
    let mut obj = pool.get_object().unwrap();

    *obj.get_mut() = 42;          // mutate in-place
    println!("{}", obj.get());    // borrow to read
    drop(obj);                    // returned to pool
    assert_eq!(pool.available_count(), 1);
}
```

**Permanent ownership transfer** — `into_detached()` consumes the `PooledObject`,
releases the active slot, and returns `T` directly. The object is **never** returned to
the pool; pool capacity is permanently reduced by one. Use this only when you
intentionally need to own `T` beyond the pool's lifetime:

```rust
use objectpool::ObjectPool;

fn main() {
    let pool = ObjectPool::new(vec![1], Default::default());
    let obj = pool.get_object().unwrap();

    let value: i32 = obj.into_detached(); // active slot freed; value detached permanently
    println!("{}", value);
    assert_eq!(pool.available_count(), 0); // capacity gone
}
```

### `ObjectPool<T>`

Fixed-size pool with pre-allocated objects. Passing `max_pool_size = 0` (or an empty
`Vec` with the default config's size capped to 0) panics at construction time with a
clear message.

**Methods:**
- `new(objects, config)` — Create pool with initial objects
- `get_object()` — Get object (non-blocking; returns `Err(PoolError::PoolEmpty)` if empty, or `Err(PoolError::CircuitBreakerOpen)` / `Err(PoolError::MaxActiveObjectsReached)` for operational guards). Marked `#[must_use]`.
- `try_get_object()` — Try to get object; returns `Ok(None)` **only** for an empty pool — operational errors (`CircuitBreakerOpen`, `MaxActiveObjectsReached`) are still returned as `Err`. Marked `#[must_use]`.
- `get_object_async()` — Async get with jittered retry (5–20 ms) and timeout; **non-retryable errors (`CircuitBreakerOpen`, `MaxActiveObjectsReached`) are returned immediately** without waiting for the timeout
- `try_get_object_async()` — Thin async wrapper around `try_get_object()`; performs a single non-blocking attempt (no polling loop, no timeout)
- `available_count()` — Number of objects currently available in the queue
- `active_count()` — Number of objects currently checked out
- `capacity()` — Maximum number of objects the pool can hold (set at construction time)
- `evict_expired()` — Proactively remove expired objects; returns count evicted (push-to-requeue failures are tracked separately in `queue_push_failures` and are **not** counted as evictions)
- `drain()` — Remove and return all available objects (for graceful shutdown)
- `get_health_status()` — Get health status (includes circuit breaker state)
- `get_metrics()` — Get typed `PoolMetrics` struct
- `export_metrics()` — Export metrics as `HashMap<String, String>`
- `export_metrics_prometheus()` — Export in Prometheus format

### `QueryableObjectPool<T>`

Pool that supports finding objects by predicate.

**Methods:**
- `new(objects, config)` — Create queryable pool
- `get_object(predicate)` — Find object matching predicate; O(n) worst case. `MaxActiveObjectsReached` is enforced atomically before the scan.
- `try_get_object(predicate)` — Returns `Ok(None)` only when no match is found; propagates operational errors as `Err`
- `get_object_async(predicate)` — Async find with jittered retry and timeout; non-retryable errors fail fast
- `capacity()` — Maximum pool size
- `available_count()` / `active_count()` — Observe pool state
- `get_metrics()` — Typed metrics struct
- `export_metrics()` / `export_metrics_prometheus()` — Metrics export
- `evict_expired()` / `drain()` — Eviction and shutdown helpers

### `DynamicObjectPool<T>`

Pool that creates objects on-demand using a factory function.

**Methods:**
- `new(factory, config)` — Create with factory function
- `with_initial(factory, objects, config)` — Create with initial objects and factory
- `get_object()` — Returns an available pooled object if one exists; calls the factory to create a new one **only** when the pool is empty *and* the active + available count is below `capacity`. Enforced with a `Mutex` (prevents TOCTOU over-creation) + CAS slot reservation (prevents `MaxActiveObjectsReached` race). `CircuitBreakerOpen` and `MaxActiveObjectsReached` are propagated immediately — the factory is **not** called.
- `try_get_object()` — Returns `Ok(None)` when pool is at capacity; propagates other errors
- `get_object_async()` — Async get with jittered retry and timeout
- `capacity()` — Maximum pool size
- `available_count()` / `active_count()` — Observe pool state
- `get_metrics()` — Typed metrics struct
- `export_metrics()` / `export_metrics_prometheus()` — Metrics export
- `evict_expired()` — Proactively remove expired objects
- `drain()` — Remove and return all available objects
- `warmup(count)` — Pre-populate pool (capped at pool capacity; eviction entries are cleaned up on push failure)
- `warmup_async(count)` — Async pre-population

### `PoolConfiguration<T>`

Configuration options for pool behavior.

**Builder Methods:**
- `with_max_pool_size(size)` — Set maximum pool capacity (must be ≥ 1)
- `with_max_active_objects(count)` — Limit concurrent checkouts (enforced with an atomic CAS semaphore)
- `with_validation(func)` — Enable validation on return
- `with_timeout(duration)` — Set async operation timeout
- `with_ttl(duration)` — Set time-to-live for objects
- `with_idle_timeout(duration)` — Set idle timeout
- `with_warmup(size)` — Set warm-up size
- `with_circuit_breaker(threshold, timeout)` — Enable circuit breaker

## Performance Characteristics

| Operation | Complexity | Implementation |
|-----------|-----------|----------------|
| `get_object()` | O(1) amortized | Lock-free `ArrayQueue` pop + CAS slot reservation |
| `return_object()` | O(1) | Lock-free `ArrayQueue` push + atomic decrement |
| `get_object(query)` | O(n) worst | Full scan with early-exit once match is found |
| `try_get_object()` | O(1) | Non-blocking variant with full error propagation |

## Thread Safety

All operations are thread-safe using:
- **`crossbeam::queue::ArrayQueue`** — Lock-free MPMC queue for the object inventory
- **`dashmap::DashMap`** — Concurrent hash map for eviction metadata
- **`std::sync::atomic::AtomicUsize`** (CAS loop) — Race-free active-slot semaphore; eliminates the TOCTOU window that existed with a read-then-increment pattern
- **`std::sync::Mutex`** — Serialises dynamic object creation in `DynamicObjectPool`

Tested under high concurrency with zero data races (verified by Rust's ownership system).

## Comparison with .NET Version

| Feature | .NET | Rust | Notes |
|---------|------|------|-------|
| Thread Safety | `ConcurrentStack` | `ArrayQueue` + atomics | Lock-free in both |
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
- `PooledObject::into_detached()` permanently removes the object from pool capacity; the active slot is freed but the object is never returned. Avoid in hot paths where maintaining pool size matters. (`unwrap()` is a deprecated alias for the same operation.)
- `try_get_object_async()` is a thin async wrapper around the synchronous `try_get_object()` — it performs one non-blocking attempt and returns immediately. It does **not** poll or apply a timeout.
- TTL/idle-timeout eviction is lazy (expired objects are filtered on checkout). For strict enforcement, call `evict_expired()` periodically from a background task.
- `QueryableObjectPool::get_object()` drains the entire queue to scan for a matching object, then refills it. Concurrent callers serialise on the lock-free queue, making it unsuitable for high-throughput concurrent use.
- When the return-to-pool queue push fails after retries (e.g. under extreme contention with a full queue), the object is discarded and the `queue_push_failures` metric is incremented. This permanently reduces pool capacity.
- `ObjectPool::new()` panics if the resolved capacity is 0 (i.e. empty `Vec` + `max_pool_size = 0`). Always provide at least one initial object or set `max_pool_size ≥ 1`.
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

### 1.1.1 - May 2026
- **`#[must_use]`** on all pool query / observability methods — calling `get_object`, `try_get_object`, `get_metrics`, `get_health_status`, `available_count`, `active_count`, `capacity`, `evict_expired`, `drain` and variants without using the result now produces a compiler warning
- **`capacity()` method** added on `ObjectPool`, `QueryableObjectPool`, and `DynamicObjectPool`
- **`get_metrics()` delegated** from `QueryableObjectPool` and `DynamicObjectPool` (previously only accessible via the string-map `export_metrics()`)
- **`AsRef<T>` / `AsMut<T>`** implemented for `PooledObject<T>` — works with any API accepting `impl AsRef<T>`
- **`max_active_objects` TOCTOU race fixed** — replaced `DashMap.len() >= max` with a CAS-loop atomic semaphore; check and increment are now one atomic operation
- **`warmup` / `warmup_async` eviction leak fixed** — eviction tracker entries are now cleaned up if the queue push fails
- **`evict_expired` miscount fixed** — objects lost to a requeue push failure are no longer counted as evictions; they increment `queue_push_failures` only
- **`HealthTracker` removed** — the struct was populated on every get/return but its data was never read; its `Arc` and atomic overhead are gone
- **Zero-capacity guard** — `ObjectPool::new()` panics with a clear message when the resolved capacity is 0
- **Async retry jitter** — `get_object_async` on all three pool types now uses a 5–20 ms staggered delay instead of a fixed 10 ms, reducing thundering-herd wake-ups
- **Silent `let _` in constructor replaced** with a `panic!` that includes the object index (was always unreachable but now explicit)

### 1.1.0 - May 2026
- **`PooledObject::get()` / `get_mut()`** — explicit borrow accessors that do not affect pool state; the object is still returned on drop
- **`PooledObject::into_detached()`** — explicit "permanent detach" API replacing the old `unwrap()` behaviour; makes ownership transfer intent clear at the call site
- **`PooledObject::unwrap()` deprecated** — now an alias for `into_detached()`; emits a compiler warning directing to the new name
- **`total_detached` metric** — tracks how often objects are permanently removed via `into_detached()`; exposed in `PoolMetrics`, `export_metrics()` map, and Prometheus output (`objectpool_objects_detached_total`)

### 1.0.0 - April 2026 (initial release)
- **Initial crates.io release** (Rust port of .NET EsoxSolutions.ObjectPool v4.0.0)
- Thread-safe pooling with lock-free operations (`crossbeam::ArrayQueue` + `dashmap`)
- Async support with tokio (`get_object_async`, `warmup_async`)
- Health monitoring with circuit-breaker state in `HealthStatus`
- Prometheus-format metrics export
- Proactive eviction via `evict_expired()` + lazy eviction on checkout
- Graceful-shutdown `drain()` on all pool types
- Circuit breaker (Closed → Open → Half-Open) with consecutive-failure counting
- Pool warm-up / pre-population
- Queryable and dynamic pools with full observability (`available_count`, `active_count`)
- `DynamicObjectPool`: TOCTOU-safe capacity enforcement; CB failure offset on successful creation
- Requires Rust 1.88+

## Architecture Differences

### .NET → Rust Mappings

```
.NET                          →  Rust
─────────────────────────────────────────────────────────
IDisposable                   →  Drop trait
ConcurrentStack<T>            →  ArrayQueue<T> (lock-free MPMC)
Task<T>                       →  Future with tokio
lock { }                      →  Mutex/RwLock (RAII locks)
Interlocked.Increment         →  AtomicUsize::fetch_add / CAS
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
