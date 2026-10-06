use super::*;

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

impl<T: PoolItem, P: PoolProvider<T>> PoolGuard<T, P> {
    pub(crate) fn entry(&self) -> &P::Entry {
        &self.node
    }
}

fn assert_storage_invariants<E, const N: usize>(storage: &Storage<E, N>) {
    assert!(storage.len <= N);
    assert_eq!(storage.len(), storage.len);
    assert!(storage.slots[..storage.len].iter().all(Option::is_some));
    assert!(storage.slots[storage.len..].iter().all(Option::is_none));
}

#[test]
fn storage_can_be_initialized_in_a_const_context() {
    const EMPTY: Storage<usize, 3> = Storage::new();
    let mut storage = EMPTY;

    assert_storage_invariants(&storage);
    assert_eq!(storage.len(), 0);
    assert_eq!(storage.pop(), None);
    assert_storage_invariants(&storage);
}

#[test]
fn zero_capacity_storage_rejects_without_consuming_an_iterator() {
    let mut storage = Storage::<usize, 0>::new();
    assert_eq!(storage.try_push(42), Err(42));
    assert_eq!(storage.pop(), None);

    let mut entries = [3, 2, 1].into_iter();
    storage.extend_newest_first(&mut entries);

    assert_eq!(entries.collect::<Vec<_>>(), [3, 2, 1]);
    assert_eq!(storage.len(), 0);
    assert_storage_invariants(&storage);
}

#[test]
fn storage_is_lifo_and_full_rejection_keeps_existing_entries() {
    let mut storage = Storage::<usize, 3>::new();
    for value in 1..=3 {
        assert_eq!(storage.try_push(value), Ok(()));
        assert_storage_invariants(&storage);
    }
    assert_eq!(storage.try_push(4), Err(4));
    assert_eq!(storage.len(), 3);

    for value in (1..=3).rev() {
        assert_eq!(storage.pop(), Some(value));
        assert_storage_invariants(&storage);
    }
    assert_eq!(storage.pop(), None);
    assert_eq!(storage.pop(), None);
    assert_storage_invariants(&storage);
}

#[test]
fn storage_reuses_slots_after_popping() {
    let mut storage = Storage::<usize, 2>::new();
    assert_eq!(storage.try_push(1), Ok(()));
    assert_eq!(storage.try_push(2), Ok(()));
    assert_eq!(storage.pop(), Some(2));
    assert_eq!(storage.try_push(3), Ok(()));
    assert_eq!(storage.pop(), Some(3));
    assert_eq!(storage.pop(), Some(1));
    assert_eq!(storage.try_push(4), Ok(()));
    assert_eq!(storage.pop(), Some(4));
    assert_storage_invariants(&storage);
}

#[test]
fn storage_appends_newest_entries_without_reversing_old_entries() {
    let mut storage = Storage::<usize, 5>::new();
    assert_eq!(storage.try_push(1), Ok(()));
    assert_eq!(storage.try_push(2), Ok(()));
    let mut entries = [5, 4, 3].into_iter();

    storage.extend_newest_first(&mut entries);

    assert_eq!(entries.next(), None);
    assert_storage_invariants(&storage);
    for value in [5, 4, 3, 2, 1] {
        assert_eq!(storage.pop(), Some(value));
        assert_storage_invariants(&storage);
    }
    assert_eq!(storage.pop(), None);
}

#[test]
fn storage_leaves_older_overflow_in_the_iterator() {
    let mut storage = Storage::<usize, 3>::new();
    assert_eq!(storage.try_push(1), Ok(()));
    let mut entries = [8, 7, 6, 5].into_iter();

    storage.extend_newest_first(&mut entries);

    assert_eq!(entries.collect::<Vec<_>>(), [6, 5]);
    assert_eq!(storage.pop(), Some(8));
    assert_eq!(storage.pop(), Some(7));
    assert_eq!(storage.pop(), Some(1));
    assert_storage_invariants(&storage);
}

#[test]
fn extending_empty_or_full_storage_does_not_change_pop_order() {
    let mut storage = Storage::<usize, 2>::new();
    assert_eq!(storage.try_push(1), Ok(()));
    let mut empty = std::iter::empty();
    storage.extend_newest_first(&mut empty);
    assert_eq!(storage.try_push(2), Ok(()));
    let mut overflow = [4, 3].into_iter();
    storage.extend_newest_first(&mut overflow);

    assert_eq!(overflow.collect::<Vec<_>>(), [4, 3]);
    assert_eq!(storage.pop(), Some(2));
    assert_eq!(storage.pop(), Some(1));
    assert_storage_invariants(&storage);
}

#[test]
fn storage_preserves_order_across_multiple_extensions() {
    let mut storage = Storage::<usize, 5>::new();
    storage.extend_newest_first(&mut [2, 1].into_iter());
    storage.extend_newest_first(&mut [4, 3].into_iter());
    assert_eq!(storage.pop(), Some(4));
    storage.extend_newest_first(&mut [6, 5].into_iter());

    for value in [6, 5, 3, 2, 1] {
        assert_eq!(storage.pop(), Some(value));
        assert_storage_invariants(&storage);
    }
    assert_eq!(storage.pop(), None);
}

struct DropProbe {
    id: usize,
    drops: Arc<Mutex<Vec<usize>>>,
}

impl DropProbe {
    fn new(id: usize, drops: &Arc<Mutex<Vec<usize>>>) -> Self {
        Self {
            id,
            drops: Arc::clone(drops),
        }
    }
}

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.drops.lock().unwrap().push(self.id);
    }
}

#[test]
fn storage_transfers_ownership_without_early_or_duplicate_drops() {
    let drops = Arc::new(Mutex::new(Vec::new()));
    let mut storage = Storage::<DropProbe, 2>::new();
    assert!(storage.try_push(DropProbe::new(1, &drops)).is_ok());
    assert!(storage.try_push(DropProbe::new(2, &drops)).is_ok());

    let rejected = storage.try_push(DropProbe::new(3, &drops)).err().unwrap();
    assert_eq!(rejected.id, 3);
    assert!(drops.lock().unwrap().is_empty());
    drop(rejected);
    assert_eq!(*drops.lock().unwrap(), [3]);

    let popped = storage.pop().unwrap();
    assert_eq!(popped.id, 2);
    assert_eq!(*drops.lock().unwrap(), [3]);
    drop(storage);
    assert_eq!(*drops.lock().unwrap(), [3, 1]);
    drop(popped);
    assert_eq!(*drops.lock().unwrap(), [3, 1, 2]);
}

#[test]
fn storage_and_its_overflow_iterator_own_disjoint_entries() {
    let drops = Arc::new(Mutex::new(Vec::new()));
    let mut storage = Storage::<DropProbe, 2>::new();
    let mut entries = (1..=4)
        .map(|id| DropProbe::new(id, &drops))
        .collect::<Vec<_>>()
        .into_iter();

    storage.extend_newest_first(&mut entries);
    assert!(drops.lock().unwrap().is_empty());
    drop(entries);
    assert_eq!(*drops.lock().unwrap(), [3, 4]);
    drop(storage);
    let mut observed = drops.lock().unwrap().clone();
    observed.sort_unstable();
    assert_eq!(observed, [1, 2, 3, 4]);
}

#[test]
fn storage_remains_packed_when_an_incoming_iterator_panics() {
    let drops = Arc::new(Mutex::new(Vec::new()));
    let mut storage = Storage::<DropProbe, 3>::new();
    let mut calls = 0;
    let mut entries = std::iter::from_fn(|| {
        calls += 1;
        assert!(calls != 3, "iterator panic");
        Some(DropProbe::new(calls, &drops))
    });

    let result = catch_unwind(AssertUnwindSafe(|| {
        storage.extend_newest_first(&mut entries);
    }));

    assert!(result.is_err());
    assert_eq!(storage.len(), 2);
    assert_storage_invariants(&storage);
    assert!(drops.lock().unwrap().is_empty());
    drop(storage);
    let mut observed = drops.lock().unwrap().clone();
    observed.sort_unstable();
    assert_eq!(observed, [1, 2]);
}

fn check_storage_against_model<const N: usize>() {
    // Exhaust all push/pop sequences of length eight, independently for each
    // small capacity. Assert the packed-slot invariant after every operation.
    for operations in 0_u16..256 {
        let mut storage = Storage::<usize, N>::new();
        let mut model = Vec::new();
        for step in 0..8 {
            if operations & (1 << step) == 0 {
                assert_eq!(storage.pop(), model.pop());
            } else if model.len() < N {
                model.push(step);
                assert_eq!(storage.try_push(step), Ok(()));
            } else {
                assert_eq!(storage.try_push(step), Err(step));
            }
            assert_eq!(storage.len(), model.len());
            assert_storage_invariants(&storage);
        }
        while let Some(value) = model.pop() {
            assert_eq!(storage.pop(), Some(value));
        }
        assert_eq!(storage.pop(), None);
        assert_storage_invariants(&storage);
    }
}

#[test]
fn storage_matches_a_reference_stack_for_small_capacities() {
    check_storage_against_model::<0>();
    check_storage_against_model::<1>();
    check_storage_against_model::<2>();
    check_storage_against_model::<3>();
}

#[derive(Default)]
struct Lifecycle {
    resets: AtomicUsize,
    drops: AtomicUsize,
    panic_on_reset: AtomicBool,
    provider_access: AtomicBool,
    drops_during_provider_access: AtomicUsize,
    events: Mutex<Vec<&'static str>>,
}

#[derive(Default)]
struct TestItem {
    value: usize,
    lifecycle: Arc<Lifecycle>,
}

impl TestItem {
    fn new(value: usize, lifecycle: &Arc<Lifecycle>) -> Self {
        Self {
            value,
            lifecycle: Arc::clone(lifecycle),
        }
    }
}

impl PoolItem for TestItem {
    fn reset(&mut self) {
        self.lifecycle.events.lock().unwrap().push("reset");
        self.lifecycle.resets.fetch_add(1, Ordering::Relaxed);
        self.value = 0;
        assert!(
            !self.lifecycle.panic_on_reset.load(Ordering::Relaxed),
            "reset panic"
        );
    }
}

impl Drop for TestItem {
    fn drop(&mut self) {
        if self.lifecycle.provider_access.load(Ordering::Relaxed) {
            self.lifecycle
                .drops_during_provider_access
                .fetch_add(1, Ordering::Relaxed);
        }
        self.lifecycle.events.lock().unwrap().push("item drop");
        self.lifecycle.drops.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Default)]
struct ProviderState {
    entry: Mutex<Option<Box<TestItem>>>,
    takes: AtomicUsize,
    returns: AtomicUsize,
    drops: AtomicUsize,
    returned_value: AtomicUsize,
    lifecycle: Arc<Lifecycle>,
}

#[derive(Clone, Copy)]
enum TakeMode {
    CatchFactory,
    UncaughtFactory,
    Panic,
    ReturnedPanic,
}

#[derive(Clone, Copy)]
enum ReturnMode {
    Store,
    Reject,
    Panic,
}

#[derive(Clone)]
struct TestProvider {
    state: Arc<ProviderState>,
    take_mode: TakeMode,
    return_mode: ReturnMode,
}

struct ProviderAccess<'a>(&'a Lifecycle);

impl Drop for ProviderAccess<'_> {
    fn drop(&mut self) {
        self.0.provider_access.store(false, Ordering::Relaxed);
    }
}

impl PoolProvider<TestItem> for TestProvider {
    type Entry = Box<TestItem>;

    fn take<F, E>(&self, create: F) -> Result<Self::Entry, PoolError<E>>
    where
        F: FnOnce() -> Result<TestItem, E>,
    {
        self.state.takes.fetch_add(1, Ordering::Relaxed);
        match self.take_mode {
            TakeMode::Panic => panic!("provider take panic"),
            TakeMode::ReturnedPanic => {
                return PoolError::catch(|| panic!("provider captured panic"));
            }
            TakeMode::CatchFactory | TakeMode::UncaughtFactory => {}
        }
        let stored = self.state.entry.lock().unwrap().take();
        if let Some(entry) = stored {
            return Ok(entry);
        }
        match self.take_mode {
            TakeMode::CatchFactory => PoolError::catch(|| create().map(Box::new)),
            TakeMode::UncaughtFactory => create().map(Box::new).map_err(PoolError::Factory),
            TakeMode::Panic | TakeMode::ReturnedPanic => unreachable!(),
        }
    }

    fn return_entry(&self, entry: Self::Entry) -> Result<(), Self::Entry> {
        self.state.returns.fetch_add(1, Ordering::Relaxed);
        self.state
            .returned_value
            .store(entry.value, Ordering::Relaxed);
        self.state.lifecycle.events.lock().unwrap().push("return");
        assert!(
            !self
                .state
                .lifecycle
                .provider_access
                .swap(true, Ordering::Relaxed)
        );
        let _access = ProviderAccess(&self.state.lifecycle);

        match self.return_mode {
            ReturnMode::Store => {
                let mut stored = self.state.entry.lock().unwrap();
                if stored.is_some() {
                    drop(stored);
                    Err(entry)
                } else {
                    *stored = Some(entry);
                    drop(stored);
                    Ok(())
                }
            }
            ReturnMode::Reject => Err(entry),
            ReturnMode::Panic => panic!("provider return panic"),
        }
    }

    fn warm<F, E>(&self, _count: usize, _create: F) -> Result<usize, PoolError<E>>
    where
        F: FnMut() -> Result<TestItem, E>,
    {
        Ok(0)
    }
}

impl Drop for TestProvider {
    fn drop(&mut self) {
        self.state.drops.fetch_add(1, Ordering::Relaxed);
        self.state
            .lifecycle
            .events
            .lock()
            .unwrap()
            .push("provider drop");
    }
}

fn provider(return_mode: ReturnMode) -> (TestProvider, Arc<ProviderState>) {
    let state = Arc::new(ProviderState::default());
    (
        TestProvider {
            state: Arc::clone(&state),
            take_mode: TakeMode::CatchFactory,
            return_mode,
        },
        state,
    )
}

fn address(item: &TestItem) -> usize {
    std::ptr::from_ref(item).addr()
}

#[test]
fn guard_acquire_uses_default_and_resets_before_returning() {
    let (provider, state) = provider(ReturnMode::Store);
    let mut guard = PoolGuard::acquire(provider).unwrap();
    let lifecycle = Arc::clone(&guard.lifecycle);
    assert_eq!(guard.value, 0);
    guard.value = 42;
    drop(guard);

    assert_eq!(state.takes.load(Ordering::Relaxed), 1);
    assert_eq!(state.returns.load(Ordering::Relaxed), 1);
    assert_eq!(state.returned_value.load(Ordering::Relaxed), 0);
    assert_eq!(lifecycle.resets.load(Ordering::Relaxed), 1);
    assert_eq!(lifecycle.drops.load(Ordering::Relaxed), 0);
    assert_eq!(state.drops.load(Ordering::Relaxed), 1);
    drop(state.entry.lock().unwrap().take());
    assert_eq!(lifecycle.drops.load(Ordering::Relaxed), 1);
}

#[test]
fn guard_returns_and_reuses_the_same_reset_entry() {
    let (provider, state) = provider(ReturnMode::Store);
    let mut guard =
        PoolGuard::acquire_with(provider.clone(), || TestItem::new(42, &state.lifecycle)).unwrap();
    let original_address = address(&guard);
    guard.value = 99;
    drop(guard);

    assert_eq!(
        *state.lifecycle.events.lock().unwrap(),
        ["reset", "return", "provider drop"]
    );
    let reused = PoolGuard::acquire_with(provider, || panic!("unexpected factory call")).unwrap();
    assert_eq!(address(&reused), original_address);
    assert_eq!(reused.value, 0);
    drop(reused);

    assert_eq!(state.lifecycle.resets.load(Ordering::Relaxed), 2);
    assert_eq!(state.returns.load(Ordering::Relaxed), 2);
    assert_eq!(state.takes.load(Ordering::Relaxed), 2);
    assert_eq!(state.lifecycle.drops.load(Ordering::Relaxed), 0);
    drop(state.entry.lock().unwrap().take());
    assert_eq!(state.lifecycle.drops.load(Ordering::Relaxed), 1);
}

#[test]
fn guard_rejection_destroys_once_after_provider_access_ends() {
    let (provider, state) = provider(ReturnMode::Reject);
    let guard = PoolGuard::acquire_with(provider, || TestItem::new(42, &state.lifecycle)).unwrap();
    drop(guard);

    assert_eq!(
        *state.lifecycle.events.lock().unwrap(),
        ["reset", "return", "item drop", "provider drop"]
    );
    assert_eq!(state.lifecycle.resets.load(Ordering::Relaxed), 1);
    assert_eq!(state.lifecycle.drops.load(Ordering::Relaxed), 1);
    assert_eq!(state.returned_value.load(Ordering::Relaxed), 0);
    assert_eq!(
        state
            .lifecycle
            .drops_during_provider_access
            .load(Ordering::Relaxed),
        0
    );
}

#[test]
fn guard_accepts_a_consuming_factory_and_calls_it_once() {
    let (provider, state) = provider(ReturnMode::Reject);
    let seed = String::from("configured");
    let mut calls = 0;
    let guard = PoolGuard::acquire_with(provider, || {
        calls += 1;
        let value = seed.len();
        drop(seed);
        TestItem::new(value, &state.lifecycle)
    })
    .unwrap();

    assert_eq!(calls, 1);
    assert_eq!(guard.value, 10);
    drop(guard);
    assert_eq!(calls, 1);
}

#[test]
fn guard_drops_unused_factory_captures_on_a_pool_hit() {
    let (provider, state) = provider(ReturnMode::Reject);
    *state.entry.lock().unwrap() = Some(Box::new(TestItem::new(7, &state.lifecycle)));
    let drops = Arc::new(Mutex::new(Vec::new()));
    let capture = DropProbe::new(1, &drops);
    let guard = PoolGuard::acquire_with(provider, move || {
        drop(capture);
        panic!("factory must not run on a hit");
    })
    .unwrap();

    assert_eq!(guard.value, 7);
    assert_eq!(*drops.lock().unwrap(), [1]);
    drop(guard);
    assert_eq!(*drops.lock().unwrap(), [1]);
}

#[test]
fn guard_preserves_factory_error_ownership_and_drops_the_provider() {
    let (provider, state) = provider(ReturnMode::Reject);
    let failure = Box::new(42);
    let failure_address = std::ptr::from_ref(&*failure).addr();
    let mut calls = 0;
    let error = PoolGuard::try_acquire_with(provider, || {
        calls += 1;
        Err::<TestItem, _>(failure)
    })
    .err()
    .unwrap();
    let PoolError::Factory(failure) = error else {
        panic!("expected the original factory error");
    };

    assert_eq!(std::ptr::from_ref(&*failure).addr(), failure_address);
    assert_eq!(*failure, 42);
    assert_eq!(calls, 1);
    assert_eq!(state.drops.load(Ordering::Relaxed), 1);
    assert_eq!(state.returns.load(Ordering::Relaxed), 0);
    assert_eq!(state.lifecycle.resets.load(Ordering::Relaxed), 0);
}

#[test]
fn guard_try_acquire_with_returns_a_successful_fallible_factory_value() {
    let (provider, state) = provider(ReturnMode::Reject);
    let guard = PoolGuard::try_acquire_with(provider, || {
        Ok::<_, String>(TestItem::new(42, &state.lifecycle))
    })
    .unwrap();
    assert_eq!(guard.value, 42);
    drop(guard);
    assert_eq!(state.lifecycle.resets.load(Ordering::Relaxed), 1);
}

#[test]
fn failed_acquisition_drops_an_unused_factory_capture() {
    let (mut provider, state) = provider(ReturnMode::Reject);
    provider.take_mode = TakeMode::Panic;
    let drops = Arc::new(Mutex::new(Vec::new()));
    let capture = DropProbe::new(1, &drops);
    let error = PoolGuard::acquire_with(provider, move || {
        drop(capture);
        panic!("factory must not run after a provider panic");
    })
    .err()
    .unwrap();

    assert!(matches!(error, PoolError::Panic(_)));
    assert_eq!(*drops.lock().unwrap(), [1]);
    assert_eq!(state.drops.load(Ordering::Relaxed), 1);
    assert_eq!(state.returns.load(Ordering::Relaxed), 0);
}

#[test]
fn guard_captures_both_escaping_and_provider_captured_factory_panics() {
    for take_mode in [TakeMode::CatchFactory, TakeMode::UncaughtFactory] {
        let (mut provider, state) = provider(ReturnMode::Reject);
        provider.take_mode = take_mode;
        let mut calls = 0;
        let error = PoolGuard::acquire_with(provider, || {
            calls += 1;
            panic!("factory panic");
        })
        .err()
        .unwrap();
        let PoolError::Panic(error) = error;

        assert_eq!(error.to_string(), "factory panic");
        assert_eq!(
            error.into_payload().downcast_ref::<&str>(),
            Some(&"factory panic")
        );
        assert_eq!(calls, 1);
        assert_eq!(state.drops.load(Ordering::Relaxed), 1);
        assert_eq!(state.returns.load(Ordering::Relaxed), 0);
        assert_eq!(state.lifecycle.drops.load(Ordering::Relaxed), 0);
    }
}

#[test]
fn guard_captures_provider_panics_before_the_factory_runs() {
    for (take_mode, message) in [
        (TakeMode::Panic, "provider take panic"),
        (TakeMode::ReturnedPanic, "provider captured panic"),
    ] {
        let (mut provider, state) = provider(ReturnMode::Reject);
        provider.take_mode = take_mode;
        let mut calls = 0;
        let error = PoolGuard::acquire_with(provider, || {
            calls += 1;
            TestItem::new(42, &state.lifecycle)
        })
        .err()
        .unwrap();
        let PoolError::Panic(error) = error;

        assert_eq!(error.to_string(), message);
        assert_eq!(calls, 0);
        assert_eq!(state.takes.load(Ordering::Relaxed), 1);
        assert_eq!(state.drops.load(Ordering::Relaxed), 1);
        assert_eq!(state.returns.load(Ordering::Relaxed), 0);
    }
}

#[test]
fn guard_moves_and_mutation_preserve_the_value_address() {
    fn requires_stable_deref(_: &impl StableDeref) {}

    let (provider, state) = provider(ReturnMode::Reject);
    let mut guard =
        PoolGuard::acquire_with(provider, || TestItem::new(42, &state.lifecycle)).unwrap();
    requires_stable_deref(&guard);
    let original_address = address(&guard);
    let borrowed: &TestItem = &guard;
    assert_eq!(borrowed.value, 42);
    let borrowed: &mut TestItem = &mut guard;
    borrowed.value = 99;

    let mut guards = vec![std::hint::black_box(guard)];
    assert_eq!(address(&guards[0]), original_address);
    let moved = std::hint::black_box(guards.pop().unwrap());
    assert_eq!(address(&moved), original_address);
    assert_eq!(moved.value, 99);
    drop(moved);
    assert_eq!(state.lifecycle.drops.load(Ordering::Relaxed), 1);
}

#[test]
fn guard_can_move_to_another_thread_and_return_once() {
    fn requires_send<T: Send>() {}
    requires_send::<PoolGuard<TestItem, TestProvider>>();

    let (provider, state) = provider(ReturnMode::Store);
    let guard = PoolGuard::acquire_with(provider, || TestItem::new(42, &state.lifecycle)).unwrap();
    let original_address = address(&guard);

    std::thread::spawn(move || {
        assert_eq!(address(&guard), original_address);
        drop(guard);
    })
    .join()
    .unwrap();

    assert_eq!(state.returns.load(Ordering::Relaxed), 1);
    assert_eq!(state.lifecycle.resets.load(Ordering::Relaxed), 1);
    assert_eq!(state.lifecycle.drops.load(Ordering::Relaxed), 0);
    assert_eq!(
        address(state.entry.lock().unwrap().as_ref().unwrap()),
        original_address
    );
    drop(state.entry.lock().unwrap().take());
    assert_eq!(state.lifecycle.drops.load(Ordering::Relaxed), 1);
}

#[test]
fn guard_reset_panic_destroys_the_entry_and_skips_return() {
    let (provider, state) = provider(ReturnMode::Store);
    state
        .lifecycle
        .panic_on_reset
        .store(true, Ordering::Relaxed);
    let guard = PoolGuard::acquire_with(provider, || TestItem::new(42, &state.lifecycle)).unwrap();

    let payload = catch_unwind(AssertUnwindSafe(|| drop(guard)))
        .err()
        .unwrap();

    assert_eq!(payload.downcast_ref::<&str>(), Some(&"reset panic"));
    assert_eq!(state.lifecycle.resets.load(Ordering::Relaxed), 1);
    assert_eq!(state.lifecycle.drops.load(Ordering::Relaxed), 1);
    assert_eq!(state.returns.load(Ordering::Relaxed), 0);
    assert_eq!(state.drops.load(Ordering::Relaxed), 1);
    assert!(state.entry.lock().unwrap().is_none());
    assert_eq!(
        *state.lifecycle.events.lock().unwrap(),
        ["reset", "item drop", "provider drop"]
    );
}

#[test]
fn guard_return_panic_destroys_the_entry_after_reset() {
    let (provider, state) = provider(ReturnMode::Panic);
    let guard = PoolGuard::acquire_with(provider, || TestItem::new(42, &state.lifecycle)).unwrap();

    let payload = catch_unwind(AssertUnwindSafe(|| drop(guard)))
        .err()
        .unwrap();

    assert_eq!(
        payload.downcast_ref::<&str>(),
        Some(&"provider return panic")
    );
    assert_eq!(state.lifecycle.resets.load(Ordering::Relaxed), 1);
    assert_eq!(state.lifecycle.drops.load(Ordering::Relaxed), 1);
    assert_eq!(state.returns.load(Ordering::Relaxed), 1);
    assert_eq!(state.drops.load(Ordering::Relaxed), 1);
    assert_eq!(
        state
            .lifecycle
            .drops_during_provider_access
            .load(Ordering::Relaxed),
        0
    );
    assert_eq!(
        *state.lifecycle.events.lock().unwrap(),
        ["reset", "return", "item drop", "provider drop"]
    );
}

#[test]
fn guard_returns_the_entry_when_client_code_unwinds() {
    let (provider, state) = provider(ReturnMode::Store);
    let payload = catch_unwind(AssertUnwindSafe(|| {
        let mut guard =
            PoolGuard::acquire_with(provider, || TestItem::new(42, &state.lifecycle)).unwrap();
        guard.value = 99;
        panic!("client panic");
    }))
    .err()
    .unwrap();

    assert_eq!(payload.downcast_ref::<&str>(), Some(&"client panic"));
    assert_eq!(state.lifecycle.resets.load(Ordering::Relaxed), 1);
    assert_eq!(state.returns.load(Ordering::Relaxed), 1);
    assert_eq!(state.returned_value.load(Ordering::Relaxed), 0);
    assert_eq!(state.lifecycle.drops.load(Ordering::Relaxed), 0);
    drop(state.entry.lock().unwrap().take());
    assert_eq!(state.lifecycle.drops.load(Ordering::Relaxed), 1);
}

struct HandleEntry {
    value: Box<TestItem>,
    routing: usize,
}

impl Deref for HandleEntry {
    type Target = TestItem;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl DerefMut for HandleEntry {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.value
    }
}

// SAFETY: the owned Box keeps the value's address fixed as this handle moves.
unsafe impl StableDeref for HandleEntry {}

struct HandleProvider {
    returned_routing: Arc<AtomicUsize>,
}

impl PoolProvider<TestItem> for HandleProvider {
    type Entry = HandleEntry;

    fn take<F, E>(&self, create: F) -> Result<Self::Entry, PoolError<E>>
    where
        F: FnOnce() -> Result<TestItem, E>,
    {
        PoolError::catch(|| {
            Ok(HandleEntry {
                value: Box::new(create()?),
                routing: 42,
            })
        })
    }

    fn return_entry(&self, entry: Self::Entry) -> Result<(), Self::Entry> {
        self.returned_routing
            .store(entry.routing, Ordering::Relaxed);
        assert_eq!(entry.value.value, 0);
        Err(entry)
    }

    fn warm<F, E>(&self, _count: usize, _create: F) -> Result<usize, PoolError<E>>
    where
        F: FnMut() -> Result<TestItem, E>,
    {
        Ok(0)
    }
}

#[test]
fn guard_preserves_provider_owned_entry_context() {
    let lifecycle = Arc::new(Lifecycle::default());
    let returned_routing = Arc::new(AtomicUsize::new(0));
    let provider = HandleProvider {
        returned_routing: Arc::clone(&returned_routing),
    };
    let guard = PoolGuard::acquire_with(provider, || TestItem::new(7, &lifecycle)).unwrap();
    let original_address = address(&guard);
    let moved = std::hint::black_box(guard);

    assert_eq!(address(&moved), original_address);
    assert_eq!(moved.value, 7);
    drop(moved);

    assert_eq!(returned_routing.load(Ordering::Relaxed), 42);
    assert_eq!(lifecycle.resets.load(Ordering::Relaxed), 1);
    assert_eq!(lifecycle.drops.load(Ordering::Relaxed), 1);
}

struct BoxOnlyProvider;

impl<T: PoolItem> PoolProvider<T> for BoxOnlyProvider {
    type Entry = Box<T>;

    fn take<F, E>(&self, create: F) -> Result<Self::Entry, PoolError<E>>
    where
        F: FnOnce() -> Result<T, E>,
    {
        PoolError::catch(|| create().map(Box::new))
    }

    fn return_entry(&self, entry: Self::Entry) -> Result<(), Self::Entry> {
        Err(entry)
    }

    fn warm<F, E>(&self, _count: usize, _create: F) -> Result<usize, PoolError<E>>
    where
        F: FnMut() -> Result<T, E>,
    {
        Ok(0)
    }
}

// Cell makes this value Send but not Sync, and it has no Default impl.
struct NonDefaultItem(std::cell::Cell<usize>);

impl PoolItem for NonDefaultItem {
    fn reset(&mut self) {
        self.0.set(0);
    }
}

#[test]
fn guard_factories_accept_items_without_default_or_sync() {
    let guard =
        PoolGuard::acquire_with(BoxOnlyProvider, || NonDefaultItem(std::cell::Cell::new(42)))
            .unwrap();
    assert_eq!(guard.0.get(), 42);
    drop(guard);

    let guard = PoolGuard::try_acquire_with(BoxOnlyProvider, || {
        Ok::<_, &str>(NonDefaultItem(std::cell::Cell::new(7)))
    })
    .unwrap();
    assert_eq!(guard.0.get(), 7);
    std::thread::spawn(move || {
        assert_eq!(guard.0.get(), 7);
        drop(guard);
    })
    .join()
    .unwrap();
}

struct PanickingDefaultItem;

impl Default for PanickingDefaultItem {
    fn default() -> Self {
        panic!("default factory panic");
    }
}

impl PoolItem for PanickingDefaultItem {
    fn reset(&mut self) {}
}

#[test]
fn guard_acquire_captures_a_panicking_default_factory() {
    let error = PoolGuard::<PanickingDefaultItem, _>::acquire(BoxOnlyProvider)
        .err()
        .unwrap();
    let PoolError::Panic(error) = error;
    assert_eq!(error.to_string(), "default factory panic");
}
