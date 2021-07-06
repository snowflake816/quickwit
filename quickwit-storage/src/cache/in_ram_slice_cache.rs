// Quickwit
//  Copyright (C) 2021 Quickwit Inc.
//
//  Quickwit is offered under the AGPL v3.0 and as commercial software.
//  For commercial licensing, contact us at hello@quickwit.io.
//
//  AGPL:
//  This program is free software: you can redistribute it and/or modify
//  it under the terms of the GNU Affero General Public License as
//  published by the Free Software Foundation, either version 3 of the
//  License, or (at your option) any later version.
//
//  This program is distributed in the hope that it will be useful,
//  but WITHOUT ANY WARRANTY; without even the implied warranty of
//  MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
//  GNU Affero General Public License for more details.
//
//  You should have received a copy of the GNU Affero General Public License
//  along with this program.  If not, see <http://www.gnu.org/licenses/>.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use bytes::Bytes;
use lru::LruCache;
use tracing::{error, warn};

#[derive(Hash, Debug, Clone, PartialEq, Eq)]
struct SliceAddress {
    pub path: PathBuf,
    pub byte_range: Range<usize>,
}

#[derive(Clone, Copy, Debug)]
enum Capacity {
    Unlimited,
    InBytes(usize),
}

impl Capacity {
    fn exceeds_capacity(&self, num_bytes: usize) -> bool {
        match *self {
            Capacity::Unlimited => false,
            Capacity::InBytes(capacity_in_bytes) => num_bytes > capacity_in_bytes,
        }
    }
}
struct NeedMutSliceCache {
    lru_cache: LruCache<SliceAddress, Bytes>,
    num_bytes: usize,
    capacity: Capacity,
}

impl NeedMutSliceCache {
    /// Creates a new NeedMutSliceCache with the given capacity.
    fn with_capacity(capacity: Capacity) -> Self {
        NeedMutSliceCache {
            // The limit will be decided by the amount of memory in the cache,
            // not the number of items in the cache.
            // Enforcing this limit is done in the `NeedMutCache` impl.
            lru_cache: LruCache::unbounded(),
            num_bytes: 0,
            capacity,
        }
    }

    fn get(&mut self, cache_key: &SliceAddress) -> Option<Bytes> {
        self.lru_cache.get(cache_key).cloned()
    }

    /// Attempt to put the given amount of data in the cache.
    /// This may fail silently if the owned_bytes slice is larger than the cache
    /// capacity.
    fn put(&mut self, slice_addr: SliceAddress, bytes: Bytes) {
        if self.capacity.exceeds_capacity(bytes.len()) {
            // The value does not fit in the cache. We simply don't store it.
            warn!(
                capacity_in_bytes = ?self.capacity,
                len = bytes.len(),
                "Downloaded a byte slice larger than the cache capacity."
            );
            return;
        }
        if let Some(previous_data) = self.lru_cache.pop(&slice_addr) {
            self.num_bytes -= previous_data.len();
        }
        while self.capacity.exceeds_capacity(self.num_bytes + bytes.len()) {
            if let Some((_, bytes)) = self.lru_cache.pop_lru() {
                self.num_bytes -= bytes.len();
            } else {
                error!("Logical error. Even after removing all of the items in the cache the capacity is insufficient. This case is guarded against and should never happen.");
                return;
            }
        }
        self.num_bytes += bytes.len();
        self.lru_cache.put(slice_addr, bytes);
    }
}

/// A simple in-resident memory slice cache.
pub struct SliceCache {
    inner: Mutex<NeedMutSliceCache>,
}

impl SliceCache {
    /// Creates an slice cache with the given capacity.
    pub fn with_capacity_in_bytes(capacity_in_bytes: usize) -> Self {
        SliceCache {
            inner: Mutex::new(NeedMutSliceCache::with_capacity(Capacity::InBytes(
                capacity_in_bytes,
            ))),
        }
    }

    /// Creates a slice cache that nevers removes any entry.
    pub fn with_infinite_capacity() -> Self {
        SliceCache {
            inner: Mutex::new(NeedMutSliceCache::with_capacity(Capacity::Unlimited)),
        }
    }

    /// If available, returns the cached view of the slice.
    pub fn get(&self, path: &Path, bytes_range: Range<usize>) -> Option<Bytes> {
        let slice_addr = SliceAddress {
            path: path.to_path_buf(),
            byte_range: bytes_range,
        };
        self.inner.lock().unwrap().get(&slice_addr)
    }

    /// Attempt to put the given amount of data in the cache.
    /// This may fail silently if the owned_bytes slice is larger than the cache
    /// capacity.
    pub fn put(&self, path: PathBuf, byte_range: Range<usize>, bytes: Bytes) {
        let slice_addr = SliceAddress { path, byte_range };
        self.inner.lock().unwrap().put(slice_addr, bytes);
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn test_cache_edge_condition() {
        let cache = SliceCache::with_capacity_in_bytes(5);
        {
            let data = Bytes::from_static(&b"abc"[..]);
            cache.put(PathBuf::from("3"), 0..3, data);
            assert_eq!(cache.get(&Path::new("3"), 0..3).unwrap(), &b"abc"[..]);
        }
        {
            let data = Bytes::from_static(&b"de"[..]);
            cache.put(PathBuf::from("2"), 0..2, data);
            // our first entry should still be here.
            assert_eq!(cache.get(&Path::new("3"), 0..3).unwrap(), &b"abc"[..]);
            assert_eq!(cache.get(&Path::new("2"), 0..2).unwrap(), &b"de"[..]);
        }
        {
            let data = Bytes::from_static(&b"fghij"[..]);
            cache.put(PathBuf::from("5"), 0..5, data);
            assert_eq!(cache.get(&Path::new("5"), 0..5).unwrap(), &b"fghij"[..]);
            // our two first entries should have be removed from the cache
            assert!(cache.get(&Path::new("2"), 0..2).is_none());
            assert!(cache.get(&Path::new("3"), 0..3).is_none());
        }
        {
            let data = Bytes::from_static(&b"klmnop"[..]);
            cache.put(PathBuf::from("6"), 0..6, data);
            // The entry put should have been dismissed as it is too large for the cache
            assert!(cache.get(Path::new("6"), 0..6).is_none());
            // The previous entry should however be remaining.
            assert_eq!(cache.get(Path::new("5"), 0..5).unwrap(), &b"fghij"[..]);
        }
    }

    #[test]
    fn test_cache_edge_unlimited_capacity() {
        let cache = SliceCache::with_infinite_capacity();
        {
            let data = Bytes::from_static(&b"abc"[..]);
            cache.put(PathBuf::from("3"), 0..3, data);
            assert_eq!(cache.get(Path::new("3"), 0..3).unwrap(), &b"abc"[..]);
        }
        {
            let data = Bytes::from_static(&b"de"[..]);
            cache.put(PathBuf::from("2"), 0..2, data);
            assert_eq!(cache.get(&Path::new("3"), 0..3).unwrap(), &b"abc"[..]);
            assert_eq!(cache.get(&Path::new("2"), 0..2).unwrap(), &b"de"[..]);
        }
    }

    #[test]
    fn test_cache() {
        let cache = SliceCache::with_capacity_in_bytes(10_000);
        assert!(cache.get(&Path::new("hello.seg"), 1..3).is_none());
        let data = Bytes::from_static(&b"werwer"[..]);
        cache.put(PathBuf::from("hello.seg"), 1..3, data);
        assert_eq!(
            cache.get(&Path::new("hello.seg"), 1..3).unwrap(),
            &b"werwer"[..]
        );
    }

    #[test]
    fn test_cache_different_slice() {
        let cache = SliceCache::with_capacity_in_bytes(10_000);
        assert!(cache.get(Path::new("hello.seg"), 1..3).is_none());
        let data = Bytes::from_static(&b"werwer"[..]);
        // We could actually have a cache hit here, but this is not useful for Quickwit.
        cache.put(PathBuf::from("hello.seg"), 1..3, data);
        assert!(cache.get(Path::new("hello.seg"), 1..3).is_some());
        assert!(cache.get(Path::new("hello.seg"), 2..3).is_none());
    }
}
