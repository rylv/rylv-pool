//! Dependency-free benchmarks through the pool's public API.

mod local;
mod remote;
mod support;

use std::{env, process::ExitCode, time::Duration};

use rylv_pool::FixedThreadLocalPool;
use support::{Buffer, CAPACITY, Small};

thread_local! {
    static SMALL_POOL: FixedThreadLocalPool<Small, CAPACITY> = FixedThreadLocalPool::new();
    static BUFFER_POOL: FixedThreadLocalPool<Buffer, CAPACITY> = FixedThreadLocalPool::new();
    static SMALL_MISS_POOL: FixedThreadLocalPool<Small, 0> = FixedThreadLocalPool::new();
    static BUFFER_MISS_POOL: FixedThreadLocalPool<Buffer, 0> = FixedThreadLocalPool::new();
}

struct Config {
    benchmark: bool,
    list: bool,
    samples: usize,
    iterations: usize,
    filter: Option<String>,
}

impl Config {
    fn parse() -> Result<Option<Self>, String> {
        let mut config = Self {
            benchmark: false,
            list: false,
            samples: 9,
            iterations: 200,
            filter: None,
        };
        let mut args = env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--bench" => config.benchmark = true,
                "--test" => config.benchmark = false,
                "--list" => config.list = true,
                "--help" | "-h" => {
                    println!(
                        "Usage: cargo bench --locked --bench pool -- [OPTIONS] [FILTER]\n\
                         Options:\n\
                         --samples N     Samples per case (default: 9)\n\
                         --iterations N  Batches per sample (default: 200)\n\
                         --list          List cases without running them\n\
                         --help          Show this help\n\
                         FILTER          Case-name substring, e.g. warm/ or /buffer\n\
                         Cargo supplies --bench for timed runs; tests run scenario smoke checks."
                    );
                    return Ok(None);
                }
                "--samples" | "--iterations" => {
                    let value = args
                        .next()
                        .ok_or_else(|| format!("{arg} requires a positive integer"))?;
                    let value = value
                        .parse::<usize>()
                        .ok()
                        .filter(|value| *value > 0)
                        .ok_or_else(|| format!("{arg} requires a positive integer"))?;
                    if arg == "--samples" {
                        config.samples = value;
                    } else {
                        config.iterations = value;
                    }
                }
                value if value.starts_with('-') => {
                    return Err(format!("unknown option: {value}"));
                }
                _ if config.filter.is_none() => config.filter = Some(arg),
                _ => return Err("provide at most one case-name filter".to_owned()),
            }
        }
        Ok(Some(config))
    }
}

struct Measurement {
    elapsed: Duration,
    batches: usize,
}

struct Runner {
    config: Config,
    cases: usize,
}

impl Runner {
    fn measure(
        &mut self,
        name: &str,
        units_per_batch: usize,
        unit: &str,
        mut work: impl FnMut(usize) -> Measurement,
    ) {
        if self
            .config
            .filter
            .as_ref()
            .is_some_and(|filter| !name.contains(filter))
        {
            return;
        }
        self.cases += 1;
        if self.config.list {
            println!("{name}");
            return;
        }
        if !self.config.benchmark {
            work(1);
            println!("scenario {name} ... ok");
            return;
        }

        // Exercise initialization and scenario checks before collecting samples.
        work(1);
        let mut samples = Vec::with_capacity(self.config.samples);
        for _ in 0..self.config.samples {
            let measurement = work(self.config.iterations);
            assert!(measurement.batches > 0 && units_per_batch > 0);
            samples.push(measurement.elapsed.as_secs_f64() * 1e9 / measurement.batches as f64);
        }
        samples.sort_unstable_by(f64::total_cmp);
        let median_batch = percentile(&samples, 5, 10);
        let units = units_per_batch as f64;
        println!(
            "{name:<36} median {:>10.2} ns/{unit:<5} p10 {:>10.2}  p90 {:>10.2}  batch {:>12.2} ns",
            median_batch / units,
            percentile(&samples, 1, 10) / units,
            percentile(&samples, 9, 10) / units,
            median_batch,
        );
    }
}

fn percentile(sorted: &[f64], numerator: usize, denominator: usize) -> f64 {
    let position = (sorted.len() - 1) * numerator;
    let index = position / denominator;
    let fraction = (position % denominator) as f64 / denominator as f64;
    let lower = sorted[index];
    let upper = sorted[(index + 1).min(sorted.len() - 1)];
    lower + (upper - lower) * fraction
}

fn main() -> ExitCode {
    let config = match Config::parse() {
        Ok(Some(config)) => config,
        Ok(None) => return ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}; use --help for usage");
            return ExitCode::from(2);
        }
    };
    if config.benchmark && !config.list {
        println!(
            "{} samples, {} batches/sample; medians and sample percentiles\n",
            config.samples, config.iterations
        );
        if cfg!(debug_assertions) {
            eprintln!("Timing an unoptimized build; use cargo bench for performance comparisons.");
        }
    }
    let mut runner = Runner { config, cases: 0 };
    local::run::<Small>(&mut runner, &SMALL_POOL, &SMALL_MISS_POOL, "small");
    local::run::<Buffer>(&mut runner, &BUFFER_POOL, &BUFFER_MISS_POOL, "buffer");
    remote::run::<Small>(&mut runner, &SMALL_POOL, "small");
    remote::run::<Buffer>(&mut runner, &BUFFER_POOL, "buffer");
    if runner.cases == 0 {
        eprintln!("no benchmark cases match the filter");
        return ExitCode::from(2);
    }
    ExitCode::SUCCESS
}
