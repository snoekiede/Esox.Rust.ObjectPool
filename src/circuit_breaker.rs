//! Circuit breaker pattern implementation

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Circuit breaker state
///
/// # Examples
///
/// ```
/// use esox_objectpool::{CircuitBreaker, CircuitBreakerState};
/// use std::time::Duration;
///
/// let breaker = CircuitBreaker::new(3, Duration::from_secs(60));
/// assert_eq!(breaker.state(), CircuitBreakerState::Closed);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitBreakerState {
    /// Circuit is closed - normal operation
    Closed,
    
    /// Circuit is open - failing fast
    Open,
    
    /// Circuit is half-open - testing recovery
    HalfOpen,
}

/// Circuit breaker for protecting against cascading failures
///
/// # Examples
///
/// ```
/// use esox_objectpool::CircuitBreaker;
/// use std::time::Duration;
///
/// let breaker = CircuitBreaker::new(3, Duration::from_secs(60));
///
/// // Record failures
/// breaker.record_failure();
/// breaker.record_failure();
/// breaker.record_failure();
///
/// // Circuit should be open after threshold
/// assert!(!breaker.allow_request());
/// ```
pub struct CircuitBreaker {
    state: Arc<Mutex<CircuitBreakerState>>,
    failure_count: Arc<AtomicUsize>,
    success_count: Arc<AtomicUsize>,
    failure_threshold: usize,
    timeout: Duration,
    last_failure_time: Arc<Mutex<Option<Instant>>>,
}

impl CircuitBreaker {
    /// Create a new circuit breaker
    pub fn new(failure_threshold: usize, timeout: Duration) -> Self {
        Self {
            state: Arc::new(Mutex::new(CircuitBreakerState::Closed)),
            failure_count: Arc::new(AtomicUsize::new(0)),
            success_count: Arc::new(AtomicUsize::new(0)),
            failure_threshold,
            timeout,
            last_failure_time: Arc::new(Mutex::new(None)),
        }
    }
    
    /// Get the current state
    pub fn state(&self) -> CircuitBreakerState {
        *self.state.lock().unwrap()
    }
    
    /// Check if the circuit breaker allows the operation
    pub fn allow_request(&self) -> bool {
        let current_state = self.state();
        
        match current_state {
            CircuitBreakerState::Closed => true,
            CircuitBreakerState::Open => {
                // Check if timeout has elapsed
                let last_failure = self.last_failure_time.lock().unwrap();
                if let Some(time) = *last_failure
                    && time.elapsed() > self.timeout
                {
                    drop(last_failure);
                    self.transition_to_half_open();
                    return true;
                }
                false
            }
            CircuitBreakerState::HalfOpen => true,
        }
    }
    
    /// Record a successful operation
    pub fn record_success(&self) {
        self.success_count.fetch_add(1, Ordering::Relaxed);
        
        let current_state = self.state();
        if current_state == CircuitBreakerState::HalfOpen {
            // After a few successes in half-open, close the circuit
            if self.success_count.load(Ordering::Relaxed) >= 3 {
                self.transition_to_closed();
            }
        }
    }
    
    /// Record a failed operation
    pub fn record_failure(&self) {
        let count = self.failure_count.fetch_add(1, Ordering::Relaxed) + 1;
        *self.last_failure_time.lock().unwrap() = Some(Instant::now());
        
        let current_state = self.state();
        match current_state {
            CircuitBreakerState::Closed => {
                if count >= self.failure_threshold {
                    self.transition_to_open();
                }
            }
            CircuitBreakerState::HalfOpen => {
                // Any failure in half-open immediately opens the circuit
                self.transition_to_open();
            }
            CircuitBreakerState::Open => {}
        }
    }
    
    fn transition_to_open(&self) {
        *self.state.lock().unwrap() = CircuitBreakerState::Open;
    }
    
    fn transition_to_half_open(&self) {
        *self.state.lock().unwrap() = CircuitBreakerState::HalfOpen;
        self.success_count.store(0, Ordering::Relaxed);
    }
    
    fn transition_to_closed(&self) {
        *self.state.lock().unwrap() = CircuitBreakerState::Closed;
        self.failure_count.store(0, Ordering::Relaxed);
        self.success_count.store(0, Ordering::Relaxed);
    }
    
    /// Reset the circuit breaker
    pub fn reset(&self) {
        self.transition_to_closed();
    }
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self::new(5, Duration::from_secs(60))
    }
}
