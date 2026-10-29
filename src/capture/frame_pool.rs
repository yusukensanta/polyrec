//! Recycled pixel buffers for captured video frames.
//!
//! A BGRA frame is 8.3 MB at 1080p and 33 MB at 4K. Allocating one fresh per
//! frame goes straight to `VirtualAlloc` at that size, so every frame used to
//! fault in brand-new pages before the GPU readback could even be copied into
//! them. Frames now come from a `FramePool` instead: a dropped `FrameBuf`
//! (the encoder is done with it, or it was never sent) goes back to the pool
//! it came from and is reused for a later frame of the same size.

use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex, PoisonError};

/// How many idle buffers a pool keeps for reuse. Beyond this, returned
/// buffers are freed, so a backlog (frames queued behind a slow encoder)
/// doesn't keep its peak memory pinned once it drains. In the steady state
/// only a handful of frames are in flight at once.
const MAX_IDLE: usize = 4;

#[derive(Default)]
pub struct FramePool {
    idle: Mutex<Vec<Vec<u8>>>,
}

impl FramePool {
    pub fn new() -> Arc<Self> {
        Arc::default()
    }

    /// A `len`-byte buffer that returns to this pool when dropped. Its
    /// contents are unspecified (a previous frame's pixels, or zeros for a
    /// fresh allocation) -- callers overwrite every byte.
    pub fn take(self: &Arc<Self>, len: usize) -> FrameBuf {
        let reused = self
            .idle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop()
            .filter(|bytes| bytes.len() == len);
        FrameBuf {
            bytes: reused.unwrap_or_else(|| vec![0u8; len]),
            pool: Some(Arc::clone(self)),
        }
    }

    fn put(&self, bytes: Vec<u8>) {
        let mut idle = self.idle.lock().unwrap_or_else(PoisonError::into_inner);
        if idle.len() < MAX_IDLE {
            idle.push(bytes);
        }
    }

    #[cfg(test)]
    fn idle_count(&self) -> usize {
        self.idle.lock().unwrap().len()
    }
}

/// A frame's pixel bytes. Dereferences to `[u8]`. Pooled buffers go back to
/// their `FramePool` on drop; ones built `From<Vec<u8>>` are simply freed.
pub struct FrameBuf {
    bytes: Vec<u8>,
    pool: Option<Arc<FramePool>>,
}

impl From<Vec<u8>> for FrameBuf {
    fn from(bytes: Vec<u8>) -> Self {
        Self { bytes, pool: None }
    }
}

impl Deref for FrameBuf {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.bytes
    }
}

impl DerefMut for FrameBuf {
    fn deref_mut(&mut self) -> &mut [u8] {
        &mut self.bytes
    }
}

impl Drop for FrameBuf {
    fn drop(&mut self) {
        if let Some(pool) = self.pool.take() {
            pool.put(std::mem::take(&mut self.bytes));
        }
    }
}

impl std::fmt::Debug for FrameBuf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameBuf")
            .field("len", &self.bytes.len())
            .field("pooled", &self.pool.is_some())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dropped_buffer_is_reused_for_the_next_frame_of_the_same_size() {
        let pool = FramePool::new();
        let first = pool.take(64);
        let first_ptr = first.as_ptr();
        drop(first);
        assert_eq!(pool.idle_count(), 1);
        let second = pool.take(64);
        assert_eq!(
            second.as_ptr(),
            first_ptr,
            "expected the same allocation back"
        );
        assert_eq!(second.len(), 64);
        assert_eq!(pool.idle_count(), 0);
    }

    #[test]
    fn a_buffer_of_a_different_size_is_not_reused() {
        let pool = FramePool::new();
        drop(pool.take(64));
        let other = pool.take(128);
        assert_eq!(other.len(), 128);
    }

    #[test]
    fn the_pool_keeps_at_most_max_idle_buffers() {
        let pool = FramePool::new();
        let bufs: Vec<FrameBuf> = (0..MAX_IDLE + 3).map(|_| pool.take(16)).collect();
        drop(bufs);
        assert_eq!(pool.idle_count(), MAX_IDLE);
    }

    #[test]
    fn a_buffer_built_from_a_vec_is_not_pooled() {
        let buf = FrameBuf::from(vec![1u8, 2, 3]);
        assert_eq!(&*buf, &[1, 2, 3]);
        drop(buf); // must not panic or go anywhere
    }

    #[test]
    fn a_fresh_buffer_is_zeroed_and_writable() {
        let pool = FramePool::new();
        let mut buf = pool.take(8);
        assert!(buf.iter().all(|&b| b == 0));
        buf.copy_from_slice(&[9u8; 8]);
        assert_eq!(&*buf, &[9u8; 8]);
    }
}
