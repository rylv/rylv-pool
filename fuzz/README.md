# Pool lifecycle fuzzing

`pool_lifecycle` interprets input bytes as lifecycle operations against a
thread-local pool with four retained slots. The provider keeps its normal
lock-free remote-return implementation. The fuzz package is an isolated Cargo
workspace, so `libfuzzer-sys` does not become a library or root development
dependency.

The state machine covers:

- Acquisition with custom, fallible, panicking, and reentrant factories.
- Reuse that bypasses a factory, resetting mutable values, and moving guards.
- Successful and partially failing warming, capacity limits, and zero-target
  warming that drains pending remote returns.
- Local returns, individual and batched foreign-thread returns, remote overflow,
  and active guards that outlive the origin thread.
- Exclusive ownership, unique live values, stable addresses, and exact counts
  of resets, creation, and destruction after every origin thread shuts down.

Each input is limited to 64 operations, eight active guards, and four remote
batches. This bounds allocations and thread creation while retaining sequences
that exceed the fixed storage capacity. Expected factory panics use
`resume_unwind`, so they exercise panic capture without flooding the panic hook.
The target does not intentionally reset or return entries through a different
provider.

Install the runner and run from the repository root:

```sh
cargo install cargo-fuzz --locked
cargo +nightly fuzz run pool_lifecycle -- -max_total_time=60 -max_len=64
```

Replay a saved failure:

```sh
cargo +nightly fuzz run pool_lifecycle fuzz/artifacts/pool_lifecycle/crash-<hash>
```

Minimize a saved input:

```sh
cargo +nightly fuzz tmin pool_lifecycle fuzz/artifacts/pool_lifecycle/crash-<hash>
```

Run the deterministic ownership and remote-return scenarios under Miri:

```sh
rustup +nightly component add miri
cargo +nightly miri setup
cargo +nightly miri test --test miri_test
```

The same bounded scenarios also participate in ordinary `cargo test` and
coverage collection. Miri explores a concrete execution; varying its seed can
exercise other schedules, but neither Miri nor fuzzing proves correctness for
every possible thread interleaving. A crashing input should be retained and
converted into a deterministic regression test before changing the target.

These commands are documentation only. The initial addition of this harness
has not been compiled, tested, or fuzzed.
