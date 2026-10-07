# Pool benchmarks

The `pool` benchmark target uses Criterion and the crate's public API.
Criterion manages warmup, sample calibration, statistical analysis, and saved
baselines. Scenario code defines the operations and timing boundaries and uses
`std::hint::black_box` to keep them observable to the optimizer.
Criterion 0.7 is a development dependency compatible with Rust 1.85; it is not
a runtime dependency of applications using the pool. `harness = false` disables
Cargo's built-in harness so Criterion can supply the benchmark entry point.

## Running

```sh
make bench
make bench BENCH_ARGS='warm/ --sample-size 50 --measurement-time 3'
make bench BENCH_ARGS='--list'
make bench BENCH_ARGS='--help'
```

The equivalent direct command is:

```sh
cargo bench --locked --bench pool -- warm/ --sample-size 50 --measurement-time 3
```

Cargo builds the optimized bench profile. Defaults are 50 samples, one second
of warmup, and a two-second measurement budget per case. Criterion chooses the
iteration count. Override these with `--sample-size`, `--warm-up-time`, and
`--measurement-time`. A positional regular expression selects benchmark IDs;
`warm/` selects warming cases and `/buffer` selects buffer cases. `--list` lists
matching cases, and `--help` documents the supported Criterion arguments.

Criterion reports time per batch with statistical estimates and confidence
intervals. `Throughput::Elements` records the operations or entries in a batch
and reports their throughput. Local operations and `warm/full` use 256
operations or calls per iteration. Refill and drain IDs specify the number of
entries in a batch; remote return IDs specify producers, each returning 256
entries. Batch time divided by its entry count is a normalized cost, not an
individual guard's latency distribution.

`cargo test --all-targets` uses Criterion's test mode to exercise every scenario
once without collecting performance samples. This preserves smoke checks on
every CI platform and profile.

## Cases

Every case below has `/small` and `/buffer` suffixes, for 26 scenarios in total.
The small value contains 64 bytes and has an empty reset operation. The buffer
value reserves 1 KiB, writes 64 bytes when used, and retains its allocation on
reset by clearing its length. The allocation reference also performs the same
touch and reset operations before dropping its plain `Box`.

| Case prefix | Measured operation | Elements per timed batch |
| --- | --- | --- |
| `local/reuse` | Acquire and drop guards from a prewarmed pool | 256 operations |
| `local/miss` | Acquire through a factory and drop with no retained capacity | 256 operations |
| `alloc/box` | Allocate and drop plain `Box` values as a reference cost | 256 operations |
| `warm/full` | Call `warm` when the pool is already full | 256 calls |
| `warm/refill/1` | Create and retain one entry in an empty pool | 1 entry |
| `warm/refill/32` | Create and retain 32 entries in an empty pool | 32 entries |
| `warm/refill/256` | Create and retain 256 entries in an empty pool | 256 entries |
| `remote/return/1` | Return guards from one producer thread | 256 entries |
| `remote/return/2` | Return guards concurrently from two producer threads | 512 entries |
| `remote/return/4` | Return guards concurrently from four producer threads | 1024 entries |
| `remote/drain/1` | Refill local storage from one pending remote return | 1 entry |
| `remote/drain/256` | Refill from a pending batch that fits the 256-entry pool | 256 entries |
| `remote/drain/1024` | Refill from an oversized batch and destroy overflow | 1024 entries |

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
These scenarios and remote return use Criterion's `iter_custom`, returning the
sum of the measured durations for all requested iterations. Criterion handles
analysis while setup and cleanup remain outside that returned duration. Since
calibration observes the entire callback, expensive setup can reduce the
iteration count; persistent workers avoid repeated thread creation.

## Comparing results

Use the same toolchain, profile, case filter, configuration, and machine for
before/after comparisons. Run on an otherwise idle machine, repeat the command,
and inspect the spread across samples rather than treating one timing as a
stable baseline. Thread scheduling, CPU frequency, and allocator behavior can
change the result, particularly for concurrent cases.

Save and compare baselines locally with:

```sh
cargo install critcmp --version 0.1.8 --locked
make bench BENCH_ARGS='--save-baseline base'
# After changing the implementation:
make bench BENCH_ARGS='--save-baseline candidate'
critcmp base candidate
critcmp base candidate --threshold 15
```

Criterion stores measurements under `target/criterion`.
[critcmp](https://github.com/BurntSushi/critcmp) reads the saved measurements and
compares mean batch times. Its `--threshold` option shows only differences
greater than the specified percentage, including improvements. If nothing
exceeds that threshold, it prints `no benchmark comparisons to show` and exits
with an error. Displayed ratios are relative to the fastest baseline in each
case; a lower mean time is better.

Pull-request CI benchmarks the exact base and head commits on the same runner
with the same toolchain and configuration, using critcmp 0.1.8. A small gate
reads critcmp's lists and fails when the PR mean batch time increases by more
than 15%. critcmp applies the threshold to the unrounded measurements; the gate
uses baseline order to distinguish regressions from improvements. Confidence
interval overlap does not suppress a regression. Added and removed scenarios
are reported separately; comparisons with no common scenarios fail.
CI saves the measurements, critcmp output, and job summary as artifacts.
A base commit with the previous custom harness is explicitly skipped until the
Criterion migration is merged. A new baseline is measured for each PR, avoiding
a historical baseline from another machine.
Set the repository variable `BENCHMARK_THRESHOLD_PERCENT` to override the default
15% threshold with a finite, nonnegative value. For local reports use critcmp's
`--threshold` argument; CI's gate supplies the failure status for regressions.

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
