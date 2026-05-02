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

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    // ── ObjectMetadata ────────────────────────────────────────────────────────

    #[test]
    fn none_policy_never_expires() {
        let meta = ObjectMetadata::new();
        assert!(!meta.is_expired(&EvictionPolicy::None));
        thread::sleep(Duration::from_millis(10));
        assert!(!meta.is_expired(&EvictionPolicy::None));
    }

    #[test]
    fn ttl_policy_not_yet_expired() {
        let meta = ObjectMetadata::new();
        assert!(!meta.is_expired(&EvictionPolicy::TimeToLive(Duration::from_secs(60))));
    }

    #[test]
    fn ttl_policy_expires_after_duration() {
        let meta = ObjectMetadata::new();
        thread::sleep(Duration::from_millis(30));
        assert!(meta.is_expired(&EvictionPolicy::TimeToLive(Duration::from_millis(20))));
    }

    #[test]
    fn idle_timeout_not_yet_expired() {
        let meta = ObjectMetadata::new();
        assert!(!meta.is_expired(&EvictionPolicy::IdleTimeout(Duration::from_secs(60))));
    }

    #[test]
    fn idle_timeout_expires_after_idle() {
        let meta = ObjectMetadata::new();
        thread::sleep(Duration::from_millis(30));
        assert!(meta.is_expired(&EvictionPolicy::IdleTimeout(Duration::from_millis(20))));
    }

    #[test]
    fn idle_timeout_stays_fresh_after_touch() {
        let mut meta = ObjectMetadata::new();
        thread::sleep(Duration::from_millis(30));
        meta.touch(); // reset last_used
        // Should no longer be expired under a 50 ms idle policy.
        assert!(!meta.is_expired(&EvictionPolicy::IdleTimeout(Duration::from_millis(50))));
    }

    #[test]
    fn combined_expires_on_ttl() {
        let meta = ObjectMetadata::new();
        thread::sleep(Duration::from_millis(30));
        // TTL expired, idle not yet expired.
        assert!(meta.is_expired(&EvictionPolicy::Combined {
            ttl: Duration::from_millis(20),
            idle_timeout: Duration::from_secs(60),
        }));
    }

    #[test]
    fn combined_expires_on_idle() {
        let meta = ObjectMetadata::new();
        thread::sleep(Duration::from_millis(30));
        // Idle expired, TTL not yet expired.
        assert!(meta.is_expired(&EvictionPolicy::Combined {
            ttl: Duration::from_secs(60),
            idle_timeout: Duration::from_millis(20),
        }));
    }

    #[test]
    fn combined_not_expired_when_both_fresh() {
        let meta = ObjectMetadata::new();
        assert!(!meta.is_expired(&EvictionPolicy::Combined {
            ttl: Duration::from_secs(60),
            idle_timeout: Duration::from_secs(60),
        }));
    }

    // ── EvictionTracker ───────────────────────────────────────────────────────

    #[test]
    fn tracker_none_policy_skips_metadata_and_never_expires() {
        let tracker = EvictionTracker::<i32>::new(EvictionPolicy::None);
        tracker.track_object(1);
        tracker.touch_object(1); // should be a no-op without panic
        assert!(!tracker.is_expired(1));
    }

    #[test]
    fn tracker_ttl_tracks_and_expires() {
        let tracker = EvictionTracker::<i32>::new(EvictionPolicy::TimeToLive(Duration::from_millis(20)));
        tracker.track_object(42);
        assert!(!tracker.is_expired(42));

        thread::sleep(Duration::from_millis(30));
        assert!(tracker.is_expired(42));
    }

    #[test]
    fn tracker_remove_clears_expiry_check() {
        let tracker = EvictionTracker::<i32>::new(EvictionPolicy::TimeToLive(Duration::from_millis(20)));
        tracker.track_object(7);
        thread::sleep(Duration::from_millis(30));
        assert!(tracker.is_expired(7));

        tracker.remove_object(7);
        // After removal, unknown id → not expired.
        assert!(!tracker.is_expired(7));
    }

    #[test]
    fn tracker_touch_resets_idle_timer() {
        let tracker = EvictionTracker::<i32>::new(EvictionPolicy::IdleTimeout(Duration::from_millis(50)));
        tracker.track_object(3);
        thread::sleep(Duration::from_millis(30));
        tracker.touch_object(3); // reset last_used
        // Should not be expired yet.
        assert!(!tracker.is_expired(3));
    }

    #[test]
    fn tracker_get_expired_returns_expired_ids() {
        let tracker = EvictionTracker::<i32>::new(EvictionPolicy::TimeToLive(Duration::from_millis(20)));
        tracker.track_object(1);
        tracker.track_object(2);
        tracker.track_object(3);

        thread::sleep(Duration::from_millis(30));

        let mut expired = tracker.get_expired_objects();
        expired.sort();
        assert_eq!(expired, vec![1, 2, 3]);
    }

    #[test]
    fn tracker_get_expired_none_policy_returns_empty() {
        let tracker = EvictionTracker::<i32>::new(EvictionPolicy::None);
        tracker.track_object(1);
        assert!(tracker.get_expired_objects().is_empty());
    }

    #[test]
    fn tracker_unknown_id_is_not_expired() {
        let tracker = EvictionTracker::<i32>::new(EvictionPolicy::TimeToLive(Duration::from_millis(1)));
        // id 99 was never tracked
        assert!(!tracker.is_expired(99));
    }
}

