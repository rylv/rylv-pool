use super::*;
use std::{
    cell::Cell,
    collections::HashSet,
    sync::{
        Barrier,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use crate::{PoolGuard, PoolItem, PoolProvider};

impl<T: PoolItem, const N: usize> ThreadLocalMetadata<T, N> {
    fn origin_is_alive(&self) -> bool {
        self.return_queue.upgrade().is_some()
    }
}

impl<T: PoolItem, const N: usize> FixedThreadLocalPool<T, N> {
    fn remote_strong_count(&self) -> usize {
        // SAFETY: this test-only read follows the same thread-confined access
        // rules as the production helpers on `FixedThreadLocalPool`.
        unsafe { std::sync::Arc::strong_count(&(&*self.0.get()).remote) }
    }
}

impl<T: PoolItem, const N: usize> FixedThreadLocalEntry<T, N> {
    fn metadata(&self) -> &ThreadLocalMetadata<T, N> {
        &self.node.metadata
    }
}

impl<T: PoolItem, const N: usize> FixedThreadLocalPoolGuard<T, N> {
    fn metadata(&self) -> &ThreadLocalMetadata<T, N> {
        self.entry().metadata()
    }
}

const CAPACITY: usize = 8;

#[derive(Default)]
struct Item(usize);

impl PoolItem for Item {
    fn reset(&mut self) {
        self.0 = 0;
    }
}

thread_local! {
    static POOL: FixedThreadLocalPool<Item, CAPACITY> = FixedThreadLocalPool::new();
    static CONFIGURED_POOL: FixedThreadLocalPool<Item, 1> = FixedThreadLocalPool::new();
    static PREWARMED_POOL: FixedThreadLocalPool<Item, 3> = FixedThreadLocalPool::new();
    static FALLIBLE_POOL: FixedThreadLocalPool<Item, 1> = FixedThreadLocalPool::new();
    static REENTRANT_POOL: FixedThreadLocalPool<Item, 2> = FixedThreadLocalPool::new();
    static DROP_REENTRANT_POOL: FixedThreadLocalPool<DropReentrantItem, 1> = FixedThreadLocalPool::new();
    static REENTER_FROM_DROP: Cell<bool> = const { Cell::new(false) };
}

#[derive(Default)]
struct DropReentrantItem;

impl PoolItem for DropReentrantItem {
    fn reset(&mut self) {}
}

impl Drop for DropReentrantItem {
    fn drop(&mut self) {
        if REENTER_FROM_DROP.with(|flag| flag.replace(false)) {
            let provider = &DROP_REENTRANT_POOL;
            drop(PoolGuard::acquire(provider).unwrap());
        }
    }
}

type Guard = FixedThreadLocalPoolGuard<Item, CAPACITY>;

fn provider() -> &'static LocalKey<FixedThreadLocalPool<Item, CAPACITY>> {
    &POOL
}

fn item_address(item: &Item) -> usize {
    std::ptr::from_ref(item).addr()
}

#[test]
fn resets_and_reuses_a_local_value() {
    let mut guard = Guard::acquire(provider()).unwrap();
    let address = item_address(&guard);
    guard.0 = 42;
    drop(guard);

    let reused = Guard::acquire(provider()).unwrap();
    assert_eq!(item_address(&reused), address);
    assert_eq!(reused.0, 0);
}

#[test]
fn acquire_with_only_invokes_the_factory_on_a_pool_miss() {
    let provider = &CONFIGURED_POOL;
    let guard = PoolGuard::acquire_with(provider, || Item(42)).unwrap();
    assert_eq!(guard.0, 42);
    drop(guard);

    let reused = PoolGuard::acquire_with(provider, || {
        panic!("factory must not run when a pooled value is available")
    })
    .unwrap();
    assert_eq!(reused.0, 0);
}

#[test]
fn fallible_factory_errors_only_on_a_pool_miss() {
    let provider = &FALLIBLE_POOL;
    let error = PoolGuard::try_acquire_with(provider, || Err::<Item, _>("creation failed"))
        .err()
        .unwrap();
    assert!(matches!(error, PoolError::Factory("creation failed")));

    drop(PoolGuard::acquire_with(provider, || Item(42)).unwrap());
    let reused =
        PoolGuard::try_acquire_with(provider, || Err::<Item, &str>("must not run")).unwrap();
    assert_eq!(reused.0, 0);
}

#[test]
fn factory_can_reenter_the_same_pool() {
    let provider = &REENTRANT_POOL;
    let outer = PoolGuard::acquire_with(provider, || {
        drop(PoolGuard::acquire(provider).unwrap());
        Item(42)
    })
    .unwrap();
    assert_eq!(outer.0, 42);
}

#[test]
fn warm_creates_only_missing_values_and_respects_capacity() {
    let provider = &PREWARMED_POOL;
    let created = provider
        .warm(usize::MAX, || Ok::<_, std::convert::Infallible>(Item(42)))
        .unwrap();
    assert_eq!(created, 3);

    let guards: Vec<_> = (0..3)
        .map(|_| {
            PoolGuard::acquire_with(provider, || {
                panic!("prewarmed acquisition must not invoke the factory")
            })
            .unwrap()
        })
        .collect();
    assert!(guards.iter().all(|guard| guard.0 == 42));
    drop(guards);

    assert_eq!(
        provider
            .warm(3, || Ok::<_, std::convert::Infallible>(Item::default()))
            .unwrap(),
        0
    );
}

#[test]
fn a_foreign_thread_returns_the_value_to_its_owner() {
    std::thread::spawn(|| {
        let guard = Guard::acquire(provider()).unwrap();
        let address = item_address(&guard);

        std::thread::spawn(move || drop(guard)).join().unwrap();

        let reused = Guard::acquire(provider()).unwrap();
        assert_eq!(item_address(&reused), address);
    })
    .join()
    .unwrap();
}

#[test]
fn remote_overflow_keeps_and_selects_the_hottest_value() {
    std::thread::spawn(|| {
        let guards: Vec<_> = (0..=CAPACITY)
            .map(|_| Guard::acquire(provider()).unwrap())
            .collect();
        let hottest = item_address(guards.last().unwrap());

        std::thread::spawn(move || {
            for guard in guards {
                drop(guard);
            }
        })
        .join()
        .unwrap();

        let reused = Guard::acquire(provider()).unwrap();
        assert_eq!(item_address(&reused), hottest);
    })
    .join()
    .unwrap();
}

#[test]
fn remote_batches_preserve_newest_first_order_and_local_entries() {
    for count in [0, 1, CAPACITY - 1, CAPACITY, CAPACITY + 2] {
        for local_count in [0, 2] {
            let pool = FixedThreadLocalPool::<Item, CAPACITY>::new();
            let mut local_addresses = Vec::new();
            let mut remote_addresses = Vec::new();
            for value in 0..local_count {
                let entry = Box::new(Entry::new(Item(value), pool.metadata()));
                local_addresses.push(std::ptr::from_ref(&*entry).addr());
                assert!(pool.try_store(entry).is_ok());
            }
            for value in 0..count {
                let entry = Box::new(Entry::new(Item(100 + value), pool.metadata()));
                remote_addresses.push(std::ptr::from_ref(&*entry).addr());
                pool.metadata().return_queue.upgrade().unwrap().push(entry);
            }

            pool.refill_from_remote();

            let kept = count.min(CAPACITY - local_count);
            for value in (count - kept..count).rev() {
                let entry = pool.try_take_stored().unwrap();
                assert_eq!(std::ptr::from_ref(&*entry).addr(), remote_addresses[value]);
            }
            for value in (0..local_count).rev() {
                let entry = pool.try_take_stored().unwrap();
                assert_eq!(std::ptr::from_ref(&*entry).addr(), local_addresses[value]);
            }
            assert!(pool.try_take_stored().is_none());
        }
    }
}

#[test]
fn dropping_remote_overflow_can_reenter_the_same_pool() {
    std::thread::spawn(|| {
        let provider = &DROP_REENTRANT_POOL;
        let guards: Vec<_> = (0..2)
            .map(|_| PoolGuard::acquire(provider).unwrap())
            .collect();

        std::thread::spawn(move || drop(guards)).join().unwrap();

        REENTER_FROM_DROP.with(|flag| flag.set(true));
        drop(PoolGuard::acquire(provider).unwrap());
        assert!(!REENTER_FROM_DROP.with(Cell::get));
    })
    .join()
    .unwrap();
}

#[test]
fn dropping_local_overflow_can_reenter_the_same_pool() {
    thread::spawn(|| {
        let provider = &DROP_REENTRANT_POOL;
        let retained = PoolGuard::acquire(provider).unwrap();
        let retained_address = std::ptr::from_ref(&*retained).addr();
        let overflow = PoolGuard::acquire(provider).unwrap();
        drop(retained);
        REENTER_FROM_DROP.with(|flag| flag.set(true));
        drop(overflow);

        assert!(!REENTER_FROM_DROP.with(Cell::get));
        let reused = PoolGuard::acquire(provider).unwrap();
        assert_eq!(std::ptr::from_ref(&*reused).addr(), retained_address);
    })
    .join()
    .unwrap();
}

#[test]
fn acquire_does_not_increment_the_remote_queue_strong_count() {
    std::thread::spawn(|| {
        let guards: Vec<_> = (0..4)
            .map(|_| Guard::acquire(provider()).unwrap())
            .collect();
        let strong_count = POOL.with(FixedThreadLocalPool::remote_strong_count);
        assert_eq!(strong_count, 1);
        drop(guards);
    })
    .join()
    .unwrap();
}

#[test]
fn a_foreign_drop_frees_the_value_if_its_owner_has_exited() {
    let guard = std::thread::spawn(|| Guard::acquire(provider()).unwrap())
        .join()
        .unwrap();
    assert!(!guard.metadata().origin_is_alive());
    drop(guard);
}

struct ShutdownItem {
    completed: Option<Arc<AtomicBool>>,
}

impl PoolItem for ShutdownItem {
    fn reset(&mut self) {}
}

thread_local! {
    static SHUTDOWN_POOL: FixedThreadLocalPool<ShutdownItem, 1> = FixedThreadLocalPool::new();
}

impl Drop for ShutdownItem {
    fn drop(&mut self) {
        let Some(completed) = self.completed.take() else {
            return;
        };
        assert!(SHUTDOWN_POOL.try_with(|_| ()).is_err());
        let provider = &SHUTDOWN_POOL;
        assert!(matches!(
            provider.warm(1, || -> Result<Self, ()> {
                panic!("warming unavailable TLS must not invoke the factory")
            }),
            Ok(0)
        ));
        assert!(provider.take(|| Err::<Self, _>("factory error")).is_err());

        let entry = provider
            .take(|| Ok::<_, ()>(Self { completed: None }))
            .unwrap();
        assert!(!entry.metadata().origin_is_alive());
        let rejected = provider.return_entry(entry).err().unwrap();
        drop(rejected);
        completed.store(true, Ordering::Release);
    }
}

#[test]
fn acquisition_and_warming_handle_tls_destruction() {
    let completed = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&completed);
    std::thread::spawn(move || {
        let provider = &SHUTDOWN_POOL;
        let entry = provider
            .take(|| {
                Ok::<_, ()>(ShutdownItem {
                    completed: Some(signal),
                })
            })
            .unwrap();
        assert!(provider.return_entry(entry).is_ok());
    })
    .join()
    .unwrap();
    assert!(completed.load(Ordering::Acquire));
}

#[test]
fn factory_panics_are_errors_and_the_pool_remains_usable() {
    std::thread::spawn(|| {
        let provider = provider();
        let error = Guard::acquire_with(provider, || panic!("factory panic"))
            .err()
            .unwrap();
        let PoolError::Panic(error) = error;
        assert_eq!(error.to_string(), "factory panic");
        assert_eq!(
            error.into_payload().downcast_ref::<&str>(),
            Some(&"factory panic")
        );
        let guard = Guard::acquire_with(provider, || Item(42)).unwrap();
        assert_eq!(guard.0, 42);
    })
    .join()
    .unwrap();
}

#[test]
fn warming_keeps_completed_insertions_on_panic_or_factory_error() {
    std::thread::spawn(|| {
        let provider = provider();
        let mut calls = 0;
        let error = provider
            .warm(2, || -> Result<Item, &str> {
                calls += 1;
                assert!(calls != 2, "warming panic");
                Ok(Item(42))
            })
            .err()
            .unwrap();
        assert!(matches!(error, PoolError::Panic(_)));
        assert_eq!(calls, 2);
        assert_eq!(POOL.with(FixedThreadLocalPool::stored_len), 1);
        assert!(matches!(
            provider.warm(2, || Err::<Item, _>("factory error")),
            Err(PoolError::Factory("factory error"))
        ));
        assert_eq!(POOL.with(FixedThreadLocalPool::stored_len), 1);
        let guard = Guard::acquire_with(provider, || panic!("stored entry must survive")).unwrap();
        assert_eq!(guard.0, 42);
    })
    .join()
    .unwrap();
}

#[test]
fn tls_initializer_panics_are_errors_in_acquisition_and_warming() {
    thread_local! {
        static TAKE_POOL: FixedThreadLocalPool<Item, 1> = panic!("take initializer");
        static WARM_POOL: FixedThreadLocalPool<Item, 1> = panic!("warm initializer");
    }
    std::thread::spawn(|| {
        let take = &TAKE_POOL;
        let error = take
            .take(|| -> Result<Item, ()> {
                panic!("factory must not run after initializer panic");
            })
            .err()
            .unwrap();
        assert!(matches!(error, PoolError::Panic(_)));
        let warm = &WARM_POOL;
        assert!(matches!(
            warm.warm(1, || Ok::<_, ()>(Item(42))),
            Err(PoolError::Panic(_))
        ));
    })
    .join()
    .unwrap();
}

#[test]
fn captured_non_sync_payloads_can_be_shared_and_recovered() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<PoolError<std::io::Error>>();
    let error = PoolError::<()>::catch(|| -> Result<(), ()> {
        std::panic::panic_any(std::cell::Cell::new(42usize));
    })
    .err()
    .unwrap();
    let PoolError::Panic(error) = error else {
        panic!("expected the original panic payload");
    };
    assert_eq!(error.to_string(), "non-string panic payload");
    let payload = error.into_payload();
    assert_eq!(
        payload
            .downcast_ref::<std::cell::Cell<usize>>()
            .unwrap()
            .get(),
        42
    );
}

struct DropCountedItem {
    id: usize,
    drops: Arc<AtomicUsize>,
}

impl PoolItem for DropCountedItem {
    fn reset(&mut self) {}
}

impl Drop for DropCountedItem {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn new_and_default_create_empty_storage_with_the_declared_capacity() {
    for pool in [
        FixedThreadLocalPool::<Item, 3>::new(),
        FixedThreadLocalPool::<Item, 3>::default(),
    ] {
        assert_eq!(pool.stored_len(), 0);
        assert_eq!(pool.remote_strong_count(), 1);
        assert!(pool.try_take().is_none());

        for id in 0..3 {
            let metadata = pool.metadata();
            assert_eq!(metadata.pool_id, thread::current().id());
            assert!(metadata.origin_is_alive());
            assert!(metadata.remote_next.load(Ordering::Relaxed).is_null());
            assert!(
                pool.try_store(Box::new(Entry::new(Item(id), metadata)))
                    .is_ok()
            );
        }

        let rejected = pool
            .try_store(Box::new(Entry::new(Item(99), pool.metadata())))
            .err()
            .unwrap();
        assert_eq!(rejected.value.0, 99);
        assert_eq!(pool.stored_len(), 3);
        for id in (0..3).rev() {
            assert_eq!(pool.try_take_stored().unwrap().value.0, id);
        }
        assert!(pool.try_take_stored().is_none());
    }
}

#[test]
fn zero_capacity_rejects_local_values_and_discards_remote_values() {
    let drops = Arc::new(AtomicUsize::new(0));
    let pool = FixedThreadLocalPool::<DropCountedItem, 0>::default();
    let local = Box::new(Entry::new(
        DropCountedItem {
            id: 0,
            drops: Arc::clone(&drops),
        },
        pool.metadata(),
    ));
    let rejected = pool.try_store(local).err().unwrap();
    assert_eq!(rejected.value.id, 0);
    assert_eq!(drops.load(Ordering::Relaxed), 0);
    drop(rejected);

    let queue = pool.metadata().return_queue.upgrade().unwrap();
    for id in 1..=5 {
        queue.push(Box::new(Entry::new(
            DropCountedItem {
                id,
                drops: Arc::clone(&drops),
            },
            pool.metadata(),
        )));
    }
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    assert!(pool.try_take().is_none());
    assert_eq!(pool.stored_len(), 0);
    assert_eq!(drops.load(Ordering::Relaxed), 6);
    assert!(queue.take_all().next().is_none());
}

#[test]
fn remote_batches_detach_and_clear_links_without_consuming_later_returns() {
    let pool = FixedThreadLocalPool::<Item, 2>::new();
    let queue = pool.metadata().return_queue.upgrade().unwrap();
    for id in 0..3 {
        queue.push(Box::new(Entry::new(Item(id), pool.metadata())));
    }
    let mut detached = queue.take_all();
    queue.push(Box::new(Entry::new(Item(99), pool.metadata())));

    for id in (0..3).rev() {
        let entry = detached.next().unwrap();
        assert_eq!(entry.value.0, id);
        assert!(entry.metadata.remote_next.load(Ordering::Relaxed).is_null());
    }
    assert!(detached.next().is_none());
    assert!(detached.next().is_none());
    let mut later = queue.take_all();
    assert_eq!(later.next().unwrap().value.0, 99);
    assert!(later.next().is_none());
}

#[test]
fn dropping_a_partially_consumed_batch_frees_each_remaining_node_once() {
    let drops = Arc::new(AtomicUsize::new(0));
    let pool = FixedThreadLocalPool::<DropCountedItem, 2>::new();
    let queue = pool.metadata().return_queue.upgrade().unwrap();
    for id in 0..5 {
        queue.push(Box::new(Entry::new(
            DropCountedItem {
                id,
                drops: Arc::clone(&drops),
            },
            pool.metadata(),
        )));
    }
    let mut detached = queue.take_all();
    let newest = detached.next().unwrap();
    assert_eq!(newest.value.id, 4);
    drop(detached);
    assert_eq!(drops.load(Ordering::Relaxed), 4);
    drop(newest);
    assert_eq!(drops.load(Ordering::Relaxed), 5);
    drop(queue);
    drop(pool);
    assert_eq!(drops.load(Ordering::Relaxed), 5);
}

#[test]
fn dropping_storage_reclaims_queued_entries_and_invalidates_active_metadata() {
    let drops = Arc::new(AtomicUsize::new(0));
    let pool = FixedThreadLocalPool::<DropCountedItem, 2>::new();
    let active = Box::new(Entry::new(
        DropCountedItem {
            id: 99,
            drops: Arc::clone(&drops),
        },
        pool.metadata(),
    ));
    for id in 0..4 {
        let entry = Box::new(Entry::new(
            DropCountedItem {
                id,
                drops: Arc::clone(&drops),
            },
            pool.metadata(),
        ));
        pool.metadata().return_queue.upgrade().unwrap().push(entry);
    }

    assert!(active.metadata.origin_is_alive());
    drop(pool);
    assert_eq!(drops.load(Ordering::Relaxed), 4);
    assert!(!active.metadata.origin_is_alive());
    let rejected = return_remote(active).err().unwrap();
    assert_eq!(rejected.value.id, 99);
    drop(rejected);
    assert_eq!(drops.load(Ordering::Relaxed), 5);
}

#[test]
fn simultaneous_producers_and_repeated_detaches_preserve_every_entry() {
    const PRODUCERS: usize = 16;
    const ROUNDS: usize = 4;
    const PER_ROUND: usize = 8;

    let queue = Arc::new(RemoteReturns::<Item, 0>::new());
    let barrier = Arc::new(Barrier::new(PRODUCERS + 1));
    let origin = thread::current().id();
    let mut batches = Vec::new();

    thread::scope(|scope| {
        for producer in 0..PRODUCERS {
            let queue = Arc::clone(&queue);
            let barrier = Arc::clone(&barrier);
            scope.spawn(move || {
                for round in 0..ROUNDS {
                    for index in 0..PER_ROUND {
                        let id = (producer * ROUNDS + round) * PER_ROUND + index;
                        let metadata = ThreadLocalMetadata::new(origin, Arc::downgrade(&queue));
                        queue.push(Box::new(Entry::new(Item(id), metadata)));
                    }
                    barrier.wait();
                    barrier.wait();
                }
            });
        }

        for _ in 0..ROUNDS {
            barrier.wait();
            batches.push(queue.take_all().collect::<Vec<_>>());
            barrier.wait();
        }
    });

    let mut observed = HashSet::new();
    for batch in batches {
        assert_eq!(batch.len(), PRODUCERS * PER_ROUND);
        for entry in batch {
            assert!(observed.insert(entry.value.0));
            assert!(entry.metadata.remote_next.load(Ordering::Relaxed).is_null());
        }
    }
    assert_eq!(observed.len(), PRODUCERS * ROUNDS * PER_ROUND);
    assert!(queue.take_all().next().is_none());
}

#[test]
fn draining_while_producers_publish_preserves_all_values_and_clears_links() {
    const PRODUCERS: usize = 8;
    const PER_PRODUCER: usize = 32;

    let queue = Arc::new(RemoteReturns::<Item, 0>::new());
    let barrier = Arc::new(Barrier::new(PRODUCERS + 1));
    let completed = AtomicUsize::new(0);
    let origin = thread::current().id();
    let mut observed = Vec::new();

    thread::scope(|scope| {
        for producer in 0..PRODUCERS {
            let queue = Arc::clone(&queue);
            let barrier = Arc::clone(&barrier);
            let completed = &completed;
            scope.spawn(move || {
                barrier.wait();
                for index in 0..PER_PRODUCER {
                    let metadata = ThreadLocalMetadata::new(origin, Arc::downgrade(&queue));
                    queue.push(Box::new(Entry::new(
                        Item(producer * PER_PRODUCER + index),
                        metadata,
                    )));
                }
                completed.fetch_add(1, Ordering::Release);
            });
        }

        barrier.wait();
        while completed.load(Ordering::Acquire) < PRODUCERS {
            for entry in queue.take_all() {
                observed.push((
                    entry.value.0,
                    entry.metadata.remote_next.load(Ordering::Relaxed).is_null(),
                ));
                // Dropping now allows allocator reuse while publication races
                // with a later detach, rather than retaining every node.
            }
            thread::yield_now();
        }
    });
    for entry in queue.take_all() {
        observed.push((
            entry.value.0,
            entry.metadata.remote_next.load(Ordering::Relaxed).is_null(),
        ));
    }

    assert_eq!(observed.len(), PRODUCERS * PER_PRODUCER);
    assert!(observed.iter().all(|(_, cleared)| *cleared));
    let ids: HashSet<_> = observed.into_iter().map(|(id, _)| id).collect();
    assert_eq!(ids.len(), PRODUCERS * PER_PRODUCER);
    assert!(ids.iter().all(|id| *id < PRODUCERS * PER_PRODUCER));
    assert!(queue.take_all().next().is_none());
}

struct ResetReentrantItem {
    reenter: bool,
    resets: Arc<AtomicUsize>,
}

impl PoolItem for ResetReentrantItem {
    fn reset(&mut self) {
        self.resets.fetch_add(1, Ordering::Relaxed);
        if self.reenter {
            self.reenter = false;
            let resets = Arc::clone(&self.resets);
            drop(
                PoolGuard::acquire_with(&RESET_REENTRANT_POOL, || Self {
                    reenter: false,
                    resets,
                })
                .unwrap(),
            );
        }
    }
}

thread_local! {
    static RESET_REENTRANT_POOL: FixedThreadLocalPool<ResetReentrantItem, 2> = FixedThreadLocalPool::new();
}

#[test]
fn resetting_a_guard_can_reenter_the_same_thread_local_pool() {
    let resets = Arc::new(AtomicUsize::new(0));
    let owned_resets = Arc::clone(&resets);
    thread::spawn(move || {
        let provider = &RESET_REENTRANT_POOL;
        let guard = PoolGuard::acquire_with(provider, || ResetReentrantItem {
            reenter: true,
            resets: owned_resets,
        })
        .unwrap();
        let address = std::ptr::from_ref(&*guard).addr();
        drop(guard);
        assert_eq!(
            RESET_REENTRANT_POOL.with(FixedThreadLocalPool::stored_len),
            2
        );
        let reused =
            PoolGuard::acquire_with(provider, || panic!("reset must retain the entry")).unwrap();
        assert_eq!(std::ptr::from_ref(&*reused).addr(), address);
        assert!(!reused.reenter);
    })
    .join()
    .unwrap();
    assert_eq!(resets.load(Ordering::Relaxed), 3);
}

#[test]
fn acquisition_consumes_local_values_before_draining_remote_returns() {
    thread::spawn(|| {
        let provider = provider();
        let local = Guard::acquire(provider).unwrap();
        let local_address = item_address(&local);
        let remote: Vec<_> = (0..3).map(|_| Guard::acquire(provider).unwrap()).collect();
        let remote_addresses: Vec<_> = remote.iter().map(|guard| item_address(guard)).collect();
        drop(local);
        thread::spawn(move || {
            for guard in remote {
                drop(guard);
            }
        })
        .join()
        .unwrap();

        let local =
            Guard::acquire_with(provider, || panic!("local value must be retained")).unwrap();
        assert_eq!(item_address(&local), local_address);
        assert_eq!(POOL.with(FixedThreadLocalPool::stored_len), 0);

        let mut guards = Vec::new();
        for address in remote_addresses.into_iter().rev() {
            let guard =
                Guard::acquire_with(provider, || panic!("remote value must be retained")).unwrap();
            assert_eq!(item_address(&guard), address);
            guards.push(guard);
        }
        assert_eq!(POOL.with(FixedThreadLocalPool::stored_len), 0);
        drop(guards);
        drop(local);
    })
    .join()
    .unwrap();
}

#[test]
fn warm_zero_drains_remote_returns_without_calling_the_factory() {
    thread::spawn(|| {
        let provider = provider();
        let guard = Guard::acquire(provider).unwrap();
        let address = item_address(&guard);
        thread::spawn(move || drop(guard)).join().unwrap();
        assert_eq!(POOL.with(FixedThreadLocalPool::stored_len), 0);
        assert_eq!(
            provider
                .warm(0, || -> Result<Item, ()> {
                    panic!("zero warm must not create values")
                })
                .unwrap(),
            0
        );
        assert_eq!(POOL.with(FixedThreadLocalPool::stored_len), 1);
        assert_eq!(item_address(&Guard::acquire(provider).unwrap()), address);
    })
    .join()
    .unwrap();
}

#[test]
fn warming_below_the_existing_size_preserves_all_stored_values() {
    thread::spawn(|| {
        let provider = &PREWARMED_POOL;
        assert_eq!(provider.warm(3, || Ok::<_, ()>(Item(42))).unwrap(), 3);
        for count in [0, 1, 2, 3, usize::MAX] {
            assert_eq!(
                provider
                    .warm(count, || -> Result<Item, ()> {
                        panic!("full pool must not create")
                    })
                    .unwrap(),
                0
            );
            assert_eq!(PREWARMED_POOL.with(FixedThreadLocalPool::stored_len), 3);
        }
    })
    .join()
    .unwrap();
}

#[test]
fn warming_factory_can_reenter_and_fill_storage_before_outer_insertion() {
    thread::spawn(|| {
        let provider = &REENTRANT_POOL;
        let mut calls = 0;
        let inserted = provider
            .warm(2, || {
                calls += 1;
                if calls == 1 {
                    drop(PoolGuard::acquire_with(provider, || Item(10)).unwrap());
                }
                Ok::<_, ()>(Item(20 + calls))
            })
            .unwrap();
        assert_eq!(inserted, 1);
        assert_eq!(calls, 2);
        assert_eq!(REENTRANT_POOL.with(FixedThreadLocalPool::stored_len), 2);
        let newest = provider
            .take(|| Err::<Item, _>("must already exist"))
            .unwrap();
        let nested = provider
            .take(|| Err::<Item, _>("must already exist"))
            .unwrap();
        assert_eq!(newest.0, 21);
        assert_eq!(nested.0, 0);
    })
    .join()
    .unwrap();
}

#[test]
fn returning_provider_entries_directly_preserves_the_value_without_resetting() {
    thread::spawn(|| {
        let provider = provider();
        let entry = provider.take(|| Ok::<_, ()>(Item(42))).unwrap();
        let address = item_address(&entry);
        assert!(provider.return_entry(entry).is_ok());
        let reused = provider
            .take(|| Err::<Item, _>("must already exist"))
            .unwrap();
        assert_eq!(item_address(&reused), address);
        assert_eq!(reused.0, 42);
    })
    .join()
    .unwrap();
}
