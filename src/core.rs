//! Provider-independent pooling primitives.
//!
//! [`PoolProvider`] chooses storage and acquisition; [`PoolEntry`] carries the
//! return route used by [`PoolGuard`].

use std::{
    convert::Infallible,
    marker::PhantomData,
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

/// An address-stable entry that routes itself back to its origin after reset.
///
/// The guard calls this method exactly once. Implementations must release any
/// pool access before destroying a rejected entry, so its destructor may
/// reenter the pool safely.
pub trait PoolEntry<T: PoolItem>: DerefMut<Target = T> + StableDeref + Send + 'static {
    /// Return this entry to its origin, or destroy it if it cannot be retained.
    fn return_to_pool(self);
}

/// An entry whose value can be detached from its return route and later reattached.
///
/// Splitting ends the original guard's lifetime. Reattaching creates a new
/// address-stable guard; the value's address need not survive the split.
pub trait SplittablePoolEntry<T: PoolItem>: PoolEntry<T> {
    /// Opaque ownership of the route back to the entry's original pool.
    type ReturnHandle: Send + 'static;

    /// Move the value out of the entry without resetting it.
    fn into_parts(self) -> (T, Self::ReturnHandle);

    /// Build an entry from a value and its original return route.
    fn from_parts(value: T, handle: Self::ReturnHandle) -> Self;
}

/// A boxed entry that is intentionally discarded when its guard is dropped.
///
/// Providers that retain entries should define their own [`PoolEntry`] type.
pub struct DiscardEntry<T: PoolItem>(Box<T>);

impl<T: PoolItem> From<Box<T>> for DiscardEntry<T> {
    #[inline]
    fn from(value: Box<T>) -> Self {
        Self(value)
    }
}

impl<T: PoolItem> Deref for DiscardEntry<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T: PoolItem> DerefMut for DiscardEntry<T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

// SAFETY: the value stays in the Box when the entry moves.
unsafe impl<T: PoolItem> StableDeref for DiscardEntry<T> {}

impl<T: PoolItem> PoolEntry<T> for DiscardEntry<T> {
    #[inline]
    fn return_to_pool(self) {
        drop(self);
    }
}

impl<T: PoolItem> SplittablePoolEntry<T> for DiscardEntry<T> {
    type ReturnHandle = ();

    fn into_parts(self) -> (T, ()) {
        (*self.0, ())
    }

    fn from_parts(value: T, (): ()) -> Self {
        Self(Box::new(value))
    }
}

/// Supplies take, direct return, and warming operations for a pool.
///
/// The entry carries the guard's return route. Providers may use a queue,
/// thread-local storage, sharding, or another implementation without exposing
/// those concepts through the guard.
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
    /// # impl rylv_pool::PoolEntry<Item> for InlineEntry {
    /// #     fn return_to_pool(self) {}
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
    type Entry: PoolEntry<T>;

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

/// Exclusive, address-stable guard over a self-returning [`PoolEntry`].
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
pub struct PoolGuard<T: PoolItem, E: PoolEntry<T>> {
    node: Option<E>,
    item: PhantomData<T>,
}

impl<T: PoolItem, E: PoolEntry<T>> PoolGuard<T, E> {
    /// Acquire a value through the supplied provider.
    ///
    /// # Errors
    ///
    /// Returns a captured panic if acquisition or the default factory unwinds.
    #[inline(always)]
    pub fn acquire<P: PoolProvider<T, Entry = E>>(
        provider: P,
    ) -> Result<Self, PoolError<Infallible>>
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
    pub fn acquire_with<P, F>(provider: P, create: F) -> Result<Self, PoolError<Infallible>>
    where
        P: PoolProvider<T, Entry = E>,
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
    #[allow(clippy::needless_pass_by_value)] // Keep the consuming acquisition API.
    pub fn try_acquire_with<P, F, FactoryError>(
        provider: P,
        create: F,
    ) -> Result<Self, PoolError<FactoryError>>
    where
        P: PoolProvider<T, Entry = E>,
        F: FnOnce() -> Result<T, FactoryError>,
    {
        let node = catch_operation(|| provider.take(create)).map_err(PoolError::Panic)??;
        Ok(Self {
            node: Some(node),
            item: PhantomData,
        })
    }
}

impl<T: PoolItem, E: SplittablePoolEntry<T>> PoolGuard<T, E> {
    /// Separate the value from its return route without resetting it.
    ///
    /// The caller owns the value until it is reattached. Dropping either part
    /// separately does not return a value to the pool.
    ///
    /// # Panics
    ///
    /// Panics only if the guard's internal entry has already been removed,
    /// which cannot happen through the public API.
    #[must_use]
    #[allow(clippy::expect_used)] // The entry exists until this method takes it.
    pub fn into_parts(mut self) -> (T, E::ReturnHandle) {
        self.node.take().expect("guard entry missing").into_parts()
    }

    /// Reattach a value to a return route obtained from [`Self::into_parts`].
    /// Dropping the resulting guard resets the value and returns it to its pool.
    #[must_use]
    pub fn from_parts(value: T, handle: E::ReturnHandle) -> Self {
        Self {
            node: Some(E::from_parts(value, handle)),
            item: PhantomData,
        }
    }
}

impl<T: PoolItem, E: PoolEntry<T>> Deref for PoolGuard<T, E> {
    type Target = T;

    #[inline(always)]
    #[allow(clippy::expect_used)] // The entry is present throughout the guard's public lifetime.
    fn deref(&self) -> &Self::Target {
        self.node.as_ref().expect("guard entry missing").deref()
    }
}

impl<T: PoolItem, E: PoolEntry<T>> DerefMut for PoolGuard<T, E> {
    #[inline(always)]
    #[allow(clippy::expect_used)] // The entry is present throughout the guard's public lifetime.
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.node.as_mut().expect("guard entry missing").deref_mut()
    }
}

// SAFETY: E implements StableDeref and remains owned by the guard until Drop.
unsafe impl<T: PoolItem, E: PoolEntry<T>> StableDeref for PoolGuard<T, E> {}

impl<T: PoolItem, E: PoolEntry<T>> Drop for PoolGuard<T, E> {
    #[inline(always)]
    fn drop(&mut self) {
        if let Some(node) = self.node.as_mut() {
            node.deref_mut().reset();
        }

        // On reset panic, the still-owned entry is dropped with the guard.
        if let Some(node) = self.node.take() {
            node.return_to_pool();
        }
    }
}

#[cfg(test)]
mod tests;
