# Changelog

## [Unreleased]

- Add dependency-free benchmarks for local reuse, allocation, warming, concurrent
  returns, and remote draining, with small values and reusable buffers.

## [0.1.0]

- Introduce `PoolItem`, `PoolProvider`, and `PoolGuard` with provider-selected,
  address-stable entries and automatic reset and return on guard destruction.
- Add `FixedThreadLocalPool` and `FixedThreadLocalPoolGuard` with compile-time
  retention capacity and lock-free returns to the originating thread. Capacity
  limits retained entries, not active guards. Static TLS key references implement
  `PoolProvider` directly.
- Preserve factory failures and captured unwind panic payloads during acquisition
  and warming through `PoolError` and `PanicError`, with standard-library error
  implementations.
- Keep entry routing metadata and storage internals private. Depend only on
  `stable_deref_trait`.
- Add unit, integration, and documentation tests for pool lifecycles, concurrent
  returns, reentrancy, and origin-thread shutdown.
- Add cross-platform CI, MSRV verification, coverage reports, Miri, lifecycle
  fuzzing, separate pool and fuzz dependency policies, API compatibility checks,
  and release automation.
