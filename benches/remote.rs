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

use crate::{
    Measurement, Runner,
    support::{BenchItem, CAPACITY, Guard, PoolKey, acquire, clear_pool},
};

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
        let workers = (0..count)
            .map(|_| {
                let (commands, received) = mpsc::channel::<Command<T>>();
                let (acknowledgement, finished) = mpsc::channel();
                let start = Arc::clone(&start);
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
                        // Preserve the allocated Vec until after the timestamp
                        // so freeing the transport buffer is not measured.
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

pub(super) fn run<T: BenchItem>(runner: &mut Runner, key: PoolKey<T>, item_label: &str) {
    for producer_count in [1, 2, 4] {
        // Threads and synchronization objects persist for every sample of this
        // case and are created outside both warmup and measured batches.
        let mut workers = None;
        runner.measure(
            &format!("remote/return/{producer_count}/{item_label}"),
            producer_count * CAPACITY,
            "entry",
            |iterations| {
                let workers = workers.get_or_insert_with(|| Workers::new(producer_count));
                let mut elapsed = Duration::ZERO;
                for _ in 0..iterations {
                    let addresses = workers.prepare(key, CAPACITY);
                    elapsed += workers.return_all();
                    let mut guard = require_returned(key);
                    validate_return(&mut guard, &addresses);
                    drop(guard);
                    clear_pool(key);
                }
                Measurement {
                    elapsed,
                    batches: iterations,
                }
            },
        );
    }

    let mut workers = None;
    for entry_count in [1, CAPACITY, CAPACITY * 4] {
        runner.measure(
            &format!("remote/drain/{entry_count}/{item_label}"),
            entry_count,
            "entry",
            |iterations| {
                let workers = workers.get_or_insert_with(|| Workers::new(1));
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
                Measurement {
                    elapsed,
                    batches: iterations,
                }
            },
        );
    }
}
