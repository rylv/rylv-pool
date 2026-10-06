//! Thread-local provider with lock-free cross-thread returns.

use std::{
    cell::UnsafeCell,
    ops::{Deref, DerefMut},
    ptr::NonNull,
    sync::{
        Arc, Weak,
        atomic::{AtomicPtr, Ordering},
    },
    thread::{self, LocalKey, ThreadId},
};

use stable_deref_trait::StableDeref;

use super::{PoolError, PoolGuard, PoolItem, PoolProvider, core::Storage};

struct Entry<T: PoolItem, const N: usize> {
    value: T,
    metadata: ThreadLocalMetadata<T, N>,
}

impl<T: PoolItem, const N: usize> Entry<T, N> {
    const fn new(value: T, metadata: ThreadLocalMetadata<T, N>) -> Self {
        Self { value, metadata }
    }
}

/// Origin routing carried by entries managed by [`FixedThreadLocalPool`].
struct ThreadLocalMetadata<T: PoolItem, const N: usize> {
    pool_id: ThreadId,
    return_queue: Weak<RemoteReturns<T, N>>,
    remote_next: AtomicPtr<Entry<T, N>>,
}

/// Opaque entry owned by the thread-local provider.
/// Its value remains address-stable while the entry moves between threads.
#[doc(hidden)]
pub struct FixedThreadLocalEntry<T: PoolItem, const N: usize> {
    node: Box<Entry<T, N>>,
}

impl<T: PoolItem, const N: usize> Deref for FixedThreadLocalEntry<T, N> {
    type Target = T;

    #[inline(always)]
    fn deref(&self) -> &T {
        &self.node.value
    }
}

impl<T: PoolItem, const N: usize> DerefMut for FixedThreadLocalEntry<T, N> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut T {
        &mut self.node.value
    }
}

// SAFETY: the value lives in a Box and the entry never replaces the allocation.
unsafe impl<T: PoolItem, const N: usize> StableDeref for FixedThreadLocalEntry<T, N> {}

/// Intrusive MPSC stack. Foreign threads are producers and the origin thread
/// is its only consumer.
struct RemoteReturns<T: PoolItem, const N: usize> {
    head: AtomicPtr<Entry<T, N>>,
}

impl<T: PoolItem, const N: usize> RemoteReturns<T, N> {
    const fn new() -> Self {
        Self {
            head: AtomicPtr::new(std::ptr::null_mut()),
        }
    }

    fn push(&self, entry: Box<Entry<T, N>>) {
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

    fn take_all(&self) -> RemoteReturnBatch<T, N> {
        RemoteReturnBatch {
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

struct RemoteReturnBatch<T: PoolItem, const N: usize> {
    next: *mut Entry<T, N>,
}

impl<T: PoolItem, const N: usize> Iterator for RemoteReturnBatch<T, N> {
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

impl<T: PoolItem, const N: usize> Drop for RemoteReturnBatch<T, N> {
    fn drop(&mut self) {
        for entry in self.by_ref() {
            drop(entry);
        }
    }
}

impl<T: PoolItem, const N: usize> ThreadLocalMetadata<T, N> {
    #[inline(always)]
    const fn new(pool_id: ThreadId, return_queue: Weak<RemoteReturns<T, N>>) -> Self {
        Self {
            pool_id,
            return_queue,
            remote_next: AtomicPtr::new(std::ptr::null_mut()),
        }
    }
}

struct ThreadLocalStorage<T: PoolItem, const N: usize> {
    storage: Storage<Box<Entry<T, N>>, N>,
    remote: Arc<RemoteReturns<T, N>>,
}

impl<T: PoolItem, const N: usize> ThreadLocalStorage<T, N> {
    fn new() -> Self {
        Self {
            storage: Storage::new(),
            remote: Arc::new(RemoteReturns::new()),
        }
    }
}

/// Per-thread pool with capacity `N` fixed at compile time.
///
/// `N` limits locally retained entries, not active guards. Remote returns may
/// temporarily exceed `N`; older overflow is discarded when the queue drains.
///
/// `UnsafeCell` removes dynamic borrow tracking from the hot path. Every
/// private access is short and never invokes client code or drops an entry.
/// The storage itself cannot be shared between threads; use its TLS key.
/// When calling [`PoolProvider`] directly, return entries through the same key
/// used to acquire them. [`FixedThreadLocalPoolGuard`] retains that key automatically.
///
/// ```compile_fail,E0277
/// use rylv_pool::{FixedThreadLocalPool, PoolItem};
/// # struct Item;
/// # impl PoolItem for Item { fn reset(&mut self) {} }
/// fn require_sync<T: Sync>() {}
/// require_sync::<FixedThreadLocalPool<Item, 1>>();
/// ```
pub struct FixedThreadLocalPool<T: PoolItem, const N: usize>(UnsafeCell<ThreadLocalStorage<T, N>>);

impl<T: PoolItem, const N: usize> FixedThreadLocalPool<T, N> {
    /// Create empty storage for each thread that initializes this value.
    #[must_use]
    pub fn new() -> Self {
        Self(UnsafeCell::new(ThreadLocalStorage::new()))
    }

    #[inline(always)]
    fn try_take_stored(&self) -> Option<Box<Entry<T, N>>> {
        // SAFETY: this TLS value is reachable only on its owning thread. The
        // exclusive reference does not escape and no client code is invoked.
        unsafe { (&mut *self.0.get()).storage.pop() }
    }

    #[inline(always)]
    fn try_store(&self, entry: Box<Entry<T, N>>) -> Result<(), Box<Entry<T, N>>> {
        // SAFETY: insertion neither invokes client code nor drops the entry.
        unsafe { (&mut *self.0.get()).storage.try_push(entry) }
    }

    #[inline(always)]
    fn stored_len(&self) -> usize {
        // SAFETY: this read does not invoke client code or expose a reference.
        unsafe { (&*self.0.get()).storage.len() }
    }

    #[inline(always)]
    fn take_remote_returns(&self) -> RemoteReturnBatch<T, N> {
        // SAFETY: the shared access reaches only the atomic remote queue.
        unsafe { (&*self.0.get()).remote.take_all() }
    }

    #[inline(always)]
    fn metadata(&self) -> ThreadLocalMetadata<T, N> {
        // SAFETY: cloning a Weak neither mutates storage nor invokes client
        // code, and the returned metadata owns the Weak handle.
        let return_queue = unsafe { Arc::downgrade(&(&*self.0.get()).remote) };
        ThreadLocalMetadata::new(thread::current().id(), return_queue)
    }

    #[cold]
    #[inline(never)]
    fn refill_from_remote(&self) {
        let mut returned = self.take_remote_returns();
        {
            // SAFETY: storage is confined to this thread and the reference
            // does not escape this scope. The detached iterator only
            // reconstructs owned Boxes; filling empty slots and reversing them
            // neither invokes client code nor drops entries.
            let storage = unsafe { &mut *self.0.get() };
            storage.storage.extend_newest_first(&mut returned);
        }
        // The batch now contains only older overflow. Drop it after releasing
        // storage access so destructors can safely reenter the pool.
        drop(returned);
    }

    #[inline(always)]
    fn try_take(&self) -> Option<Box<Entry<T, N>>> {
        if let Some(entry) = self.try_take_stored() {
            return Some(entry);
        }
        self.refill_from_remote();
        self.try_take_stored()
    }
}

impl<T: PoolItem, const N: usize> Default for FixedThreadLocalPool<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

/// Guard using a consumer-defined thread-local pool key as its provider.
pub type FixedThreadLocalPoolGuard<T, const N: usize> =
    PoolGuard<T, &'static LocalKey<FixedThreadLocalPool<T, N>>>;

/// If storage is unavailable during thread shutdown, acquisition creates an
/// unpooled entry and warming does nothing. Returns reject entries so they
/// are destroyed outside storage access.
impl<T: PoolItem, const N: usize> PoolProvider<T>
    for &'static LocalKey<FixedThreadLocalPool<T, N>>
{
    type Entry = FixedThreadLocalEntry<T, N>;

    #[inline(always)]
    fn take<F, E>(&self, create: F) -> Result<Self::Entry, PoolError<E>>
    where
        F: FnOnce() -> Result<T, E>,
    {
        PoolError::catch(|| {
            if let Ok(Some(entry)) = self.try_with(FixedThreadLocalPool::try_take) {
                return Ok(FixedThreadLocalEntry { node: entry });
            }

            // User code and allocation run after TLS storage access has ended.
            let value = create()?;
            let metadata = self
                .try_with(FixedThreadLocalPool::metadata)
                .unwrap_or_else(|_| ThreadLocalMetadata::new(thread::current().id(), Weak::new()));
            Ok(FixedThreadLocalEntry {
                node: Box::new(Entry::new(value, metadata)),
            })
        })
    }

    #[inline(always)]
    fn return_entry(&self, entry: Self::Entry) -> Result<(), Self::Entry> {
        try_return_to_origin(self, entry.node).map_err(|node| FixedThreadLocalEntry { node })
    }

    fn warm<F, E>(&self, count: usize, mut create: F) -> Result<usize, PoolError<E>>
    where
        F: FnMut() -> Result<T, E>,
    {
        PoolError::catch(|| {
            let target = count.min(N);
            let Ok(missing) = self.try_with(|pool| {
                pool.refill_from_remote();
                target.saturating_sub(pool.stored_len())
            }) else {
                return Ok(0);
            };
            let mut inserted = 0;

            for _ in 0..missing {
                // User code and allocation deliberately run without an active TLS
                // storage access, preserving reentrant acquisition.
                let value = create()?;
                let Ok(metadata) = self.try_with(FixedThreadLocalPool::metadata) else {
                    drop(value);
                    break;
                };
                let entry = Box::new(Entry::new(value, metadata));
                match try_return_to_origin(self, entry) {
                    Ok(()) => inserted += 1,
                    Err(entry) => {
                        drop(entry);
                        break;
                    }
                }
            }
            Ok(inserted)
        })
    }
}

#[inline(always)]
fn try_return_to_origin<T: PoolItem, const N: usize>(
    pool: &'static LocalKey<FixedThreadLocalPool<T, N>>,
    entry: Box<Entry<T, N>>,
) -> Result<(), Box<Entry<T, N>>> {
    if entry.metadata.pool_id == thread::current().id() {
        let mut entry = Some(entry);
        let _ = pool.try_with(|pool| {
            if let Some(returned) = entry.take() {
                entry = pool.try_store(returned).err();
            }
        });
        return entry.map_or(Ok(()), Err);
    }
    return_remote(entry)
}

/// Publish an entry back to its origin thread without touching current TLS.
#[cold]
#[inline(never)]
fn return_remote<T: PoolItem, const N: usize>(
    entry: Box<Entry<T, N>>,
) -> Result<(), Box<Entry<T, N>>> {
    // TODO:perf Benchmark the Weak::upgrade on remote returns.
    let Some(queue) = entry.metadata.return_queue.upgrade() else {
        return Err(entry);
    };
    queue.push(entry);
    Ok(())
}

#[cfg(test)]
mod tests;
