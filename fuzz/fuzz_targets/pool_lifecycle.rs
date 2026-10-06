#![no_main]

use std::{
    convert::Infallible,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

use libfuzzer_sys::fuzz_target;
use rylv_pool::{
    FixedThreadLocalPool, FixedThreadLocalPoolGuard, PoolError, PoolItem, PoolProvider,
};

const CAPACITY: usize = 4;
const MAX_STEPS: usize = 64;
const MAX_GUARDS: usize = 8;
const MAX_REMOTE_BATCHES: usize = 4;

#[derive(Default)]
struct Counts {
    created: AtomicUsize,
    resets: AtomicUsize,
    dropped: AtomicUsize,
}

struct Item {
    counts: Arc<Counts>,
    id: usize,
    dirty: bool,
}

impl Item {
    fn new(counts: &Arc<Counts>) -> Self {
        let id = counts.created.fetch_add(1, Ordering::Relaxed) + 1;
        Self {
            counts: Arc::clone(counts),
            id,
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
            "a dirty guard value was destroyed without reset"
        );
        self.counts.dropped.fetch_add(1, Ordering::Relaxed);
    }
}

thread_local! {
    static POOL: FixedThreadLocalPool<Item, CAPACITY> = FixedThreadLocalPool::new();
}

type Guard = FixedThreadLocalPoolGuard<Item, CAPACITY>;

struct Held {
    guard: Guard,
    address: usize,
}

impl Held {
    fn new(guard: Guard) -> Self {
        assert!(
            !guard.dirty,
            "acquired values must be created clean or reset"
        );
        let address = std::ptr::from_ref(&*guard).addr();
        Self { guard, address }
    }

    fn assert_stable(&self) {
        assert_eq!(std::ptr::from_ref(&*self.guard).addr(), self.address);
    }
}

fn assert_exclusive(held: &[Held]) {
    for (index, entry) in held.iter().enumerate() {
        entry.assert_stable();
        for other in &held[index + 1..] {
            assert_ne!(entry.guard.id, other.guard.id);
            assert_ne!(entry.address, other.address);
        }
    }
}

fn drop_remote(mut batch: Vec<Held>) {
    thread::spawn(move || {
        for entry in &mut batch {
            entry.assert_stable();
            entry.guard.dirty = true;
        }
        drop(batch);
    })
    .join()
    .unwrap();
}

fn run_script(data: &[u8], counts: &Arc<Counts>) -> (Vec<Held>, usize) {
    let mut held = Vec::<Held>::new();
    let mut expected_resets = 0;
    let mut remote_batches = 0;

    for &byte in data {
        let index = usize::from(byte >> 4) % held.len().max(1);
        match byte % 12 {
            0 if held.len() < MAX_GUARDS => {
                let guard = Guard::acquire_with(&POOL, || Item::new(counts)).unwrap();
                held.push(Held::new(guard));
            }
            1 if held.len() < MAX_GUARDS => {
                let mut called = false;
                let result = Guard::try_acquire_with(&POOL, || {
                    called = true;
                    Err::<Item, _>(byte)
                });
                match result {
                    Ok(guard) => {
                        assert!(!called, "a pooled value must bypass the factory");
                        held.push(Held::new(guard));
                    }
                    Err(PoolError::Factory(error)) => {
                        assert!(called);
                        assert_eq!(error, byte);
                    }
                    Err(PoolError::Panic(error)) => {
                        panic!("a nonpanicking factory produced a panic error: {error}");
                    }
                }
            }
            2 if held.len() < MAX_GUARDS => {
                let mut called = false;
                let result = Guard::acquire_with(&POOL, || {
                    called = true;
                    // resume_unwind exercises capture without calling a panic hook.
                    std::panic::resume_unwind(Box::new("fuzz factory panic"));
                });
                match result {
                    Ok(guard) => {
                        assert!(!called);
                        held.push(Held::new(guard));
                    }
                    Err(PoolError::Panic(error)) => {
                        assert!(called);
                        assert_eq!(
                            error.into_payload().downcast_ref::<&str>(),
                            Some(&"fuzz factory panic")
                        );
                    }
                    Err(PoolError::Factory(never)) => match never {},
                }
            }
            3 if !held.is_empty() => {
                held[index].assert_stable();
                held[index].guard.dirty = true;
            }
            4 if !held.is_empty() => {
                let mut entry = held.swap_remove(index);
                entry.assert_stable();
                entry.guard.dirty = true;
                expected_resets += 1;
                drop(entry);
            }
            5 if !held.is_empty() && remote_batches < MAX_REMOTE_BATCHES => {
                let entry = held.swap_remove(index);
                expected_resets += 1;
                remote_batches += 1;
                drop_remote(vec![entry]);
            }
            6 => {
                let success_budget = usize::from(byte >> 4) % (CAPACITY + 1);
                let mut created = 0;
                let result = (&POOL).warm(usize::from(byte >> 4), || {
                    if created == success_budget {
                        return Err(byte);
                    }
                    created += 1;
                    Ok(Item::new(counts))
                });
                match result {
                    Ok(inserted) => assert_eq!(inserted, created),
                    Err(PoolError::Factory(error)) => assert_eq!(error, byte),
                    Err(PoolError::Panic(error)) => {
                        panic!("a nonpanicking warm factory produced a panic error: {error}");
                    }
                }
            }
            7 if !held.is_empty() => {
                let moved = held.swap_remove(index);
                moved.assert_stable();
                held.push(moved);
                held.rotate_left(1);
            }
            8 if held.len() < MAX_GUARDS => {
                let guard = Guard::acquire_with(&POOL, || {
                    let nested = Guard::acquire_with(&POOL, || Item::new(counts)).unwrap();
                    assert!(!nested.dirty);
                    expected_resets += 1;
                    drop(nested);
                    Item::new(counts)
                })
                .unwrap();
                held.push(Held::new(guard));
            }
            9 => {
                let mut created = 0;
                let inserted = (&POOL)
                    .warm(usize::from(byte >> 4), || {
                        created += 1;
                        Ok::<_, Infallible>(Item::new(counts))
                    })
                    .unwrap();
                assert_eq!(inserted, created);
                assert!(inserted <= CAPACITY);
            }
            10 if !held.is_empty() && remote_batches < MAX_REMOTE_BATCHES => {
                let batch = held.split_off(index);
                expected_resets += batch.len();
                remote_batches += 1;
                drop_remote(batch);
            }
            11 => {
                // Drain remote returns even when local storage is already full.
                assert_eq!(
                    (&POOL)
                        .warm(0, || -> Result<Item, Infallible> {
                            panic!("warming to zero must not invoke the factory")
                        })
                        .unwrap(),
                    0
                );
            }
            _ => {}
        }
        assert_exclusive(&held);
    }

    // Keep active guards alive after the origin thread and its TLS are destroyed.
    (held, expected_resets)
}

fuzz_target!(|data: &[u8]| {
    let script: Vec<_> = data.iter().copied().take(MAX_STEPS).collect();
    let counts = Arc::new(Counts::default());
    let worker_counts = Arc::clone(&counts);
    let (mut survivors, mut expected_resets) =
        thread::spawn(move || run_script(&script, &worker_counts))
            .join()
            .unwrap();

    assert_exclusive(&survivors);
    expected_resets += survivors.len();
    let remote = survivors.split_off(survivors.len() / 2);
    for entry in &mut survivors {
        entry.guard.dirty = true;
    }
    drop(survivors);
    if !remote.is_empty() {
        drop_remote(remote);
    }

    assert_eq!(counts.resets.load(Ordering::Relaxed), expected_resets);
    assert_eq!(
        counts.created.load(Ordering::Relaxed),
        counts.dropped.load(Ordering::Relaxed),
        "each created value must be destroyed exactly once after origin shutdown"
    );
});
