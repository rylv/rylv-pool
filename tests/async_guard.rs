//! Compile-time regression for a generic Send future holding a TLS guard.

use std::{future::Future, thread::LocalKey};

use rylv_pool::{FixedThreadLocalPool, FixedThreadLocalPoolGuard, PoolItem};

#[derive(Default)]
struct Item;

impl PoolItem for Item {
    fn reset(&mut self) {}
}

thread_local! {
    static POOL: FixedThreadLocalPool<Item, 1> = FixedThreadLocalPool::new();
}

async fn hold_across_await<T: PoolItem + Default, const N: usize>(
    key: &'static LocalKey<FixedThreadLocalPool<T, N>>,
) {
    let guard = FixedThreadLocalPoolGuard::<T, N>::acquire(key).unwrap();
    std::future::ready(()).await;
    drop(guard);
}

fn require_send(_: impl Future<Output = ()> + Send) {}

#[test]
fn generic_future_with_guard_is_send() {
    require_send(hold_across_await(&POOL));
}
