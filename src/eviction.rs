//! Eviction policies for automatic object removal

use dashmap::DashMap;
use std::time::{Duration, Instant};

/// Eviction policy for pool objects
///
/// # Examples
///
/// ```
/// use esox_objectpool::{ObjectPool, PoolConfiguration};
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
    metadata: DashMap<usize, ObjectMetadata>,
    policy: EvictionPolicy,
    _phantom: std::marker::PhantomData<T>,
}

impl<T> EvictionTracker<T> {
    pub fn new(policy: EvictionPolicy) -> Self {
        Self {
            metadata: DashMap::new(),
            policy,
            _phantom: std::marker::PhantomData,
        }
    }

    pub fn track_object(&self, id: usize) {
        if !matches!(self.policy, EvictionPolicy::None) {
            self.metadata.insert(id, ObjectMetadata::new());
        }
    }

    pub fn touch_object(&self, id: usize) {
        if !matches!(self.policy, EvictionPolicy::None) {
            if let Some(mut meta) = self.metadata.get_mut(&id) {
                meta.touch();
            }
        }
    }

    pub fn is_expired(&self, id: usize) -> bool {
        if matches!(self.policy, EvictionPolicy::None) {
            return false;
        }
        self.metadata
            .get(&id)
            .map_or(false, |meta| meta.is_expired(&self.policy))
    }

    pub fn remove_object(&self, id: usize) {
        self.metadata.remove(&id);
    }

    /// Returns the IDs of all currently expired objects. Useful for inspection;
    /// for actual removal use [`ObjectPool::evict_expired`].
    #[allow(dead_code)]
    pub fn get_expired_objects(&self) -> Vec<usize> {
        if matches!(self.policy, EvictionPolicy::None) {
            return Vec::new();
        }
        self.metadata
            .iter()
            .filter(|entry| entry.value().is_expired(&self.policy))
            .map(|entry| *entry.key())
            .collect()
    }
}
