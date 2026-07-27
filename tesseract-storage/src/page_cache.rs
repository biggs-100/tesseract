// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

use lru::LruCache;
use std::num::NonZeroUsize;
use std::sync::Mutex;

use tesseract_common::error::{Error, Result};

/// Key for identifying a cached cold-tier page.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct PageKey {
    pub partition_id: u64,
    pub page_index: u32,
}

/// A page of data from the cold tier.
#[derive(Debug, Clone)]
pub struct Page {
    pub data: Vec<u8>,
    pub size: usize,
}

/// LRU page cache for cold tier reads.
///
/// Wraps `LruCache` in a `Mutex` for thread safety.
/// `get` promotes the entry (LRU semantics). `insert` may evict the
/// least recently used entry when the cache is at capacity.
#[derive(Debug)]
pub struct PageCache {
    inner: Mutex<LruCache<PageKey, Page>>,
}

impl PageCache {
    /// Create a new page cache with the given capacity (in number of pages).
    ///
    /// # Errors
    ///
    /// Returns `Err(Error::InvalidConfig(...))` if `capacity` is 0.
    pub fn new(capacity: usize) -> Result<Self> {
        let cap = NonZeroUsize::new(capacity).ok_or_else(|| {
            Error::InvalidConfig(format!("PageCache capacity must be > 0, got {capacity}"))
        })?;
        Ok(Self { inner: Mutex::new(LruCache::new(cap)) })
    }

    /// Retrieve a page by key.
    ///
    /// Promotes the entry in the LRU order (makes it most recently used).
    /// Returns `None` if the key is not in the cache.
    pub fn get(&self, key: &PageKey) -> Result<Option<Page>> {
        let mut cache = self.inner.lock().map_err(|e| Error::LockPoisoned(e.to_string()))?;
        Ok(cache.get(key).cloned())
    }

    /// Insert a page into the cache.
    ///
    /// If the cache is at capacity, the least recently used entry is evicted.
    pub fn insert(&self, key: PageKey, page: Page) -> Result<()> {
        let mut cache = self.inner.lock().map_err(|e| Error::LockPoisoned(e.to_string()))?;
        cache.put(key, page);
        Ok(())
    }

    /// Remove a page from the cache. Returns `Some(Page)` if it existed.
    pub fn remove(&self, key: &PageKey) -> Result<Option<Page>> {
        let mut cache = self.inner.lock().map_err(|e| Error::LockPoisoned(e.to_string()))?;
        Ok(cache.pop(key))
    }

    /// Clear all entries from the cache.
    pub fn clear(&self) -> Result<()> {
        let mut cache = self.inner.lock().map_err(|e| Error::LockPoisoned(e.to_string()))?;
        cache.clear();
        Ok(())
    }

    /// Number of pages currently in the cache.
    pub fn len(&self) -> Result<usize> {
        let cache = self.inner.lock().map_err(|e| Error::LockPoisoned(e.to_string()))?;
        Ok(cache.len())
    }

    /// Returns `true` if the cache is empty.
    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn page_a() -> (PageKey, Page) {
        (PageKey { partition_id: 0, page_index: 0 }, Page { data: vec![1u8; 64], size: 64 })
    }

    fn page_b() -> (PageKey, Page) {
        (PageKey { partition_id: 0, page_index: 1 }, Page { data: vec![2u8; 64], size: 64 })
    }

    fn page_c() -> (PageKey, Page) {
        (PageKey { partition_id: 0, page_index: 2 }, Page { data: vec![3u8; 64], size: 64 })
    }

    fn page_d() -> (PageKey, Page) {
        (PageKey { partition_id: 0, page_index: 3 }, Page { data: vec![4u8; 64], size: 64 })
    }

    #[test]
    fn insert_and_get() {
        let cache = PageCache::new(10).unwrap();
        let (key, page) = page_a();
        cache.insert(key.clone(), page.clone()).unwrap();

        let result = cache.get(&key).unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().size, page.size);
    }

    #[test]
    fn get_non_existent_returns_none() {
        let cache = PageCache::new(10).unwrap();
        let key = PageKey { partition_id: 99, page_index: 0 };
        assert!(cache.get(&key).unwrap().is_none());
    }

    #[test]
    fn eviction_removes_lru_entry() {
        let cache = PageCache::new(3).unwrap();

        let (k_a, p_a) = page_a();
        let (k_b, p_b) = page_b();
        let (k_c, p_c) = page_c();
        let (k_d, p_d) = page_d();

        cache.insert(k_a.clone(), p_a).unwrap();
        cache.insert(k_b.clone(), p_b).unwrap();
        cache.insert(k_c.clone(), p_c).unwrap();

        // Access A to promote it — makes B the LRU
        let _ = cache.get(&k_a);

        // Insert D — should evict B (the LRU)
        cache.insert(k_d.clone(), p_d).unwrap();

        assert!(cache.get(&k_a).unwrap().is_some(), "A should survive (was promoted)");
        assert!(cache.get(&k_b).unwrap().is_none(), "B should be evicted (LRU)");
        assert!(cache.get(&k_c).unwrap().is_some(), "C should survive");
        assert!(cache.get(&k_d).unwrap().is_some(), "D should be present");
    }

    #[test]
    fn get_promotes_entry() {
        let cache = PageCache::new(3).unwrap();

        let (k_a, p_a) = page_a();
        let (k_b, p_b) = page_b();
        let (k_c, p_c) = page_c();
        let (k_d, p_d) = page_d();

        cache.insert(k_a.clone(), p_a).unwrap();
        cache.insert(k_b.clone(), p_b).unwrap();
        cache.insert(k_c.clone(), p_c).unwrap();

        // Access A then B — C becomes the LRU
        let _ = cache.get(&k_a);
        let _ = cache.get(&k_b);

        // Insert D — should evict C
        cache.insert(k_d.clone(), p_d).unwrap();

        assert!(cache.get(&k_a).unwrap().is_some(), "A should survive");
        assert!(cache.get(&k_b).unwrap().is_some(), "B should survive (was promoted)");
        assert!(cache.get(&k_c).unwrap().is_none(), "C should be evicted (LRU)");
        assert!(cache.get(&k_d).unwrap().is_some(), "D should be present");
    }

    #[test]
    fn clear_empties_cache() {
        let cache = PageCache::new(10).unwrap();
        let (k_a, p_a) = page_a();
        let (k_b, p_b) = page_b();

        cache.insert(k_a, p_a).unwrap();
        cache.insert(k_b, p_b).unwrap();
        assert!(!cache.is_empty().unwrap());

        cache.clear().unwrap();
        assert!(cache.is_empty().unwrap());
        assert_eq!(cache.len().unwrap(), 0);
    }

    #[test]
    fn remove_returns_page() {
        let cache = PageCache::new(10).unwrap();
        let (key, page) = page_a();
        cache.insert(key.clone(), page.clone()).unwrap();

        let removed = cache.remove(&key).unwrap();
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().size, page.size);
        assert!(cache.get(&key).unwrap().is_none());
    }

    #[test]
    fn concurrent_access_is_safe() {
        let cache = Arc::new(PageCache::new(100).unwrap());
        let mut handles = vec![];

        for i in 0..10 {
            let cache = cache.clone();
            handles.push(std::thread::spawn(move || {
                for j in 0..10 {
                    let key = PageKey { partition_id: i as u64, page_index: j };
                    let page = Page { data: vec![(i * 10 + j) as u8; 64], size: 64 };
                    let _ = cache.insert(key.clone(), page);
                    let _ = cache.get(&key);
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(cache.len().unwrap(), 100);
    }

    #[test]
    fn zero_capacity_returns_err() {
        let err = PageCache::new(0).unwrap_err();
        assert!(err.to_string().contains("capacity must be > 0"));
    }

    #[test]
    fn single_page_cache() {
        let cache = PageCache::new(1).unwrap();
        let (k_a, p_a) = page_a();
        let (k_b, p_b) = page_b();

        cache.insert(k_a.clone(), p_a).unwrap();
        cache.insert(k_b.clone(), p_b).unwrap();

        // A should be evicted when B is inserted
        assert!(cache.get(&k_a).unwrap().is_none());
        assert!(cache.get(&k_b).unwrap().is_some());
    }
}
