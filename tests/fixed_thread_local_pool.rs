//! Public API tests for fixed-capacity thread-local pooling.
//!
//! Every stateful scenario runs on a fresh owner thread so TLS contents and
//! destructor counters cannot leak between tests or depend on runner order.

use std::{
    cell::Cell,
    collections::HashSet,
    convert::Infallible,
    sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

use rylv_pool::{
    FixedThreadLocalPool, FixedThreadLocalPoolGuard, PoolError, PoolItem, PoolProvider,
};
use stable_deref_trait::StableDeref;

#[derive(Default)]
struct Buffer {
    bytes: Vec<u8>,
}

impl PoolItem for Buffer {
    fn reset(&mut self) {
        self.bytes.clear();
    }
}

#[derive(Default)]
struct Counts {
    created: AtomicUsize,
    reset: AtomicUsize,
    dropped: AtomicUsize,
}

struct CountedItem {
    id: usize,
    dirty: bool,
    counts: Arc<Counts>,
}

impl CountedItem {
    fn new(id: usize, counts: &Arc<Counts>) -> Self {
        counts.created.fetch_add(1, Ordering::Relaxed);
        Self {
            id,
            dirty: false,
            counts: Arc::clone(counts),
        }
    }
}

impl PoolItem for CountedItem {
    fn reset(&mut self) {
        self.dirty = false;
        self.counts.reset.fetch_add(1, Ordering::Relaxed);
    }
}

impl Drop for CountedItem {
    fn drop(&mut self) {
        self.counts.dropped.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Default)]
struct SendOnlyItem(Cell<usize>);

impl PoolItem for SendOnlyItem {
    fn reset(&mut self) {
        self.0.set(0);
    }
}

#[derive(Default)]
struct ZeroSizedItem;

impl PoolItem for ZeroSizedItem {
    fn reset(&mut self) {}
}

thread_local! {
    static BUFFER_POOL: FixedThreadLocalPool<Buffer, 2> = FixedThreadLocalPool::default();
    static COUNTED_POOL: FixedThreadLocalPool<CountedItem, 16> = FixedThreadLocalPool::new();
    static ONE_POOL: FixedThreadLocalPool<CountedItem, 1> = FixedThreadLocalPool::new();
    static ZERO_POOL: FixedThreadLocalPool<CountedItem, 0> = FixedThreadLocalPool::new();
    static SEND_ONLY_POOL: FixedThreadLocalPool<SendOnlyItem, 1> = FixedThreadLocalPool::new();
    static ZST_POOL: FixedThreadLocalPool<ZeroSizedItem, 3> = FixedThreadLocalPool::new();
    static ZERO_ZST_POOL: FixedThreadLocalPool<ZeroSizedItem, 0> = FixedThreadLocalPool::new();
}

fn address<T>(value: &T) -> usize {
    std::ptr::from_ref(value).addr()
}

fn assert_send<T: Send>() {}

fn assert_stable_deref<T: StableDeref>(_: &T) {}

#[test]
fn default_pool_is_empty_and_guards_reset_without_losing_buffer_capacity() {
    thread::spawn(|| {
        let mut guard = FixedThreadLocalPoolGuard::acquire(&BUFFER_POOL).unwrap();
        assert!(guard.bytes.is_empty());
        guard.bytes.extend_from_slice(b"retained allocation");
        let item_address = address(&*guard);
        let buffer_address = guard.bytes.as_ptr();
        let capacity = guard.bytes.capacity();
        drop(guard);

        let reused = FixedThreadLocalPoolGuard::acquire_with(&BUFFER_POOL, || {
            panic!("stored buffer must avoid the factory")
        })
        .unwrap();
        assert!(reused.bytes.is_empty());
        assert_eq!(address(&*reused), item_address);
        assert_eq!(reused.bytes.as_ptr(), buffer_address);
        assert_eq!(reused.bytes.capacity(), capacity);
    })
    .join()
    .unwrap();
}

#[test]
fn simultaneous_guards_exceed_retained_capacity_and_own_distinct_values() {
    let counts = Arc::new(Counts::default());
    let owned_counts = Arc::clone(&counts);
    thread::spawn(move || {
        let mut guards: Vec<_> = (0..40)
            .map(|id| {
                FixedThreadLocalPoolGuard::acquire_with(&COUNTED_POOL, || {
                    CountedItem::new(id, &owned_counts)
                })
                .unwrap()
            })
            .collect();
        let addresses: HashSet<_> = guards.iter().map(|guard| address(&**guard)).collect();
        assert_eq!(addresses.len(), 40);
        for guard in &mut guards {
            guard.dirty = true;
        }
        assert!(guards.iter().all(|guard| guard.dirty));
        assert_eq!(owned_counts.created.load(Ordering::Relaxed), 40);

        for guard in guards {
            drop(guard);
        }
        assert_eq!(owned_counts.reset.load(Ordering::Relaxed), 40);
        assert_eq!(owned_counts.dropped.load(Ordering::Relaxed), 24);

        let retained: Vec<_> = (0..16)
            .map(|_| {
                FixedThreadLocalPoolGuard::acquire_with(&COUNTED_POOL, || {
                    panic!("retained entries must avoid the factory")
                })
                .unwrap()
            })
            .collect();
        assert!(retained.iter().all(|guard| !guard.dirty));
        assert_eq!(
            retained.iter().map(|guard| guard.id).collect::<Vec<_>>(),
            (0..16).rev().collect::<Vec<_>>()
        );
    })
    .join()
    .unwrap();
    assert_eq!(counts.created.load(Ordering::Relaxed), 40);
    assert_eq!(counts.reset.load(Ordering::Relaxed), 56);
    assert_eq!(counts.dropped.load(Ordering::Relaxed), 40);
}

#[test]
fn the_same_key_has_independent_storage_on_each_thread() {
    let counts = Arc::new(Counts::default());
    let owned_counts = Arc::clone(&counts);
    thread::spawn(move || {
        let original = FixedThreadLocalPoolGuard::acquire_with(&ONE_POOL, || {
            CountedItem::new(1, &owned_counts)
        })
        .unwrap();
        let original_address = address(&*original);
        drop(original);

        let other_counts = Arc::clone(&owned_counts);
        thread::spawn(move || {
            let other = FixedThreadLocalPoolGuard::acquire_with(&ONE_POOL, || {
                CountedItem::new(2, &other_counts)
            })
            .unwrap();
            assert_eq!(other.id, 2);
            assert_ne!(address(&*other), original_address);
        })
        .join()
        .unwrap();

        let original = FixedThreadLocalPoolGuard::acquire_with(&ONE_POOL, || {
            panic!("the owner must retain its own entry")
        })
        .unwrap();
        assert_eq!(original.id, 1);
        assert_eq!(address(&*original), original_address);
    })
    .join()
    .unwrap();
    assert_eq!(counts.created.load(Ordering::Relaxed), 2);
    assert_eq!(counts.reset.load(Ordering::Relaxed), 3);
    assert_eq!(counts.dropped.load(Ordering::Relaxed), 2);
}

#[test]
fn guard_moves_between_threads_preserve_non_sync_values_and_addresses() {
    type Guard = FixedThreadLocalPoolGuard<SendOnlyItem, 1>;
    type Entry =
        <&'static thread::LocalKey<FixedThreadLocalPool<SendOnlyItem, 1>> as PoolProvider<
            SendOnlyItem,
        >>::Entry;
    assert_send::<Guard>();
    assert_send::<Entry>();

    thread::spawn(|| {
        let guard = Guard::acquire(&SEND_ONLY_POOL).unwrap();
        assert_stable_deref(&guard);
        let original_address = address(&*guard);
        guard.0.set(42);

        let moved = thread::spawn(move || {
            assert_eq!(address(&*guard), original_address);
            assert_eq!(guard.0.get(), 42);
            guard.0.set(99);
            guard
        })
        .join()
        .unwrap();
        assert_eq!(address(&*moved), original_address);
        assert_eq!(moved.0.get(), 99);
        drop(moved);

        let reused = Guard::acquire(&SEND_ONLY_POOL).unwrap();
        assert_eq!(address(&*reused), original_address);
        assert_eq!(reused.0.get(), 0);
    })
    .join()
    .unwrap();
}

#[test]
fn fallible_factory_errors_do_not_mutate_storage_or_hide_reusable_entries() {
    thread::spawn(|| {
        let error = FixedThreadLocalPoolGuard::try_acquire_with(&BUFFER_POOL, || {
            Err::<Buffer, _>(String::from("creation failed"))
        })
        .err()
        .unwrap();
        assert!(matches!(error, PoolError::Factory(message) if message == "creation failed"));

        let mut guard = FixedThreadLocalPoolGuard::acquire(&BUFFER_POOL).unwrap();
        guard.bytes.push(1);
        drop(guard);
        let guard = FixedThreadLocalPoolGuard::try_acquire_with(&BUFFER_POOL, || {
            Err::<Buffer, _>("a pool hit must ignore this error")
        })
        .unwrap();
        assert!(guard.bytes.is_empty());
    })
    .join()
    .unwrap();
}

#[test]
fn direct_provider_returns_keep_rejected_entries_unchanged_and_owned_by_the_caller() {
    let counts = Arc::new(Counts::default());
    let owned_counts = Arc::clone(&counts);
    thread::spawn(move || {
        let provider = &ONE_POOL;
        let first = provider
            .take(|| Ok::<_, Infallible>(CountedItem::new(1, &owned_counts)))
            .unwrap();
        let mut second = provider
            .take(|| Ok::<_, Infallible>(CountedItem::new(2, &owned_counts)))
            .unwrap();
        assert_stable_deref(&second);
        second.dirty = true;
        let second_address = address(&*second);

        assert!(provider.return_entry(first).is_ok());
        let rejected = provider.return_entry(second).err().unwrap();
        assert_eq!(rejected.id, 2);
        assert!(rejected.dirty);
        assert_eq!(address(&*rejected), second_address);
        assert_eq!(owned_counts.reset.load(Ordering::Relaxed), 0);
        assert_eq!(owned_counts.dropped.load(Ordering::Relaxed), 0);
        drop(rejected);

        let retained = provider
            .take(|| Err::<CountedItem, _>("must retain the first entry"))
            .unwrap();
        assert_eq!(retained.id, 1);
        assert_eq!(owned_counts.dropped.load(Ordering::Relaxed), 1);
        drop(retained);
    })
    .join()
    .unwrap();
    assert_eq!(counts.created.load(Ordering::Relaxed), 2);
    assert_eq!(counts.reset.load(Ordering::Relaxed), 0);
    assert_eq!(counts.dropped.load(Ordering::Relaxed), 2);
}

#[test]
fn warm_targets_available_entries_and_never_exceeds_the_retained_capacity() {
    let counts = Arc::new(Counts::default());
    let owned_counts = Arc::clone(&counts);
    thread::spawn(move || {
        let provider = &COUNTED_POOL;
        let mut calls = 0;
        assert_eq!(
            provider
                .warm(0, || Err::<CountedItem, _>("unused"))
                .unwrap(),
            0
        );
        assert_eq!(
            provider
                .warm(3, || {
                    calls += 1;
                    Ok::<_, Infallible>(CountedItem::new(calls, &owned_counts))
                })
                .unwrap(),
            3
        );
        assert_eq!(calls, 3);
        assert_eq!(
            provider
                .warm(2, || Err::<CountedItem, _>("unused"))
                .unwrap(),
            0
        );

        let active = FixedThreadLocalPoolGuard::acquire_with(provider, || {
            panic!("warmed value must be available")
        })
        .unwrap();
        assert_eq!(
            provider
                .warm(usize::MAX, || {
                    calls += 1;
                    Ok::<_, Infallible>(CountedItem::new(calls, &owned_counts))
                })
                .unwrap(),
            14
        );
        assert_eq!(calls, 17);
        assert_eq!(
            provider
                .warm(usize::MAX, || Err::<CountedItem, _>("unused"))
                .unwrap(),
            0
        );
        drop(active);
        assert_eq!(owned_counts.reset.load(Ordering::Relaxed), 1);
        assert_eq!(owned_counts.dropped.load(Ordering::Relaxed), 1);
    })
    .join()
    .unwrap();
    assert_eq!(counts.created.load(Ordering::Relaxed), 17);
    assert_eq!(counts.dropped.load(Ordering::Relaxed), 17);
}

#[test]
fn warming_errors_and_panics_keep_completed_values_available() {
    thread::spawn(|| {
        let provider = &BUFFER_POOL;
        let mut calls = 0;
        let error = provider
            .warm(2, || {
                calls += 1;
                if calls == 2 {
                    Err("second creation failed")
                } else {
                    Ok(Buffer { bytes: vec![7] })
                }
            })
            .err()
            .unwrap();
        assert!(matches!(
            error,
            PoolError::Factory("second creation failed")
        ));
        assert_eq!(calls, 2);
        let first = FixedThreadLocalPoolGuard::acquire_with(provider, || {
            panic!("completed warm insertion must survive")
        })
        .unwrap();
        assert_eq!(first.bytes, vec![7]);

        calls = 0;
        let error = provider
            .warm(2, || -> Result<Buffer, Infallible> {
                calls += 1;
                assert_ne!(calls, 2, "second warm factory panicked");
                Ok(Buffer { bytes: vec![9] })
            })
            .err()
            .unwrap();
        assert!(matches!(error, PoolError::Panic(_)));
        assert_eq!(calls, 2);
        let second = FixedThreadLocalPoolGuard::acquire_with(provider, || {
            panic!("completed warm insertion must survive a panic")
        })
        .unwrap();
        assert_eq!(second.bytes, vec![9]);
    })
    .join()
    .unwrap();
}

#[test]
fn remote_returns_retain_the_newest_values_and_drop_older_overflow_once() {
    let counts = Arc::new(Counts::default());
    let owned_counts = Arc::clone(&counts);
    thread::spawn(move || {
        let guards: Vec<_> = (0..5)
            .map(|id| {
                FixedThreadLocalPoolGuard::acquire_with(&ONE_POOL, || {
                    CountedItem::new(id, &owned_counts)
                })
                .unwrap()
            })
            .collect();
        let newest_address = address(&**guards.last().unwrap());
        thread::spawn(move || {
            for guard in guards {
                drop(guard);
            }
        })
        .join()
        .unwrap();
        assert_eq!(owned_counts.reset.load(Ordering::Relaxed), 5);
        assert_eq!(owned_counts.dropped.load(Ordering::Relaxed), 0);

        let newest = FixedThreadLocalPoolGuard::acquire_with(&ONE_POOL, || {
            panic!("newest remote entry must be reused")
        })
        .unwrap();
        assert_eq!(newest.id, 4);
        assert_eq!(address(&*newest), newest_address);
        assert_eq!(owned_counts.dropped.load(Ordering::Relaxed), 4);
    })
    .join()
    .unwrap();
    assert_eq!(counts.created.load(Ordering::Relaxed), 5);
    assert_eq!(counts.reset.load(Ordering::Relaxed), 6);
    assert_eq!(counts.dropped.load(Ordering::Relaxed), 5);
}

#[test]
fn many_foreign_producers_return_each_value_once_under_contention() {
    const PRODUCERS: usize = 16;
    const PER_PRODUCER: usize = 8;
    const TOTAL: usize = PRODUCERS * PER_PRODUCER;
    const RETAINED: usize = 16;

    let counts = Arc::new(Counts::default());
    let owned_counts = Arc::clone(&counts);
    thread::spawn(move || {
        let batches: Vec<Vec<_>> = (0..PRODUCERS)
            .map(|producer| {
                (0..PER_PRODUCER)
                    .map(|index| {
                        FixedThreadLocalPoolGuard::acquire_with(&COUNTED_POOL, || {
                            CountedItem::new(producer * PER_PRODUCER + index, &owned_counts)
                        })
                        .unwrap()
                    })
                    .collect()
            })
            .collect();
        let barrier = Arc::new(Barrier::new(PRODUCERS + 1));
        thread::scope(|scope| {
            for batch in batches {
                let barrier = Arc::clone(&barrier);
                scope.spawn(move || {
                    barrier.wait();
                    for guard in batch {
                        drop(guard);
                    }
                });
            }
            barrier.wait();
        });

        assert_eq!(owned_counts.created.load(Ordering::Relaxed), TOTAL);
        assert_eq!(owned_counts.reset.load(Ordering::Relaxed), TOTAL);
        assert_eq!(owned_counts.dropped.load(Ordering::Relaxed), 0);
        let retained: Vec<_> = (0..RETAINED)
            .map(|_| {
                FixedThreadLocalPoolGuard::acquire_with(&COUNTED_POOL, || {
                    panic!("returned values must satisfy the retained acquisitions")
                })
                .unwrap()
            })
            .collect();
        let ids: HashSet<_> = retained.iter().map(|guard| guard.id).collect();
        let addresses: HashSet<_> = retained.iter().map(|guard| address(&**guard)).collect();
        assert_eq!(ids.len(), RETAINED);
        assert_eq!(addresses.len(), RETAINED);
        assert!(ids.iter().all(|id| *id < TOTAL));
        assert_eq!(
            owned_counts.dropped.load(Ordering::Relaxed),
            TOTAL - RETAINED
        );
    })
    .join()
    .unwrap();
    assert_eq!(counts.created.load(Ordering::Relaxed), TOTAL);
    assert_eq!(counts.reset.load(Ordering::Relaxed), TOTAL + RETAINED);
    assert_eq!(counts.dropped.load(Ordering::Relaxed), TOTAL);
}

#[test]
fn zero_capacity_creates_each_acquisition_and_rejects_each_local_return() {
    let counts = Arc::new(Counts::default());
    let owned_counts = Arc::clone(&counts);
    thread::spawn(move || {
        let provider = &ZERO_POOL;
        assert_eq!(
            provider
                .warm(usize::MAX, || Err::<CountedItem, _>("unused"))
                .unwrap(),
            0
        );
        for id in 0..8 {
            let guard = FixedThreadLocalPoolGuard::acquire_with(&ZERO_POOL, || {
                CountedItem::new(id, &owned_counts)
            })
            .unwrap();
            assert_eq!(guard.id, id);
            drop(guard);
            assert_eq!(owned_counts.reset.load(Ordering::Relaxed), id + 1);
            assert_eq!(owned_counts.dropped.load(Ordering::Relaxed), id + 1);
        }
    })
    .join()
    .unwrap();
    assert_eq!(counts.created.load(Ordering::Relaxed), 8);
    assert_eq!(counts.dropped.load(Ordering::Relaxed), 8);
}

#[test]
fn zero_capacity_remote_returns_are_destroyed_when_warm_drains_the_queue() {
    let counts = Arc::new(Counts::default());
    let owned_counts = Arc::clone(&counts);
    thread::spawn(move || {
        let guards: Vec<_> = (0..6)
            .map(|id| {
                FixedThreadLocalPoolGuard::acquire_with(&ZERO_POOL, || {
                    CountedItem::new(id, &owned_counts)
                })
                .unwrap()
            })
            .collect();
        thread::spawn(move || drop(guards)).join().unwrap();
        assert_eq!(owned_counts.reset.load(Ordering::Relaxed), 6);
        assert_eq!(owned_counts.dropped.load(Ordering::Relaxed), 0);
        let provider = &ZERO_POOL;
        assert_eq!(
            provider
                .warm(0, || Err::<CountedItem, _>("unused"))
                .unwrap(),
            0
        );
        assert_eq!(owned_counts.dropped.load(Ordering::Relaxed), 6);
    })
    .join()
    .unwrap();
    assert_eq!(counts.created.load(Ordering::Relaxed), 6);
    assert_eq!(counts.dropped.load(Ordering::Relaxed), 6);
}

#[test]
fn owner_exit_destroys_pending_remote_returns_without_another_acquisition() {
    let counts = Arc::new(Counts::default());
    let owned_counts = Arc::clone(&counts);
    thread::spawn(move || {
        let guards: Vec<_> = (0..20)
            .map(|id| {
                FixedThreadLocalPoolGuard::acquire_with(&COUNTED_POOL, || {
                    CountedItem::new(id, &owned_counts)
                })
                .unwrap()
            })
            .collect();
        thread::spawn(move || drop(guards)).join().unwrap();
        assert_eq!(owned_counts.reset.load(Ordering::Relaxed), 20);
        assert_eq!(owned_counts.dropped.load(Ordering::Relaxed), 0);
    })
    .join()
    .unwrap();
    assert_eq!(counts.created.load(Ordering::Relaxed), 20);
    assert_eq!(counts.reset.load(Ordering::Relaxed), 20);
    assert_eq!(counts.dropped.load(Ordering::Relaxed), 20);
}

#[test]
fn guards_outlive_the_owner_and_are_destroyed_on_a_foreign_return() {
    let counts = Arc::new(Counts::default());
    let owned_counts = Arc::clone(&counts);
    let guards: Vec<_> = thread::spawn(move || {
        (0..4)
            .map(|id| {
                FixedThreadLocalPoolGuard::acquire_with(&COUNTED_POOL, || {
                    CountedItem::new(id, &owned_counts)
                })
                .unwrap()
            })
            .collect()
    })
    .join()
    .unwrap();
    assert_eq!(counts.reset.load(Ordering::Relaxed), 0);
    assert_eq!(counts.dropped.load(Ordering::Relaxed), 0);
    assert_eq!(
        guards.iter().map(|guard| guard.id).collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );
    drop(guards);
    assert_eq!(counts.created.load(Ordering::Relaxed), 4);
    assert_eq!(counts.reset.load(Ordering::Relaxed), 4);
    assert_eq!(counts.dropped.load(Ordering::Relaxed), 4);
}

#[test]
fn owner_exit_races_with_foreign_returns_without_losing_entries() {
    const PRODUCERS: usize = 16;
    const ROUNDS: usize = 8;

    let counts = Arc::new(Counts::default());
    for round in 0..ROUNDS {
        let owned_counts = Arc::clone(&counts);
        let producers = thread::spawn(move || {
            let barrier = Arc::new(Barrier::new(PRODUCERS + 1));
            let mut producers = Vec::new();
            for index in 0..PRODUCERS {
                let guard = FixedThreadLocalPoolGuard::acquire_with(&COUNTED_POOL, || {
                    CountedItem::new(round * PRODUCERS + index, &owned_counts)
                })
                .unwrap();
                let barrier = Arc::clone(&barrier);
                producers.push(thread::spawn(move || {
                    barrier.wait();
                    drop(guard);
                }));
            }
            // The owner leaves its TLS storage while the producers return.
            barrier.wait();
            producers
        })
        .join()
        .unwrap();

        for producer in producers {
            producer.join().unwrap();
        }
        let completed = (round + 1) * PRODUCERS;
        assert_eq!(counts.created.load(Ordering::Relaxed), completed);
        assert_eq!(counts.reset.load(Ordering::Relaxed), completed);
        assert_eq!(counts.dropped.load(Ordering::Relaxed), completed);
    }
}

#[test]
fn zero_sized_values_work_with_local_remote_and_zero_capacity_pools() {
    thread::spawn(|| {
        let provider = &ZST_POOL;
        assert_eq!(
            provider
                .warm(usize::MAX, || Ok::<_, Infallible>(ZeroSizedItem))
                .unwrap(),
            3
        );
        let guards: Vec<_> = (0..3)
            .map(|_| {
                FixedThreadLocalPoolGuard::acquire_with(&ZST_POOL, || {
                    panic!("warmed zero-sized values must be reused")
                })
                .unwrap()
            })
            .collect();
        assert!(
            guards
                .iter()
                .all(|guard| std::mem::size_of_val(&**guard) == 0)
        );
        thread::spawn(move || drop(guards)).join().unwrap();
        let reused = FixedThreadLocalPoolGuard::acquire_with(&ZST_POOL, || {
            panic!("remote zero-sized value must be reused")
        })
        .unwrap();
        assert_eq!(std::mem::size_of_val(&*reused), 0);
        let zero_provider = &ZERO_ZST_POOL;
        assert_eq!(
            zero_provider
                .warm(1, || Ok::<_, Infallible>(ZeroSizedItem))
                .unwrap(),
            0
        );
        drop(FixedThreadLocalPoolGuard::acquire(&ZERO_ZST_POOL).unwrap());
    })
    .join()
    .unwrap();
}
