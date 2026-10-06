//! Provider-independent pooling primitives.
//!
//! [`PoolProvider`] deliberately exposes only the lifecycle operations needed
//! by [`PoolGuard`]. Providers choose their own storage, identity, routing, and
//! synchronization strategy.

use std::{
    convert::Infallible,
    mem::ManuallyDrop,
    ops::{Deref, DerefMut},
};

use stable_deref_trait::StableDeref;

use super::{PoolError, error::catch_operation};

/// A reusable value managed by a pool.
///
/// Values must be movable between threads; thread-confined values are rejected.
///
/// ```compile_fail,E0277
/// use std::rc::Rc;
/// use rylv_pool::PoolItem;
///
/// struct LocalOnly(Rc<()>);
/// impl PoolItem for LocalOnly {
///     fn reset(&mut self) {}
/// }
/// ```
pub trait PoolItem: Send + 'static {
    /// Restore a value to its reusable state before it returns to a provider.
    fn reset(&mut self);
}

/// Supplies complete take, return, and warming operations for a pool.
///
/// The provider owns all policy decisions. It may use a queue, thread-local
/// storage, sharding, remote-return queues, or another implementation without
/// exposing those concepts through this trait.
///
/// # Reentrancy
///
/// `take` and `warm` must invoke `create` without holding an
/// exclusive borrow or lock that would make reentrant acquisition unsound.
/// `return_entry` must return a rejected entry to its caller rather than
/// dropping it while such internal access remains active. Providers must keep
/// storage invariants valid on unwind. `take` and `warm` should use
/// [`PoolError::catch`] to capture unwinding panics.
pub trait PoolProvider<T: PoolItem>: Send + 'static {
    /// Provider-owned entry with exclusive access to an address-stable value.
    /// Providers choose its allocation strategy and private routing context.
    ///
    /// An entry providing access without `StableDeref` is insufficient:
    ///
    /// ```compile_fail,E0277
    /// use rylv_pool::{PoolError, PoolItem, PoolProvider};
    /// # struct Item;
    /// # impl PoolItem for Item { fn reset(&mut self) {} }
    /// # struct InlineEntry(Item);
    /// # impl std::ops::Deref for InlineEntry {
    /// #     type Target = Item;
    /// #     fn deref(&self) -> &Item { &self.0 }
    /// # }
    /// # impl std::ops::DerefMut for InlineEntry {
    /// #     fn deref_mut(&mut self) -> &mut Item { &mut self.0 }
    /// # }
    /// struct Provider;
    /// impl PoolProvider<Item> for Provider {
    ///     type Entry = InlineEntry;
    /// #   fn take<F, E>(&self, create: F) -> Result<Self::Entry, PoolError<E>>
    /// #   where F: FnOnce() -> Result<Item, E> {
    /// #       create().map(InlineEntry).map_err(PoolError::Factory)
    /// #   }
    /// #   fn return_entry(&self, entry: Self::Entry) -> Result<(), Self::Entry> {
    /// #       Err(entry)
    /// #   }
    /// #   fn warm<F, E>(&self, _: usize, _: F) -> Result<usize, PoolError<E>>
    /// #   where F: FnMut() -> Result<Item, E> { Ok(0) }
    /// }
    /// ```
    type Entry: DerefMut<Target = T> + StableDeref + Send + 'static;

    /// Take a reusable entry, invoking `create` only on a pool miss.
    ///
    /// # Errors
    ///
    /// Returns the factory error or a captured panic when acquisition fails.
    fn take<F, E>(&self, create: F) -> Result<Self::Entry, PoolError<E>>
    where
        F: FnOnce() -> Result<T, E>;

    /// Return an entry to its pool.
    ///
    /// On rejection, ownership must be returned so destruction happens after
    /// provider access has ended.
    ///
    /// # Errors
    ///
    /// Returns the unchanged entry when the provider cannot retain or route
    /// it. The guard then destroys it after provider access has ended.
    fn return_entry(&self, entry: Self::Entry) -> Result<(), Self::Entry>;

    /// Ensure that up to `count` configured values are available for reuse.
    ///
    /// If creation fails after some entries were inserted, those entries stay
    /// available and the creation error is returned.
    ///
    /// # Errors
    ///
    /// Returns the factory error or a captured panic while warming the pool.
    fn warm<F, E>(&self, count: usize, create: F) -> Result<usize, PoolError<E>>
    where
        F: FnMut() -> Result<T, E>;
}

// Keep this helper internal even if its containing module becomes public.
#[allow(clippy::redundant_pub_crate)]
pub(super) struct Storage<E, const N: usize> {
    slots: [Option<E>; N],
    len: usize,
}

impl<E, const N: usize> Storage<E, N> {
    pub(super) const fn new() -> Self {
        Self {
            slots: [const { None }; N],
            len: 0,
        }
    }

    #[inline(always)]
    pub(super) const fn pop(&mut self) -> Option<E> {
        if self.len == 0 {
            return None;
        }
        self.len -= 1;
        self.slots[self.len].take()
    }

    #[inline(always)]
    pub(super) fn try_push(&mut self, entry: E) -> Result<(), E> {
        if self.len < N {
            self.slots[self.len] = Some(entry);
            self.len += 1;
            Ok(())
        } else {
            Err(entry)
        }
    }

    #[inline(always)]
    pub(super) const fn len(&self) -> usize {
        self.len
    }

    /// Retain the newest entries that fit, leaving older overflow in the
    /// iterator. Only the appended slots are reversed to preserve pop order.
    pub(super) fn extend_newest_first(&mut self, entries: &mut impl Iterator<Item = E>) {
        let start = self.len;
        while self.len < N {
            let Some(entry) = entries.next() else {
                break;
            };
            self.slots[self.len] = Some(entry);
            self.len += 1;
        }
        self.slots[start..self.len].reverse();
    }
}

/// Exclusive, address-stable guard returned by a [`PoolProvider`].
///
/// # Address stability
///
/// The provider entry implements [`StableDeref`], keeping the value at the same
/// address for the complete lifetime of the guard, even when the guard moves.
///
/// References into the value cannot outlive the guard:
///
/// ```compile_fail,E0505
/// use rylv_pool::{FixedThreadLocalPool, FixedThreadLocalPoolGuard, PoolItem};
/// # #[derive(Default)]
/// # struct Item(Vec<u8>);
/// # impl PoolItem for Item { fn reset(&mut self) { self.0.clear(); } }
/// # thread_local! {
/// #     static POOL: FixedThreadLocalPool<Item, 1> = FixedThreadLocalPool::new();
/// # }
/// let mut guard = FixedThreadLocalPoolGuard::acquire(&POOL).unwrap();
/// let borrowed = &mut guard.0;
/// drop(guard);
/// borrowed.push(1);
/// ```
///
/// A guard cannot provide two simultaneously used mutable references:
///
/// ```compile_fail,E0499
/// use rylv_pool::{FixedThreadLocalPool, FixedThreadLocalPoolGuard, PoolItem};
/// # #[derive(Default)]
/// # struct Item(Vec<u8>);
/// # impl PoolItem for Item { fn reset(&mut self) { self.0.clear(); } }
/// # thread_local! {
/// #     static POOL: FixedThreadLocalPool<Item, 1> = FixedThreadLocalPool::new();
/// # }
/// let mut guard = FixedThreadLocalPoolGuard::acquire(&POOL).unwrap();
/// let first = &mut guard.0;
/// let second = &mut guard.0;
/// first.push(1);
/// second.push(2);
/// ```
///
/// A guard cannot be cloned to create a second owner:
///
/// ```compile_fail,E0599
/// use rylv_pool::{FixedThreadLocalPool, FixedThreadLocalPoolGuard, PoolItem};
/// # #[derive(Default)]
/// # struct Item;
/// # impl PoolItem for Item { fn reset(&mut self) {} }
/// # thread_local! {
/// #     static POOL: FixedThreadLocalPool<Item, 1> = FixedThreadLocalPool::new();
/// # }
/// let guard = FixedThreadLocalPoolGuard::acquire(&POOL).unwrap();
/// let duplicate = guard.clone();
/// ```
pub struct PoolGuard<T: PoolItem, P: PoolProvider<T>> {
    node: ManuallyDrop<P::Entry>,
    provider: P,
}

impl<T: PoolItem, P: PoolProvider<T>> PoolGuard<T, P> {
    /// Acquire a value through the supplied provider.
    ///
    /// # Errors
    ///
    /// Returns a captured panic if acquisition or the default factory unwinds.
    #[inline(always)]
    pub fn acquire(provider: P) -> Result<Self, PoolError<Infallible>>
    where
        T: Default,
    {
        Self::acquire_with(provider, T::default)
    }

    /// Acquire a value, using `create` only when the provider misses.
    ///
    /// # Errors
    ///
    /// Returns a captured panic if acquisition or the factory unwinds.
    #[inline(always)]
    pub fn acquire_with<F>(provider: P, create: F) -> Result<Self, PoolError<Infallible>>
    where
        F: FnOnce() -> T,
    {
        Self::try_acquire_with(provider, || Ok::<T, Infallible>(create()))
    }

    /// Acquire a value using a fallible factory only when the provider misses.
    ///
    /// # Errors
    ///
    /// Returns the factory error or a captured panic without creating a guard.
    #[inline(always)]
    pub fn try_acquire_with<F, E>(provider: P, create: F) -> Result<Self, PoolError<E>>
    where
        F: FnOnce() -> Result<T, E>,
    {
        Ok(Self {
            node: ManuallyDrop::new(
                catch_operation(|| provider.take(create)).map_err(PoolError::Panic)??,
            ),
            provider,
        })
    }
}

impl<T: PoolItem, P: PoolProvider<T>> Deref for PoolGuard<T, P> {
    type Target = T;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        self.node.deref()
    }
}

impl<T: PoolItem, P: PoolProvider<T>> DerefMut for PoolGuard<T, P> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.node.deref_mut()
    }
}

// SAFETY: P::Entry implements StableDeref and the guard never replaces it.
unsafe impl<T: PoolItem, P: PoolProvider<T>> StableDeref for PoolGuard<T, P> {}

impl<T: PoolItem, P: PoolProvider<T>> Drop for PoolGuard<T, P> {
    #[inline(always)]
    fn drop(&mut self) {
        // SAFETY: `node` is initialized by every constructor, `PoolGuard`
        // cannot be reused after `Drop` starts, and this is its only take.
        let mut node = unsafe { ManuallyDrop::take(&mut self.node) };
        node.reset();

        if let Err(node) = self.provider.return_entry(node) {
            discard_entry(node);
        }
    }
}

/// Destroy rejected entries after provider access has ended.
#[cold]
#[inline(never)]
fn discard_entry<E>(entry: E) {
    drop(entry);
}

#[cfg(test)]
mod tests;
