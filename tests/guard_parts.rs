use bytes::{Bytes, BytesMut};
use rylv_pool::{FixedThreadLocalPool, FixedThreadLocalPoolGuard, PoolItem};

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
