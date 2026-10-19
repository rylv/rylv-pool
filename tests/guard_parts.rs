use bytes::{Bytes, BytesMut};
use rylv_pool::{
    FixedThreadLocalPool, FixedThreadLocalPoolGuard, PoolError, PoolGuard, PoolItem, PoolProvider,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Default)]
struct Buffer(BytesMut);

impl PoolItem for Buffer {
    fn reset(&mut self) {
        self.0.clear();
    }
}

thread_local! {
    static POOL: FixedThreadLocalPool<Buffer, 1> = FixedThreadLocalPool::new();
}

type Guard = FixedThreadLocalPoolGuard<Buffer, 1>;

#[test]
fn split_freeze_clone_recover_and_return_to_origin() {
    std::thread::spawn(|| {
        let mut guard = Guard::acquire(&POOL).unwrap();
        guard.0.extend_from_slice(b"hello");

        let (Buffer(buffer), route) = guard.into_parts();
        let bytes = buffer.freeze();
        let sent: Bytes = bytes.clone();
        let bytes = bytes.try_into_mut().unwrap_err();
        assert_eq!(&sent[..], b"hello");
        drop(sent);

        let recovered = bytes.try_into_mut().unwrap();
        let allocation = recovered.as_ptr();
        let guard = Guard::from_parts(Buffer(recovered), route);
        assert_eq!(&guard.0[..], b"hello");
        drop(guard);

        let reused = Guard::acquire_with(&POOL, || panic!("entry should be reused")).unwrap();
        assert!(reused.0.is_empty());
        assert_eq!(reused.0.as_ptr(), allocation);
    })
    .join()
    .unwrap();
}

#[test]
fn detached_value_and_route_can_rejoin_on_another_thread() {
    std::thread::spawn(|| {
        let mut guard = Guard::acquire(&POOL).unwrap();
        guard.0.extend_from_slice(b"remote");
        let (value, route) = guard.into_parts();

        std::thread::spawn(move || {
            let guard = Guard::from_parts(value, route);
            assert_eq!(&guard.0[..], b"remote");
            drop(guard);
        })
        .join()
        .unwrap();

        let reused =
            Guard::acquire_with(&POOL, || panic!("remote entry should be reused")).unwrap();
        assert!(reused.0.is_empty());
    })
    .join()
    .unwrap();
}

struct BoxProvider {
    returns: Arc<AtomicUsize>,
    drops: Arc<AtomicUsize>,
}

impl Drop for BoxProvider {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::Relaxed);
    }
}

impl PoolProvider<Buffer> for BoxProvider {
    type Entry = Box<Buffer>;

    fn take<F, E>(&self, create: F) -> Result<Self::Entry, PoolError<E>>
    where
        F: FnOnce() -> Result<Buffer, E>,
    {
        create().map(Box::new).map_err(PoolError::Factory)
    }

    fn return_entry(&self, entry: Self::Entry) -> Result<(), Self::Entry> {
        self.returns.fetch_add(1, Ordering::Relaxed);
        drop(entry);
        Ok(())
    }

    fn warm<F, E>(&self, _: usize, _: F) -> Result<usize, PoolError<E>>
    where
        F: FnMut() -> Result<Buffer, E>,
    {
        Ok(0)
    }
}

#[test]
fn generic_provider_survives_split_and_is_dropped_once() {
    let returns = Arc::new(AtomicUsize::new(0));
    let drops = Arc::new(AtomicUsize::new(0));
    let provider = BoxProvider {
        returns: Arc::clone(&returns),
        drops: Arc::clone(&drops),
    };
    let guard = PoolGuard::acquire_with(provider, Buffer::default).unwrap();
    let (mut value, route) = guard.into_parts();
    value.0.extend_from_slice(b"detached");
    assert_eq!(returns.load(Ordering::Relaxed), 0);
    assert_eq!(drops.load(Ordering::Relaxed), 0);

    let guard = PoolGuard::from_parts(value, route);
    assert_eq!(&guard.0[..], b"detached");
    drop(guard);
    assert_eq!(returns.load(Ordering::Relaxed), 1);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
}
