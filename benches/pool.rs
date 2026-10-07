//! Criterion benchmarks through the pool's public API.

mod local;
mod remote;
mod support;

use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use rylv_pool::FixedThreadLocalPool;
use support::{Buffer, CAPACITY, Small};

thread_local! {
    static SMALL_POOL: FixedThreadLocalPool<Small, CAPACITY> = FixedThreadLocalPool::new();
    static BUFFER_POOL: FixedThreadLocalPool<Buffer, CAPACITY> = FixedThreadLocalPool::new();
    static SMALL_MISS_POOL: FixedThreadLocalPool<Small, 0> = FixedThreadLocalPool::new();
    static BUFFER_MISS_POOL: FixedThreadLocalPool<Buffer, 0> = FixedThreadLocalPool::new();
}

fn benchmarks(criterion: &mut Criterion) {
    local::run::<Small>(criterion, &SMALL_POOL, &SMALL_MISS_POOL, "small");
    local::run::<Buffer>(criterion, &BUFFER_POOL, &BUFFER_MISS_POOL, "buffer");
    remote::run::<Small>(criterion, &SMALL_POOL, "small");
    remote::run::<Buffer>(criterion, &BUFFER_POOL, "buffer");
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(50)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(2));
    targets = benchmarks
}
criterion_main!(benches);
