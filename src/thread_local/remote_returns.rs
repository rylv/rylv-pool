//! Intrusive batches of entries returned by foreign threads.

use std::{
    ptr::NonNull,
    sync::atomic::{AtomicPtr, Ordering},
};

use super::{Entry, PoolItem};

/// Intrusive MPSC stack. Foreign threads are producers and the origin thread
/// is its only consumer.
pub(super) struct RemoteReturns<T: PoolItem, const N: usize> {
    head: AtomicPtr<Entry<T, N>>,
}

impl<T: PoolItem, const N: usize> RemoteReturns<T, N> {
    pub(super) const fn new() -> Self {
        Self {
            head: AtomicPtr::new(std::ptr::null_mut()),
        }
    }

    pub(super) fn push(&self, entry: Box<Entry<T, N>>) {
        let node = Box::into_raw(entry);
        let mut head = self.head.load(Ordering::Acquire);
        loop {
            // SAFETY: this producer exclusively owns `node` until the
            // successful compare-exchange publishes it.
            unsafe {
                (*node).metadata.remote_next.store(head, Ordering::Relaxed);
            }
            match self
                .head
                .compare_exchange_weak(head, node, Ordering::Release, Ordering::Acquire)
            {
                Ok(_) => return,
                Err(current) => head = current,
            }
        }
    }

    pub(super) fn take_all(&self) -> DetachedReturns<T, N> {
        DetachedReturns {
            next: self.head.swap(std::ptr::null_mut(), Ordering::Acquire),
        }
    }
}

impl<T: PoolItem, const N: usize> Drop for RemoteReturns<T, N> {
    fn drop(&mut self) {
        // Reaching Drop means no producer can upgrade its Weak handle.
        drop(self.take_all());
    }
}

/// Owns a detached list and releases unconsumed entries on drop.
pub(super) struct DetachedReturns<T: PoolItem, const N: usize> {
    next: *mut Entry<T, N>,
}

impl<T: PoolItem, const N: usize> Iterator for DetachedReturns<T, N> {
    type Item = Box<Entry<T, N>>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut current = NonNull::new(self.next)?;
        // SAFETY: nodes originated in Box::into_raw and this detached list has
        // exactly one consumer.
        unsafe {
            let metadata = &mut current.as_mut().metadata;
            self.next = metadata.remote_next.load(Ordering::Relaxed);
            metadata
                .remote_next
                .store(std::ptr::null_mut(), Ordering::Relaxed);
            Some(Box::from_raw(current.as_ptr()))
        }
    }
}

impl<T: PoolItem, const N: usize> Drop for DetachedReturns<T, N> {
    fn drop(&mut self) {
        for entry in self.by_ref() {
            drop(entry);
        }
    }
}
