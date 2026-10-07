//! Local reuse, allocation reference costs, and warming fixed-size storage.

use std::{
    convert::Infallible,
    hint::black_box,
    time::{Duration, Instant},
};

use criterion::{BenchmarkId, Criterion, Throughput};
use rylv_pool::PoolProvider;

use crate::support::{
    BenchItem, CAPACITY, Guard, LOCAL_BATCH, PoolKey, acquire, clear_pool, take_existing,
};

pub(super) fn run<T: BenchItem>(
    criterion: &mut Criterion,
    key: PoolKey<T>,
    miss_key: PoolKey<T, 0>,
    item_label: &str,
) {
    let mut reuse = criterion.benchmark_group("local/reuse");
    reuse.throughput(Throughput::Elements(LOCAL_BATCH as u64));
    reuse.bench_function(item_label, |bencher| {
        clear_pool(key);
        assert_eq!(key.warm(1, || Ok::<_, Infallible>(T::create())).unwrap(), 1);
        bencher.iter(|| {
            for _ in 0..LOCAL_BATCH {
                let mut guard = black_box(
                    Guard::acquire_with(key, || {
                        panic!("local reuse must not invoke the factory");
                    })
                    .expect("prewarmed acquisition must succeed"),
                );
                guard.touch();
                drop(black_box(guard));
            }
        });
        assert_eq!(clear_pool(key), 1);
    });
    reuse.finish();

    let mut miss = criterion.benchmark_group("local/miss");
    miss.throughput(Throughput::Elements(LOCAL_BATCH as u64));
    miss.bench_function(item_label, |bencher| {
        clear_pool(miss_key);
        bencher.iter(|| {
            for _ in 0..LOCAL_BATCH {
                let mut guard = black_box(acquire(miss_key));
                guard.touch();
                drop(black_box(guard));
            }
        });
        assert_eq!(clear_pool(miss_key), 0);
    });
    miss.finish();

    let mut allocation = criterion.benchmark_group("alloc/box");
    allocation.throughput(Throughput::Elements(LOCAL_BATCH as u64));
    allocation.bench_function(item_label, |bencher| {
        bencher.iter(|| {
            for _ in 0..LOCAL_BATCH {
                let mut value = black_box(Box::new(T::create()));
                value.touch();
                value.reset();
                drop(black_box(value));
            }
        });
    });
    allocation.finish();

    let mut full = criterion.benchmark_group("warm/full");
    full.throughput(Throughput::Elements(LOCAL_BATCH as u64));
    full.bench_function(item_label, |bencher| {
        clear_pool(key);
        assert_eq!(
            key.warm(CAPACITY, || Ok::<_, Infallible>(T::create()))
                .unwrap(),
            CAPACITY
        );
        bencher.iter(|| {
            for _ in 0..LOCAL_BATCH {
                let inserted = key
                    .warm(CAPACITY, || -> Result<T, Infallible> {
                        panic!("full warm must not invoke the factory");
                    })
                    .expect("warming a full pool must succeed");
                black_box(inserted);
            }
        });
        assert_eq!(clear_pool(key), CAPACITY);
    });
    full.finish();

    let mut refill = criterion.benchmark_group("warm/refill");
    for count in [1, 32, CAPACITY] {
        refill.throughput(Throughput::Elements(count as u64));
        refill.bench_function(BenchmarkId::new(count.to_string(), item_label), |bencher| {
            clear_pool(key);
            bencher.iter_custom(|iterations| {
                let mut elapsed = Duration::ZERO;
                for _ in 0..iterations {
                    let started = Instant::now();
                    let result = key.warm(count, || Ok::<_, Infallible>(T::create()));
                    elapsed += started.elapsed();
                    assert_eq!(result.unwrap(), count);
                    // Consume retained entries between batches without guard
                    // returns. These deallocations are outside the warm timer.
                    for _ in 0..count {
                        drop(take_existing(key));
                    }
                }
                elapsed
            });
            assert_eq!(clear_pool(key), 0);
        });
    }
    refill.finish();
}
