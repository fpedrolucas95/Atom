// cache/lru.rs — Generic LRU cache with write-back eviction
//
// Used by FatCache and ClusterCache.  When an entry must be evicted and it
// is dirty, the caller-supplied `on_evict` callback is invoked so the
// caller can flush to the block device.
//
// Key = K (u64 sector LBA for FatCache, u32 cluster number for ClusterCache)
// Value = V (byte buffer)

#![allow(dead_code)]

extern crate alloc;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;

/// A single entry in the LRU cache.
pub struct LruEntry<V> {
    pub data:     V,
    pub dirty:    bool,
    /// Logical access tick (higher = more recently used).
    access_tick: u64,
}

/// Generic LRU cache.
///
/// - `capacity`: maximum number of entries.  When full, the least-recently-used
///   entry is evicted (write-back via `on_evict` if dirty).
/// - `on_evict`: called with `(key, &V, dirty)` before the entry is dropped.
///   MUST write the entry to stable storage if `dirty == true`.
pub struct LruCache<K, V>
where
    K: Ord + Copy,
{
    entries:  BTreeMap<K, LruEntry<V>>,
    capacity: usize,
    tick:     u64,
}

impl<K, V> LruCache<K, V>
where
    K: Ord + Copy,
{
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "LruCache capacity must be > 0");
        LruCache {
            entries:  BTreeMap::new(),
            capacity,
            tick:     0,
        }
    }

    // ── Accessors ───────────────────────────────────────────────────────────

    pub fn get(&mut self, key: K) -> Option<&V> {
        let tick = self.next_tick();
        let entry = self.entries.get_mut(&key)?;
        entry.access_tick = tick;
        Some(&entry.data)
    }

    pub fn get_mut(&mut self, key: K) -> Option<&mut LruEntry<V>> {
        let tick = self.next_tick();
        let entry = self.entries.get_mut(&key)?;
        entry.access_tick = tick;
        Some(entry)
    }

    pub fn contains(&self, key: K) -> bool {
        self.entries.contains_key(&key)
    }

    // ── Insertion ────────────────────────────────────────────────────────────

    /// Insert `key` → `value`.  If an eviction is needed, the evicted key +
    /// dirty flag are returned so the caller can flush the data.
    ///
    /// Returns `Some((evicted_key, was_dirty))` if an entry was evicted.
    pub fn insert(&mut self, key: K, value: V, dirty: bool)
        -> Option<(K, bool)>
    {
        // If already present: just update
        if self.entries.contains_key(&key) {
            let tick = self.next_tick();
            let e = self.entries.get_mut(&key).unwrap();
            e.data        = value;
            e.dirty       = e.dirty || dirty;
            e.access_tick = tick;
            return None;
        }

        let evicted = if self.entries.len() >= self.capacity {
            self.evict_lru()
        } else {
            None
        };

        let tick = self.next_tick();
        self.entries.insert(key, LruEntry { data: value, dirty, access_tick: tick });
        evicted
    }

    /// Mark an existing entry as dirty (without changing its data).
    pub fn mark_dirty(&mut self, key: K) {
        if let Some(e) = self.entries.get_mut(&key) {
            e.dirty = true;
        }
    }

    /// Mark an existing entry as clean (after successful flush).
    pub fn mark_clean(&mut self, key: K) {
        if let Some(e) = self.entries.get_mut(&key) {
            e.dirty = false;
        }
    }

    // ── Dirty enumeration ──────────────────────────────────────────────────

    /// Return keys of all dirty entries (order unspecified).
    pub fn dirty_keys(&self) -> Vec<K> {
        self.entries.iter()
            .filter(|(_, e)| e.dirty)
            .map(|(k, _)| *k)
            .collect()
    }

    pub fn iter_dirty(&self) -> impl Iterator<Item = (K, &V)> {
        self.entries.iter()
            .filter(|(_, e)| e.dirty)
            .map(|(k, e)| (*k, &e.data))
    }

    pub fn len(&self) -> usize { self.entries.len() }
    pub fn is_empty(&self) -> bool { self.entries.is_empty() }

    // ── Private ─────────────────────────────────────────────────────────────

    fn next_tick(&mut self) -> u64 {
        self.tick = self.tick.wrapping_add(1);
        self.tick
    }

    /// Find and remove the LRU entry.  Returns (key, was_dirty).
    fn evict_lru(&mut self) -> Option<(K, bool)> {
        let lru_key = self.entries.iter()
            .min_by_key(|(_, e)| e.access_tick)
            .map(|(k, _)| *k)?;
        let evicted = self.entries.remove(&lru_key)?;
        Some((lru_key, evicted.dirty))
    }
}
