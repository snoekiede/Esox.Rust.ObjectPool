//! # EsoxSolutions.ObjectPool (Rust Port)
//!
//! High-performance, thread-safe object pool for Rust with async support,
//! metrics, health monitoring, and advanced features.
//!
//! ## Features
//!
//! - Thread-safe object pooling with lock-free operations
//! - Automatic return of objects via RAII (Drop trait)
//! - Async support with timeout and cancellation
//! - Queryable pools for finding objects matching predicates
//! - Dynamic pools with factory methods
//! - Health monitoring and metrics
//! - Prometheus metrics export
//! - Pool warm-up/pre-population
//! - Eviction/TTL support
//! - Circuit breaker pattern
//! - Lifecycle hooks
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
//! ```

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
