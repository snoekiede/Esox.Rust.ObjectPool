//! Error types for the object pool
//!
//! # Examples
//!
//! ```
//! use objectpool::{ObjectPool, PoolConfiguration, PoolError};
//!
//! let pool = ObjectPool::new(vec![1], PoolConfiguration::default());
//! let _obj = pool.get_object().unwrap();
//!
//! // Pool is now empty
//! let result = pool.get_object();
//! assert!(matches!(result, Err(PoolError::PoolEmpty)));
//! ```

use thiserror::Error;

#[derive(Error, Debug, Clone)]
pub enum PoolError {
    #[error("Pool is empty - no objects available")]
    PoolEmpty,
    
    #[error("Pool is at maximum capacity")]
    PoolFull,
    
    #[error("Operation timed out after {0:?}")]
    Timeout(std::time::Duration),
    
    #[error("No object matching the query was found")]
    NoMatchFound,
    
    #[error("Object validation failed")]
    ValidationFailed,
    
    #[error("Circuit breaker is open - too many failures")]
    CircuitBreakerOpen,
    
    #[error("Maximum active objects limit reached")]
    MaxActiveObjectsReached,
    
    #[error("Operation was cancelled")]
    Cancelled,
}

pub type PoolResult<T> = Result<T, PoolError>;
