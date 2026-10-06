//! Small ownership and concurrency scenarios suitable for Miri and normal CI.

use std::{
    convert::Infallible,
    sync::{
        Arc, Barrier,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
};

use rylv_pool::{
    FixedThreadLocalPool, FixedThreadLocalPoolGuard, PoolError, PoolGuard, PoolItem, PoolProvider,
};

#[derive(Default)]
struct Counts {
    created: AtomicUsize,
    resets: AtomicUsize,
    dropped: AtomicUsize,
}

impl Counts {
    fn assert_finished(&self, created: usize, resets: usize) {
        assert_eq!(self.created.load(Ordering::Relaxed), created);
        assert_eq!(self.resets.load(Ordering::Relaxed), resets);
        assert_eq!(self.dropped.load(Ordering::Relaxed), created);
    }
}

struct Item {
    counts: Arc<Counts>,
    dirty: bool,
}

impl Item {
    fn new(counts: &Arc<Counts>) -> Self {
        counts.created.fetch_add(1, Ordering::Relaxed);
        Self {
            counts: Arc::clone(counts),
            dirty: false,
        }
    }
}

impl PoolItem for Item {
    fn reset(&mut self) {
        self.dirty = false;
        self.counts.resets.fetch_add(1, Ordering::Relaxed);
    }
}

impl Drop for Item {
    fn drop(&mut self) {
        assert!(
            !self.dirty,
            "a guard must reset an item before destroying it"
        );
        self.counts.dropped.fetch_add(1, Ordering::Relaxed);
    }
}

thread_local! {
    static POOL: FixedThreadLocalPool<Item, 2> = FixedThreadLocalPool::new();
    static ZERO_POOL: FixedThreadLocalPool<Item, 0> = FixedThreadLocalPool::new();
    static REENTRANT_POOL: FixedThreadLocalPool<ReentrantItem, 1> = FixedThreadLocalPool::new();
}

type Guard = FixedThreadLocalPoolGuard<Item, 2>;

fn address(item: &Item) -> usize {
    std::ptr::from_ref(item).addr()
}

#[test]
fn local_moves_keep_the_value_stable_and_returns_reset_it() {
    let counts = Arc::new(Counts::default());
    let worker_counts = Arc::clone(&counts);
    thread::spawn(move || {
        let mut guard = Guard::acquire_with(&POOL, || Item::new(&worker_counts)).unwrap();
        let original = address(&guard);
        guard.dirty = true;

        let mut guards = vec![guard];
        guards.reserve(4);
        assert_eq!(address(&guards[0]), original);
        let moved = guards.pop().unwrap();
        assert_eq!(address(&moved), original);
        assert!(moved.dirty);
        drop(moved);

        let reused = Guard::acquire_with(&POOL, || panic!("local return must be reused")).unwrap();
        assert_eq!(address(&reused), original);
        assert!(!reused.dirty);
        drop(reused);
    })
    .join()
    .unwrap();
    counts.assert_finished(1, 2);
}

#[test]
fn zero_capacity_rejects_local_and_remote_returns_after_reset() {
    let counts = Arc::new(Counts::default());
    let worker_counts = Arc::clone(&counts);
    thread::spawn(move || {
        let provider = &ZERO_POOL;
        let local = PoolGuard::acquire_with(provider, || Item::new(&worker_counts)).unwrap();
        drop(local);
        assert_eq!(worker_counts.dropped.load(Ordering::Relaxed), 1);

        let mut remote = PoolGuard::acquire_with(provider, || Item::new(&worker_counts)).unwrap();
        remote.dirty = true;
        thread::spawn(move || drop(remote)).join().unwrap();
        assert_eq!(worker_counts.dropped.load(Ordering::Relaxed), 1);

        // Warming with zero capacity still drains and destroys remote overflow.
        assert_eq!(
            provider
                .warm(usize::MAX, || -> Result<Item, Infallible> {
                    panic!("zero capacity must not create values")
                })
                .unwrap(),
            0
        );
        assert_eq!(worker_counts.dropped.load(Ordering::Relaxed), 2);
    })
    .join()
    .unwrap();
    counts.assert_finished(2, 2);
}

#[derive(Clone, Copy)]
struct RejectProvider;

impl PoolProvider<Item> for RejectProvider {
    type Entry = Box<Item>;

    fn take<F, E>(&self, create: F) -> Result<Self::Entry, PoolError<E>>
    where
        F: FnOnce() -> Result<Item, E>,
    {
        PoolError::catch(|| create().map(Box::new))
    }

    fn return_entry(&self, entry: Self::Entry) -> Result<(), Self::Entry> {
        assert!(!entry.dirty);
        Err(entry)
    }

    fn warm<F, E>(&self, _count: usize, _create: F) -> Result<usize, PoolError<E>>
    where
        F: FnMut() -> Result<Item, E>,
    {
        Ok(0)
    }
}

#[test]
fn generic_box_entries_are_reset_and_destroyed_once_when_rejected() {
    let counts = Arc::new(Counts::default());
    let mut guard = PoolGuard::acquire_with(RejectProvider, || Item::new(&counts)).unwrap();
    let original = address(&guard);
    guard.dirty = true;
    thread::spawn(move || {
        assert_eq!(address(&guard), original);
        drop(guard);
    })
    .join()
    .unwrap();
    counts.assert_finished(1, 1);
}

#[test]
fn concurrent_remote_producers_and_the_origin_consumer_do_not_lose_entries() {
    let counts = Arc::new(Counts::default());
    let worker_counts = Arc::clone(&counts);
    thread::spawn(move || {
        let mut guards: Vec<_> = (0..4)
            .map(|_| Guard::acquire_with(&POOL, || Item::new(&worker_counts)).unwrap())
            .collect();
        for guard in &mut guards {
            guard.dirty = true;
        }
        let right = guards.split_off(2);
        let barrier = Arc::new(Barrier::new(3));
        let left_barrier = Arc::clone(&barrier);
        let left = thread::spawn(move || {
            left_barrier.wait();
            drop(guards);
        });
        let right_barrier = Arc::clone(&barrier);
        let right = thread::spawn(move || {
            right_barrier.wait();
            drop(right);
        });

        barrier.wait();
        let first = Guard::acquire_with(&POOL, || Item::new(&worker_counts)).unwrap();
        let second = Guard::acquire_with(&POOL, || Item::new(&worker_counts)).unwrap();
        assert!(!first.dirty);
        assert!(!second.dirty);
        assert_ne!(address(&first), address(&second));
        drop(first);
        drop(second);
        left.join().unwrap();
        right.join().unwrap();
        assert_eq!(
            (&POOL)
                .warm(0, || -> Result<Item, Infallible> {
                    panic!("draining must not create values")
                })
                .unwrap(),
            0
        );
    })
    .join()
    .unwrap();
    let created = counts.created.load(Ordering::Relaxed);
    assert!((4..=6).contains(&created));
    counts.assert_finished(created, 6);
}

#[test]
fn remote_overflow_keeps_newest_entries_and_destroys_older_entries_once() {
    let counts = Arc::new(Counts::default());
    let worker_counts = Arc::clone(&counts);
    thread::spawn(move || {
        let guards: Vec<_> = (0..4)
            .map(|_| Guard::acquire_with(&POOL, || Item::new(&worker_counts)).unwrap())
            .collect();
        let newest = address(guards.last().unwrap());
        thread::spawn(move || {
            for guard in guards {
                drop(guard);
            }
        })
        .join()
        .unwrap();
        assert_eq!(worker_counts.dropped.load(Ordering::Relaxed), 0);

        let reused = Guard::acquire_with(&POOL, || panic!("remote values must be reused")).unwrap();
        assert_eq!(address(&reused), newest);
        assert!(!reused.dirty);
        assert_eq!(worker_counts.dropped.load(Ordering::Relaxed), 2);
        drop(reused);
    })
    .join()
    .unwrap();
    counts.assert_finished(4, 5);
}

#[test]
fn remote_returns_after_origin_exit_destroy_values_once() {
    let counts = Arc::new(Counts::default());
    let worker_counts = Arc::clone(&counts);
    let mut guards = thread::spawn(move || {
        (0..2)
            .map(|_| Guard::acquire_with(&POOL, || Item::new(&worker_counts)).unwrap())
            .collect::<Vec<_>>()
    })
    .join()
    .unwrap();

    let second = guards.pop().unwrap();
    let first = guards.pop().unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let first_barrier = Arc::clone(&barrier);
    let first = thread::spawn(move || {
        first_barrier.wait();
        drop(first);
    });
    let second_barrier = Arc::clone(&barrier);
    let second = thread::spawn(move || {
        second_barrier.wait();
        drop(second);
    });
    barrier.wait();
    first.join().unwrap();
    second.join().unwrap();
    counts.assert_finished(2, 2);
}

#[test]
fn origin_exit_destroys_an_undrained_remote_batch_once() {
    let counts = Arc::new(Counts::default());
    let worker_counts = Arc::clone(&counts);
    thread::spawn(move || {
        let guards: Vec<_> = (0..3)
            .map(|_| Guard::acquire_with(&POOL, || Item::new(&worker_counts)).unwrap())
            .collect();
        thread::spawn(move || drop(guards)).join().unwrap();
        assert_eq!(worker_counts.dropped.load(Ordering::Relaxed), 0);
        // The origin exits without consuming the published intrusive list.
    })
    .join()
    .unwrap();
    counts.assert_finished(3, 3);
}

#[test]
fn origin_shutdown_races_remote_returns_without_losing_entries() {
    let counts = Arc::new(Counts::default());
    let worker_counts = Arc::clone(&counts);
    let producers = thread::spawn(move || {
        let first = Guard::acquire_with(&POOL, || Item::new(&worker_counts)).unwrap();
        let second = Guard::acquire_with(&POOL, || Item::new(&worker_counts)).unwrap();
        let barrier = Arc::new(Barrier::new(3));
        let first_barrier = Arc::clone(&barrier);
        let first = thread::spawn(move || {
            first_barrier.wait();
            drop(first);
        });
        let second_barrier = Arc::clone(&barrier);
        let second = thread::spawn(move || {
            second_barrier.wait();
            drop(second);
        });

        // Either publication or TLS destruction may win this race. The queue
        // must retain ownership until each successfully published node is freed.
        barrier.wait();
        [first, second]
    })
    .join()
    .unwrap();
    for producer in producers {
        producer.join().unwrap();
    }
    counts.assert_finished(2, 2);
}

struct ReentrantItem {
    counts: Arc<Counts>,
    enabled: Arc<AtomicBool>,
    reentered: Arc<AtomicUsize>,
}

impl ReentrantItem {
    fn new(counts: &Arc<Counts>, enabled: &Arc<AtomicBool>, reentered: &Arc<AtomicUsize>) -> Self {
        counts.created.fetch_add(1, Ordering::Relaxed);
        Self {
            counts: Arc::clone(counts),
            enabled: Arc::clone(enabled),
            reentered: Arc::clone(reentered),
        }
    }
}

impl PoolItem for ReentrantItem {
    fn reset(&mut self) {
        self.counts.resets.fetch_add(1, Ordering::Relaxed);
    }
}

impl Drop for ReentrantItem {
    fn drop(&mut self) {
        self.counts.dropped.fetch_add(1, Ordering::Relaxed);
        if self.enabled.swap(false, Ordering::Relaxed) {
            self.reentered.fetch_add(1, Ordering::Relaxed);
            let guard = PoolGuard::acquire_with(&REENTRANT_POOL, || {
                panic!("a retained value must be available during overflow destruction")
            })
            .unwrap();
            drop(guard);
        }
    }
}

#[test]
fn remote_overflow_destruction_can_reenter_storage_without_aliasing() {
    let counts = Arc::new(Counts::default());
    let worker_counts = Arc::clone(&counts);
    let reentered = Arc::new(AtomicUsize::new(0));
    let worker_reentered = Arc::clone(&reentered);
    thread::spawn(move || {
        let enabled = Arc::new(AtomicBool::new(false));
        let guards: Vec<_> = (0..2)
            .map(|_| {
                PoolGuard::acquire_with(&REENTRANT_POOL, || {
                    ReentrantItem::new(&worker_counts, &enabled, &worker_reentered)
                })
                .unwrap()
            })
            .collect();
        thread::spawn(move || drop(guards)).join().unwrap();

        enabled.store(true, Ordering::Relaxed);
        let guard = PoolGuard::acquire_with(&REENTRANT_POOL, || {
            panic!("the retained remote entry must remain available")
        })
        .unwrap();
        assert_eq!(worker_reentered.load(Ordering::Relaxed), 1);
        drop(guard);
    })
    .join()
    .unwrap();
    assert_eq!(reentered.load(Ordering::Relaxed), 1);
    counts.assert_finished(2, 4);
}
