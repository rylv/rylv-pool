# Test coverage map

This suite aims to cover the pool's lifecycle and failure paths without exposing
private fields or inspection helpers in the production API. No tests or checks
have been executed while preparing these additions. The coverage percentage is
unknown until CI or a local coverage run executes them.

## Unit tests

| File | Scenarios |
| --- | --- |
| `src/core/tests.rs` | Packed-slot invariants, capacity zero/full/partial, LIFO order, overflow iterator ownership, partial extensions, const initialization, reference-model operation sequences, exact destruction counts |
| `src/core/tests.rs` | Provider-selected entries, factories and pool hits, reset-before-return, rejection outside provider access, address stability, moves between threads, acquisition errors and panics, reset/return unwind cleanup |
| `src/error/tests.rs` | Successful and failed operations, factory error identity, mutable captures, owned/borrowed/non-string payloads, display/debug/source chains, formatting failure, mutex poisoning, Send/Sync and exact payload destruction |
| `src/thread_local/tests.rs` | Local storage helpers, reset and reuse, weak routing ownership, detachable intrusive batches, partially consumed batches, newest-first remote refill, local priority and overflow ownership |
| `src/thread_local/tests.rs` | Warming limits, zero/full capacity, partial errors/panics, reentrant factories/reset/destructors, TLS initializer failure and unavailable TLS during shutdown, concurrent producers and origin teardown |

## Public integration tests

- `tests/pool_provider.rs` verifies external provider implementations with an
  independently chosen handle and a plain `Box<T>`. Guards preserve addresses and
  reset rejected entries without imposing a concrete allocation wrapper.
- `tests/fixed_thread_local_pool.rs` covers fresh thread-local pools, independent
  storage on different threads, active guards exceeding retained capacity,
  non-Sync values moving between threads, ZSTs, zero capacity, factory bypass,
  warm/error/panic/reentrancy behavior, simultaneous remote producers, undrained
  returns, and origin shutdown, with exact lifecycle accounting.
- `tests/miri_test.rs` runs as ordinary integration tests and as a bounded Miri
  suite. It exercises stable addresses, ownership transfer, zero-capacity
  rejection, concurrent publication/consumption, remote overflow, origin exit,
  races between shutdown and returns, and destructor reentrancy.

Stateful tests use fresh owner threads or test-specific TLS keys. Concurrency
scenarios use barriers and joins; they do not infer completion from timing or
sleep durations. Counters are observed after the relevant synchronization.

## Compile-time contracts

Rustdoc's `compile_fail` examples ensure:

- `PoolItem` cannot contain a non-Send `Rc`.
- A provider entry with `DerefMut` but without `StableDeref` is rejected.
- A borrowed value cannot remain in use after its guard is dropped.
- A guard cannot supply simultaneously used mutable references.
- A guard cannot be cloned to create a second owner.
- The thread-confined storage cannot satisfy `Sync`.

These examples are part of the production API documentation and run in the
CI documentation job and MSRV tests. Positive trait assertions in integration
tests cover `Send`, `StableDeref`, and values that are `Send` but not `Sync`.

## Fuzzing

`fuzz/fuzz_targets/pool_lifecycle.rs` interprets bounded byte sequences as pool
operations. It alternates acquisition, failures and unwind panics, mutations,
local and remote drops, guard moves, partial warm operations, draining, and
reentrant factories. The model checks stable addresses and exclusive identities
for live guards, factory bypass, reset counts, and equality of creations and
destructions after the origin exits.

The fuzz workspace adds no dependencies to the published library. See
`fuzz/README.md` for commands and artifact replay. CI's short run is a smoke test;
longer local runs can explore more inputs. Neither fuzzing nor Miri represents an
exhaustive proof of all atomic queue schedules.

## Deliberate contracts and limits

`N` limits retained local entries. It does not limit active guards or the pending
remote queue. Tests exercise overflow and verify that every value is either owned
by a guard, retained, queued, rejected, or destroyed.

Acquisition and warming capture unwind panics; `reset` and provider return panics
propagate from guard destruction. Unit tests check cleanup for a single unwind.
The suite does not deliberately trigger a second panic during unwind, which would
abort the test process.

Direct `PoolProvider` calls require the caller to follow the provider's ownership
policy. In particular, thread-local entries must be returned through their
original key. The public guard retains the correct key automatically. Rejection
of entries deliberately passed to a different key is not asserted or implemented.
