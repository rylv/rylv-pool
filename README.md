# rylv-pool

Generic pooling with pluggable providers, extracted from `rylv_utils`.

- `PoolItem` defines how reusable values are reset.
- `PoolProvider` controls acquisition, returns, and prewarming.
- `PoolProvider::Entry` lets each provider choose its own ownership handle and
  routing context, with mutable access and `StableDeref` guarantees.
- `PoolGuard` returns values automatically on drop and implements `StableDeref`.
- `FixedThreadLocalPool<T, N>` provides fixed-capacity thread-local storage and
  lock-free returns from other threads. `N` limits retained entries, not active guards.
  Its static TLS key reference implements `PoolProvider` directly.
- `PoolError` preserves factory errors and captured unwind panic payloads.

Public types are imported from the crate root. `FixedThreadLocalEntry` is an
opaque associated type of the thread-local provider; its routing details remain
private. Storage internals and test inspection helpers are not part of the API.

The crate depends only on `stable_deref_trait`.

```rust
use rylv_pool::{FixedThreadLocalPool, FixedThreadLocalPoolGuard, PoolItem};

#[derive(Default)]
struct Scratch(Vec<u8>);

impl PoolItem for Scratch {
    fn reset(&mut self) {
        self.0.clear();
    }
}

thread_local! {
    static POOL: FixedThreadLocalPool<Scratch, 8> = FixedThreadLocalPool::new();
}

let mut scratch = FixedThreadLocalPoolGuard::acquire(&POOL)?;
scratch.0.extend_from_slice(b"reusable storage");
# Ok::<(), rylv_pool::PoolError<std::convert::Infallible>>(())
```

## Testing and CI

The CI configuration is based on the
[rylv-metrics workflows](https://github.com/rylv/rylv-metrics/tree/master/.github/workflows).
It includes stable and MSRV builds on Linux/macOS/Windows, debug/release tests,
Clippy, formatting, documentation, coverage artifacts, Miri, fuzzing, dependency
security and license checks, semver checks, and tag-driven releases.

The coverage gate targets at least 95% of production lines. This target has not
been measured: the suite and workflows were written without executing tests or
checks. See [CI setup](docs/ci.md) for commands, artifacts, and required GitHub
configuration, and [test coverage map](docs/testing.md) for the scenarios.

`make verify` runs the normal local checks. `make coverage`, `make miri`, and
`make fuzz` opt into the additional tools. `cargo test --locked` runs the root
crate's unit, integration, and documentation tests.

## License

Licensed under either [Apache License 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.
