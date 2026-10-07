# Pool benchmarks

The `pool` benchmark target uses only `std` and the crate's public API. Its custom
harness measures elapsed time with `Instant` and uses `black_box` to keep values
and operations observable to the optimizer. It adds no dependencies to the
library or benchmark target.

## Running

```sh
make bench
make bench BENCH_ARGS='--samples 9 --iterations 2000 warm/'
make bench BENCH_ARGS='--list'
make bench BENCH_ARGS='--help'
```

The equivalent direct command is:

```sh
cargo bench --locked --bench pool -- --samples 9 --iterations 2000 warm/
```

Cargo builds the optimized bench profile. The defaults are nine samples and
200 iterations per sample. `--samples` controls the number of measurements and
`--iterations` controls the number of batches per sample. Local operations and
`warm/full` use 256 operations or calls per batch. Refill and drain case names
indicate the number of entries in a batch; remote return names indicate the
number of producer threads, each returning 256 entries per batch. An optional
positional substring selects matching case names; for example, `warm/` selects
all warming cases and `/buffer` selects all buffer cases. `--list` lists matching
cases without executing them, and `--help` prints the defaults and usage.

Each result reports the median, p10, and p90 across samples, using linear
interpolation between sorted observations. These are descriptive sample
percentiles, not confidence intervals. Batch scenarios also report the
elapsed nanoseconds per batch so that the total cost is visible alongside the
normalized cost per entry.

Without Cargo's `--bench` argument, the executable runs small smoke workloads
and checks scenario invariants without reporting timings. This makes
`cargo test --all-targets` exercise the benchmark code on every CI platform and
profile. There is no performance threshold in CI.

## Cases

Every case below has `/small` and `/buffer` suffixes, for 26 scenarios in total.
The small value contains 64 bytes and has an empty reset operation. The buffer
value reserves 1 KiB, writes 64 bytes when used, and retains its allocation on
reset by clearing its length. The allocation reference also performs the same
touch and reset operations before dropping its plain `Box`.

| Case prefix | Measured operation | Unit |
| --- | --- | --- |
| `local/reuse` | Acquire and drop a guard from a prewarmed pool | ns/op |
| `local/miss` | Acquire through a factory and drop with no retained capacity | ns/op |
| `alloc/box` | Allocate and drop a plain `Box` as a reference cost | ns/op |
| `warm/full` | Call `warm` when the pool is already full | ns/call |
| `warm/refill/1` | Create and retain one entry in an empty pool | ns/entry and ns/batch |
| `warm/refill/32` | Create and retain 32 entries in an empty pool | ns/entry and ns/batch |
| `warm/refill/256` | Create and retain 256 entries in an empty pool | ns/entry and ns/batch |
| `remote/return/1` | Return guards from one producer thread | ns/entry and ns/batch |
| `remote/return/2` | Return guards concurrently from two producer threads | ns/entry and ns/batch |
| `remote/return/4` | Return guards concurrently from four producer threads | ns/entry and ns/batch |
| `remote/drain/1` | Refill local storage from one pending remote return | ns/entry and ns/batch |
| `remote/drain/256` | Refill from a pending batch that fits the 256-entry pool | ns/entry and ns/batch |
| `remote/drain/1024` | Refill from an oversized batch and destroy overflow | ns/entry and ns/batch |

## Timing boundaries

Preparation stays outside the measured section: pool prewarming, moving guards
to workers, thread creation, and control-channel coordination. Cleanup between
samples also stays outside, except when destruction is explicitly part of the
operation being measured.

`warm/refill` starts from an empty pool for every batch. Removing the retained
entries between batches is not timed, so subsequent samples keep measuring
refill rather than the already-full path. The factory and entry allocation are
part of the measured `warm` call.

Remote return workers are created once and reused. They synchronize before each
batch and record their own start and finish timestamps. The batch window runs
from the earliest worker start to the latest worker finish, including scheduling
skew inside that window. Its duration is normalized across all returned entries
to describe batch throughput under contention. This is neither the individual
latency of a guard drop nor the end-to-end time for dispatching a batch and
waiting for acknowledgements. Thread startup, distributing guards, and the
initial barrier wait stay outside the measurement.

Remote drain cases prepare and publish their pending entries before timing the
origin's first acquisition. That acquisition processes the batch, retains entries
up to capacity, and destroys overflow. Returning the acquired guard and cleaning
up retained entries happen after timing. Publishing the batch is measured
separately by the remote return cases.

Refill and drain cases timestamp each batch individually. Timer overhead is not
subtracted and can be significant for one-entry batches; compare the same cases
with the same timing method before and after a change.

## Comparing results

Use the same toolchain, profile, case filter, iteration count, and machine for
before/after comparisons. Run on an otherwise idle machine, repeat the command,
and inspect the spread across samples rather than treating one timing as a
stable baseline. Thread scheduling, CPU frequency, and allocator behavior can
change the result, particularly for concurrent cases.

`local/miss` and `alloc/box` provide context for reuse; they perform different
work. A pool miss includes the provider's entry wrapper, routing metadata,
capturing factory failures, reset, and rejection. Plain `Box` allocation does
not model that metadata or the guard lifecycle.

Remote return measurements cover the entire return route, including reset,
origin checks, upgrading the weak queue reference, and publishing the entry.
They do not isolate `Weak::upgrade` or an individual atomic instruction. Buffer
results also include the object's work when the measured path creates, uses, or
destroys its allocation.

Use `warm/refill` to compare changes to metadata construction and repeated TLS
accesses; use `local/reuse` to watch the common guard path. The suite reports
observations and leaves the production API unchanged.
