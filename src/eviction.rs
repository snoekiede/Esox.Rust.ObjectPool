//! Eviction policies for automatic object removal

use std::time::{Duration, Instant};
use std::sync::{Arc, Mutex};
use std::collections::HashMap;

/// Eviction policy for pool objects
///
/// # Examples
///
/// ```
/// use objectpool::{ObjectPool, PoolConfiguration};
/// use std::time::Duration;
///
/// // Create pool with TTL eviction
/// let config = PoolConfiguration::new()
///     .with_ttl(Duration::from_secs(3600));
///
/// let pool = ObjectPool::new(vec![1, 2, 3], config);
/// // Objects will be evicted after 1 hour
/// ```
#[derive(Debug, Clone, Default)]
pub enum EvictionPolicy {
    /// No eviction
    #[default]
    None,
    
    /// Time-to-live: objects expire after a fixed duration
    TimeToLive(Duration),
    
    /// Idle timeout: objects expire after being idle
    IdleTimeout(Duration),
    
    /// Combined: TTL or idle timeout
    Combined {
        ttl: Duration,
        idle_timeout: Duration,
    },
}

/// Metadata for tracking object lifecycle
#[derive(Debug, Clone)]
pub(crate) struct ObjectMetadata {
    pub created_at: Instant,
    pub last_used: Instant,
}

impl ObjectMetadata {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            created_at: now,
            last_used: now,
        }
    }
    
    pub fn touch(&mut self) {
        self.last_used = Instant::now();
    }
    
    pub fn is_expired(&self, policy: &EvictionPolicy) -> bool {
        match policy {
            EvictionPolicy::None => false,
            EvictionPolicy::TimeToLive(ttl) => {
                self.created_at.elapsed() > *ttl
            }
            EvictionPolicy::IdleTimeout(timeout) => {
                self.last_used.elapsed() > *timeout
            }
            EvictionPolicy::Combined { ttl, idle_timeout } => {
                self.created_at.elapsed() > *ttl || self.last_used.elapsed() > *idle_timeout
            }
        }
    }
}

/// Tracker for object metadata
pub(crate) struct EvictionTracker<T> {
    metadata: Arc<Mutex<HashMap<usize, ObjectMetadata>>>,
    policy: EvictionPolicy,
    _phantom: std::marker::PhantomData<T>,
}

impl<T> EvictionTracker<T> {
    pub fn new(policy: EvictionPolicy) -> Self {
        Self {
            metadata: Arc::new(Mutex::new(HashMap::new())),
            policy,
            _phantom: std::marker::PhantomData,
        }
    }
    
    pub fn track_object(&self, id: usize) {
        if !matches!(self.policy, EvictionPolicy::None) {
            let mut metadata = self.metadata.lock().unwrap();
            metadata.insert(id, ObjectMetadata::new());
        }
    }
    
    pub fn touch_object(&self, id: usize) {
        if !matches!(self.policy, EvictionPolicy::None) {
            let mut metadata = self.metadata.lock().unwrap();
            if let Some(meta) = metadata.get_mut(&id) {
                meta.touch();
            }
        }
    }
    
    pub fn is_expired(&self, id: usize) -> bool {
        if matches!(self.policy, EvictionPolicy::None) {
            return false;
        }
        
        let metadata = self.metadata.lock().unwrap();
        if let Some(meta) = metadata.get(&id) {
            meta.is_expired(&self.policy)
        } else {
            false
        }
    }
    
    pub fn remove_object(&self, id: usize) {
        let mut metadata = self.metadata.lock().unwrap();
        metadata.remove(&id);
    }
    
    #[allow(dead_code)]
    pub fn get_expired_objects(&self) -> Vec<usize> {
        if matches!(self.policy, EvictionPolicy::None) {
            return Vec::new();
        }
        
        let metadata = self.metadata.lock().unwrap();
        metadata
            .iter()
            .filter(|(_, meta)| meta.is_expired(&self.policy))
            .map(|(id, _)| *id)
            .collect()
    }
}
