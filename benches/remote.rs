//! Cross-thread returns with persistent workers and untimed preparation.
//!
//! Return measurements sum each batch's window from the earliest worker start
//! to the latest worker finish, including scheduling skew within that window.
//! The per-entry result describes normalized batch throughput, not individual
//! guard latency. Barrier waits, ownership transfers, allocations, and
//! origin-side draining are outside those timers.

use std::{
    hint::black_box,
    sync::{
        Arc, Barrier,
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use criterion::{BenchmarkId, Criterion, Throughput};

use crate::support::{BenchItem, CAPACITY, Guard, PoolKey, acquire, clear_pool};

enum Command<T: BenchItem> {
    Return(Vec<Guard<T>>),
    Stop,
}

struct ReturnWindow {
    started: Instant,
    finished: Instant,
}

struct Worker<T: BenchItem> {
    commands: Sender<Command<T>>,
    finished: Receiver<ReturnWindow>,
    thread: Option<JoinHandle<()>>,
}

struct Workers<T: BenchItem> {
    workers: Vec<Worker<T>>,
    start: Arc<Barrier>,
}

impl<T: BenchItem> Workers<T> {
    fn new(count: usize) -> Self {
        let start = Arc::new(Barrier::new(count + 1));
        let finish = Arc::new(Barrier::new(count));
        let workers = (0..count)
            .map(|_| {
                let (commands, received) = mpsc::channel::<Command<T>>();
                let (acknowledgement, finished) = mpsc::channel();
                let start = Arc::clone(&start);
                let finish = Arc::clone(&finish);
                let thread = thread::spawn(move || {
                    while let Ok(Command::Return(mut guards)) = received.recv() {
                        // All guards are received before timing begins. The
                        // barrier releases the producers together; waiting for
                        // that release is not part of the return measurement.
                        start.wait();
                        let started = Instant::now();
                        while let Some(guard) = guards.pop() {
                            drop(guard);
                        }
                        let finished = Instant::now();
                        // Wait until every producer has recorded its finish,
                        // so freeing transport buffers and sending acknowledgements
                        // happens after the complete measured return window.
                        finish.wait();
                        drop(guards);
                        if acknowledgement
                            .send(ReturnWindow { started, finished })
                            .is_err()
                        {
                            break;
                        }
                    }
                });
                Worker {
                    commands,
                    finished,
                    thread: Some(thread),
                }
            })
            .collect();
        Self { workers, start }
    }

    fn prepare(&self, key: PoolKey<T>, per_worker: usize) -> Vec<usize> {
        clear_pool(key);
        let mut addresses = Vec::with_capacity(self.workers.len() * per_worker);
        let mut batches = Vec::with_capacity(self.workers.len());
        for _ in &self.workers {
            let mut guards = Vec::with_capacity(per_worker);
            for _ in 0..per_worker {
                let mut guard = acquire(key);
                guard.touch();
                addresses.push(std::ptr::from_ref::<T>(&guard) as usize);
                guards.push(guard);
            }
            batches.push(guards);
        }
        // Finish all fallible acquisition/client work before any worker waits
        // at the barrier, so a setup panic cannot strand a published batch.
        for (worker, guards) in self.workers.iter().zip(batches) {
            worker
                .commands
                .send(Command::Return(guards))
                .unwrap_or_else(|_| panic!("remote benchmark worker stopped before a batch"));
        }
        addresses
    }

    fn return_all(&self) -> Duration {
        self.start.wait();
        let window = self
            .workers
            .iter()
            .map(|worker| {
                worker
                    .finished
                    .recv()
                    .expect("remote benchmark worker failed to return its guards")
            })
            .fold(None, |window: Option<ReturnWindow>, returned| {
                Some(match window {
                    Some(previous) => ReturnWindow {
                        started: previous.started.min(returned.started),
                        finished: previous.finished.max(returned.finished),
                    },
                    None => returned,
                })
            })
            .expect("remote benchmarks require at least one worker");
        window.finished.duration_since(window.started)
    }
}

impl<T: BenchItem> Drop for Workers<T> {
    fn drop(&mut self) {
        for worker in &self.workers {
            let _ = worker.commands.send(Command::Stop);
        }
        for worker in &mut self.workers {
            if let Some(thread) = worker.thread.take() {
                let _ = thread.join();
            }
        }
    }
}

fn require_returned<T: BenchItem>(key: PoolKey<T>) -> Guard<T> {
    Guard::acquire_with(key, || {
        panic!("a completed remote return batch must supply an entry")
    })
    .expect("a completed remote return batch must be reusable")
}

fn validate_return<T: BenchItem>(guard: &mut Guard<T>, addresses: &[usize]) {
    assert!(
        addresses.contains(&(std::ptr::from_ref::<T>(guard) as usize)),
        "a remote return must preserve the origin entry's address"
    );
    guard.touch();
    black_box(&mut **guard);
}

pub(super) fn run<T: BenchItem>(criterion: &mut Criterion, key: PoolKey<T>, item_label: &str) {
    let mut returns = criterion.benchmark_group("remote/return");
    for producer_count in [1, 2, 4] {
        // Threads and synchronization objects persist for every sample of this
        // case. Lazy creation also avoids creating them for filtered/listed
        // cases, and remains outside Criterion's custom measurement callback.
        let mut workers = None;
        returns.throughput(Throughput::Elements(
            u64::try_from(producer_count * CAPACITY).expect("return batch size must fit in u64"),
        ));
        returns.bench_with_input(
            BenchmarkId::new(producer_count.to_string(), item_label),
            &producer_count,
            |bencher, &producer_count| {
                let workers = workers.get_or_insert_with(|| Workers::new(producer_count));
                bencher.iter_custom(|iterations| {
                    let mut elapsed = Duration::ZERO;
                    for _ in 0..iterations {
                        let addresses = workers.prepare(key, CAPACITY);
                        elapsed += workers.return_all();
                        let mut guard = require_returned(key);
                        validate_return(&mut guard, &addresses);
                        drop(guard);
                        clear_pool(key);
                    }
                    // Return the total duration for all requested batches.
                    // Criterion handles per-batch analysis; throughput records
                    // the total number of entries in each batch.
                    elapsed
                });
            },
        );
    }
    returns.finish();

    let mut drains = criterion.benchmark_group("remote/drain");
    let mut workers = None;
    for entry_count in [1, CAPACITY, CAPACITY * 4] {
        drains.throughput(Throughput::Elements(
            u64::try_from(entry_count).expect("drain batch size must fit in u64"),
        ));
        drains.bench_with_input(
            BenchmarkId::new(entry_count.to_string(), item_label),
            &entry_count,
            |bencher, &entry_count| {
                let workers = workers.get_or_insert_with(|| Workers::new(1));
                bencher.iter_custom(|iterations| {
                    let mut elapsed = Duration::ZERO;
                    for _ in 0..iterations {
                        let addresses = workers.prepare(key, entry_count);
                        // Receiving all acknowledgements establishes that every
                        // entry is in the remote queue before the origin acquires.
                        workers.return_all();
                        let started = Instant::now();
                        let result = Guard::acquire_with(key, || {
                            panic!("remote drain must not invoke the factory")
                        });
                        elapsed += started.elapsed();
                        // Time only the first acquisition: it detaches the batch,
                        // retains at most CAPACITY entries, and destroys overflow.
                        // Mutation, assertions, guard return, and cleanup follow
                        // the timestamp.
                        let mut guard = result.expect("remote drain must return an existing entry");
                        validate_return(&mut guard, &addresses);
                        drop(guard);
                        clear_pool(key);
                    }
                    elapsed
                });
            },
        );
    }
    drains.finish();
}
