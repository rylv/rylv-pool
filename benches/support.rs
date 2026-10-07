//! Workloads and fixture cleanup shared by local and remote measurements.

use std::{hint::black_box, thread::LocalKey};

use rylv_pool::{
    FixedThreadLocalEntry, FixedThreadLocalPool, FixedThreadLocalPoolGuard, PoolError, PoolItem,
    PoolProvider,
};

pub(super) const CAPACITY: usize = 256;
pub(super) const LOCAL_BATCH: usize = 256;

pub(super) type PoolKey<T, const N: usize = CAPACITY> =
    &'static LocalKey<FixedThreadLocalPool<T, N>>;
pub(super) type Guard<T, const N: usize = CAPACITY> = FixedThreadLocalPoolGuard<T, N>;

pub(super) trait BenchItem: PoolItem {
    fn create() -> Self;
    fn touch(&mut self);
}

pub(super) struct Small([u64; 8]);

impl PoolItem for Small {
    fn reset(&mut self) {}
}

impl BenchItem for Small {
    fn create() -> Self {
        Self([42; 8])
    }

    fn touch(&mut self) {
        self.0[0] = self.0[0].wrapping_add(1);
        black_box(&self.0);
    }
}

pub(super) struct Buffer(Vec<u8>);

impl PoolItem for Buffer {
    fn reset(&mut self) {
        self.0.clear();
    }
}

impl BenchItem for Buffer {
    fn create() -> Self {
        Self(Vec::with_capacity(1024))
    }

    fn touch(&mut self) {
        self.0.extend_from_slice(&[7; 64]);
        black_box(self.0.as_slice());
    }
}

pub(super) fn acquire<T: BenchItem, const N: usize>(key: PoolKey<T, N>) -> Guard<T, N> {
    Guard::acquire_with(key, T::create).expect("the benchmark factory must succeed")
}

pub(super) fn take_existing<T: BenchItem, const N: usize>(
    key: PoolKey<T, N>,
) -> FixedThreadLocalEntry<T, N> {
    key.take(|| Err::<T, ()>(()))
        .expect("benchmark setup must leave an existing entry")
}

pub(super) fn clear_pool<T: BenchItem, const N: usize>(key: PoolKey<T, N>) -> usize {
    let mut removed = 0;
    loop {
        // Dropping provider entries directly consumes them without returning
        // them through a guard, keeping the next fixture genuinely empty.
        match key.take(|| Err::<T, ()>(())) {
            Ok(entry) => {
                drop(entry);
                removed += 1;
            }
            Err(PoolError::Factory(())) => return removed,
            Err(error @ PoolError::Panic(_)) => {
                panic!("benchmark cleanup panicked: {error:?}");
            }
        }
    }
}
