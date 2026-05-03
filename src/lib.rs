//! # EsoxSolutions.ObjectPool (Rust Port)
//!
//! High-performance, thread-safe object pool for Rust with async support,
//! metrics, health monitoring, and advanced features.
//!
//! ## Features
//!
//! - Thread-safe object pooling with lock-free operations
//! - Automatic return of objects via RAII ([`Drop`] trait)
//! - Async support with timeout and jittered retry
//! - Queryable pools for finding objects matching predicates
//! - Dynamic pools with factory methods
//! - Health monitoring and metrics (including Prometheus export)
//! - Pool warm-up/pre-population
//! - Eviction/TTL support
//! - Circuit breaker pattern
//! - [`#[must_use]`](must_use) on all observability methods
//!
//! ## Quick Start
//!
//! ```rust
//! use esox_objectpool::ObjectPool;
//!
//! let pool = ObjectPool::new(vec![1, 2, 3], Default::default());
//! {
//!     let obj = pool.get_object().unwrap();
//!     println!("Got: {}", *obj);
//!     // Object automatically returned when `obj` goes out of scope
//! }
//! println!("capacity={} available={}", pool.capacity(), pool.available_count());
//! ```
//!
//! ## Working with `PooledObject<T>`
//!
//! [`PooledObject<T>`](PooledObject) is a smart pointer that returns its contents to the
//! pool when dropped. It implements [`Deref`](std::ops::Deref),
//! [`DerefMut`](std::ops::DerefMut), [`AsRef<T>`](AsRef), and [`AsMut<T>`](AsMut) so it
//! works transparently wherever `&T` or `&mut T` are expected.
//!
//! Three explicit accessor methods are available:
//!
//! | Method | Returns | Pool effect |
//! |--------|---------|-------------|
//! | [`get()`](PooledObject::get) | `&T` | None — returned on drop |
//! | [`get_mut()`](PooledObject::get_mut) | `&mut T` | None — returned on drop |
//! | [`into_detached()`](PooledObject::into_detached) | `T` (owned) | **Permanently removed** |
//!
//! ```rust
//! use esox_objectpool::ObjectPool;
//!
//! let pool = ObjectPool::new(vec![0], Default::default());
//! let mut obj = pool.get_object().unwrap();
//!
//! *obj.get_mut() = 99;           // mutate — stays in pool
//! assert_eq!(*obj.get(), 99);    // borrow — stays in pool
//! drop(obj);                     // returned to pool as normal
//!
//! // Permanently take ownership (reduces pool capacity by 1):
//! let obj2 = pool.get_object().unwrap();
//! let value = obj2.into_detached();
//! assert_eq!(value, 99);
//! ```
//!
//! > **Note:** `unwrap()` on `PooledObject` is deprecated since 1.1.0.
//! > Use [`into_detached()`](PooledObject::into_detached) instead.
//!
//! ## Observing Pool State
//!
//! All three pool types expose the same set of observability methods:
//!
//! ```rust
//! use esox_objectpool::{ObjectPool, PoolConfiguration};
//!
//! let pool = ObjectPool::new(vec![1, 2, 3],
//!     PoolConfiguration::new().with_max_pool_size(10));
//!
//! println!("capacity={}", pool.capacity());          // 10
//! println!("available={}", pool.available_count());  // 3
//! println!("active={}", pool.active_count());        // 0
//!
//! let metrics = pool.get_metrics();
//! println!("retrieved={}", metrics.total_retrieved);
//!
//! let health = pool.get_health_status();
//! println!("healthy={} utilization={:.0}%",
//!     health.is_healthy, health.utilization * 100.0);
//! ```
//!
//! ## API Changes in 1.1.1
//!
//! - `ObjectPool` and `PooledObject` now implement `AsRef<T>` and `AsMut<T>`.
//! - `ObjectPool::capacity()` method added.
//! - Jitter support in async operations.
//! - All observability methods are annotated with `#[must_use]`.
//! - `unwrap()` on `PooledObject` is deprecated, use `into_detached()` instead.

mod pool;
mod config;
mod metrics;
mod health;
mod eviction;
mod circuit_breaker;
mod errors;

pub use pool::{ObjectPool, QueryableObjectPool, DynamicObjectPool, PooledObject};
pub use config::PoolConfiguration;
pub use metrics::{PoolMetrics, MetricsExporter};
pub use health::HealthStatus;
pub use eviction::EvictionPolicy;
pub use circuit_breaker::{CircuitBreaker, CircuitBreakerState};
pub use errors::{PoolError, PoolResult};
