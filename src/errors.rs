//! Error types for the object pool
//!
//! # Examples
//!
//! ```
//! use esox_objectpool::{ObjectPool, PoolConfiguration, PoolError};
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn error_display_messages() {
        assert_eq!(PoolError::PoolEmpty.to_string(), "Pool is empty - no objects available");
        assert_eq!(PoolError::PoolFull.to_string(), "Pool is at maximum capacity");
        assert_eq!(PoolError::NoMatchFound.to_string(), "No object matching the query was found");
        assert_eq!(PoolError::ValidationFailed.to_string(), "Object validation failed");
        assert_eq!(PoolError::CircuitBreakerOpen.to_string(), "Circuit breaker is open - too many failures");
        assert_eq!(PoolError::MaxActiveObjectsReached.to_string(), "Maximum active objects limit reached");
        assert_eq!(PoolError::Cancelled.to_string(), "Operation was cancelled");
    }

    #[test]
    fn timeout_display_includes_duration() {
        let msg = PoolError::Timeout(Duration::from_secs(30)).to_string();
        assert!(msg.contains("30s") || msg.contains("30"), "expected duration in: {msg}");
    }

    #[test]
    fn errors_are_clone() {
        let e = PoolError::PoolEmpty;
        let cloned = e.clone();
        assert_eq!(e.to_string(), cloned.to_string());
    }

    #[test]
    fn errors_are_debug() {
        let cases: &[PoolError] = &[
            PoolError::PoolEmpty,
            PoolError::PoolFull,
            PoolError::Timeout(Duration::from_millis(100)),
            PoolError::NoMatchFound,
            PoolError::ValidationFailed,
            PoolError::CircuitBreakerOpen,
            PoolError::MaxActiveObjectsReached,
            PoolError::Cancelled,
        ];
        for e in cases {
            assert!(!format!("{e:?}").is_empty());
        }
    }
}

