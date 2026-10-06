use std::{
    collections::VecDeque,
    sync::{
        LazyLock, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use rylv_pool::{PoolError, PoolGuard, PoolItem, PoolProvider};

const CAPACITY: usize = 2;

#[derive(Default)]
struct Item(usize);

impl PoolItem for Item {
    fn reset(&mut self) {
        self.0 = 0;
    }
}

// This provider chooses a handle with a boxed value and inline routing data.
// The crate does not impose a Box around the entry itself.
struct Entry {
    value: Box<Item>,
    origin: usize,
}

impl std::ops::Deref for Entry {
    type Target = Item;
    fn deref(&self) -> &Item {
        &self.value
    }
}

impl std::ops::DerefMut for Entry {
    fn deref_mut(&mut self) -> &mut Item {
        &mut self.value
    }
}

// SAFETY: moving Entry never moves the boxed Item.
unsafe impl stable_deref_trait::StableDeref for Entry {}

static CURRENT_POOL_ID: AtomicUsize = AtomicUsize::new(0);
static QUEUES: LazyLock<[Mutex<VecDeque<Entry>>; 2]> =
    LazyLock::new(|| std::array::from_fn(|_| Mutex::new(VecDeque::new())));

#[derive(Clone, Copy)]
struct QueuePoolProvider;

impl QueuePoolProvider {
    fn current_pool_id(self) -> usize {
        CURRENT_POOL_ID.load(Ordering::Relaxed)
    }
}

impl PoolProvider<Item> for QueuePoolProvider {
    type Entry = Entry;

    fn take<F, E>(&self, create: F) -> Result<Entry, PoolError<E>>
    where
        F: FnOnce() -> Result<Item, E>,
    {
        PoolError::catch(|| {
            let pool_id = self.current_pool_id();
            if let Some(entry) = QUEUES[pool_id].lock().unwrap().pop_back() {
                return Ok(entry);
            }

            // The factory runs after the queue lock has been released.
            Ok(Entry {
                value: Box::new(create()?),
                origin: pool_id,
            })
        })
    }

    fn return_entry(&self, entry: Entry) -> Result<(), Entry> {
        let origin = entry.origin;
        let mut queue = QUEUES[origin].lock().unwrap();
        if queue.len() == CAPACITY {
            return Err(entry);
        }
        queue.push_back(entry);
        Ok(())
    }

    fn warm<F, E>(&self, count: usize, mut create: F) -> Result<usize, PoolError<E>>
    where
        F: FnMut() -> Result<Item, E>,
    {
        PoolError::catch(|| {
            let pool_id = self.current_pool_id();
            let target = count.min(CAPACITY);
            let missing = target.saturating_sub(QUEUES[pool_id].lock().unwrap().len());
            let mut inserted = 0;

            for _ in 0..missing {
                let entry = Entry {
                    value: Box::new(create()?),
                    origin: pool_id,
                };
                match self.return_entry(entry) {
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

type Guard = PoolGuard<Item, QueuePoolProvider>;

fn item_address(item: &Item) -> usize {
    std::ptr::from_ref(item).addr()
}

#[test]
fn queue_provider_routes_returns_without_local_or_remote_concepts() {
    CURRENT_POOL_ID.store(0, Ordering::Relaxed);
    assert_eq!(
        QueuePoolProvider
            .warm(1, || Ok::<_, std::convert::Infallible>(Item::default()))
            .unwrap(),
        1
    );
    let guard = Guard::acquire(QueuePoolProvider).unwrap();
    let address = item_address(&guard);

    CURRENT_POOL_ID.store(1, Ordering::Relaxed);
    drop(guard);

    CURRENT_POOL_ID.store(0, Ordering::Relaxed);
    let reused = Guard::acquire(QueuePoolProvider).unwrap();
    assert_eq!(item_address(&reused), address);
}

#[derive(Clone, Copy)]
struct BoxProvider;

impl PoolProvider<Item> for BoxProvider {
    type Entry = Box<Item>;

    fn take<F, E>(&self, create: F) -> Result<Self::Entry, PoolError<E>>
    where
        F: FnOnce() -> Result<Item, E>,
    {
        PoolError::catch(|| create().map(Box::new))
    }

    fn return_entry(&self, entry: Self::Entry) -> Result<(), Self::Entry> {
        assert_eq!(entry.0, 0, "guard must reset rejected entries");
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
fn provider_can_use_a_plain_box_and_guard_moves_preserve_the_address() {
    fn requires_stable_deref(_: &impl stable_deref_trait::StableDeref) {}

    let mut guard = PoolGuard::acquire_with(BoxProvider, || Item(42)).unwrap();
    requires_stable_deref(&guard);
    let address = item_address(&guard);
    guard.0 = 99;

    let moved = std::hint::black_box(guard);
    assert_eq!(item_address(&moved), address);
    assert_eq!(moved.0, 99);
    drop(moved);
}
