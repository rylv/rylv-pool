//! Local reuse, allocation reference costs, and warming fixed-size storage.

use std::{
    convert::Infallible,
    hint::black_box,
    time::{Duration, Instant},
};

use rylv_pool::PoolProvider;

use crate::{
    Measurement, Runner,
    support::{
        BenchItem, CAPACITY, Guard, LOCAL_BATCH, PoolKey, acquire, clear_pool, take_existing,
    },
};

pub(super) fn run<T: BenchItem>(
    runner: &mut Runner,
    key: PoolKey<T>,
    miss_key: PoolKey<T, 0>,
    item_label: &str,
) {
    runner.measure(
        &format!("local/reuse/{item_label}"),
        LOCAL_BATCH,
        "op",
        |iterations| {
            clear_pool(key);
            assert_eq!(key.warm(1, || Ok::<_, Infallible>(T::create())).unwrap(), 1);
            let started = Instant::now();
            for _ in 0..iterations {
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
            }
            let elapsed = started.elapsed();
            assert_eq!(clear_pool(key), 1);
            Measurement {
                elapsed,
                batches: iterations,
            }
        },
    );

    runner.measure(
        &format!("local/miss/{item_label}"),
        LOCAL_BATCH,
        "op",
        |iterations| {
            clear_pool(miss_key);
            let started = Instant::now();
            for _ in 0..iterations {
                for _ in 0..LOCAL_BATCH {
                    let mut guard = black_box(acquire(miss_key));
                    guard.touch();
                    drop(black_box(guard));
                }
            }
            let elapsed = started.elapsed();
            assert_eq!(clear_pool(miss_key), 0);
            Measurement {
                elapsed,
                batches: iterations,
            }
        },
    );

    runner.measure(
        &format!("alloc/box/{item_label}"),
        LOCAL_BATCH,
        "op",
        |iterations| {
            let started = Instant::now();
            for _ in 0..iterations {
                for _ in 0..LOCAL_BATCH {
                    let mut value = black_box(Box::new(T::create()));
                    value.touch();
                    value.reset();
                    drop(black_box(value));
                }
            }
            Measurement {
                elapsed: started.elapsed(),
                batches: iterations,
            }
        },
    );

    runner.measure(
        &format!("warm/full/{item_label}"),
        LOCAL_BATCH,
        "call",
        |iterations| {
            clear_pool(key);
            assert_eq!(
                key.warm(CAPACITY, || Ok::<_, Infallible>(T::create()))
                    .unwrap(),
                CAPACITY
            );
            let started = Instant::now();
            for _ in 0..iterations {
                for _ in 0..LOCAL_BATCH {
                    let inserted = key
                        .warm(CAPACITY, || -> Result<T, Infallible> {
                            panic!("full warm must not invoke the factory");
                        })
                        .expect("warming a full pool must succeed");
                    black_box(inserted);
                }
            }
            let elapsed = started.elapsed();
            assert_eq!(clear_pool(key), CAPACITY);
            Measurement {
                elapsed,
                batches: iterations,
            }
        },
    );

    for count in [1, 32, CAPACITY] {
        runner.measure(
            &format!("warm/refill/{count}/{item_label}"),
            count,
            "entry",
            |iterations| {
                clear_pool(key);
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
                assert_eq!(clear_pool(key), 0);
                Measurement {
                    elapsed,
                    batches: iterations,
                }
            },
        );
    }
}
