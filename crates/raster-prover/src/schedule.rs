//! Running independent proofs side by side (`transition-split`).
//!
//! A fraud window's leaf proofs — the tile replays, the authorization, and
//! every trace, control-flow, storage and transition segment — read only the
//! recorded trace, witnesses and executed journals: none needs another's
//! receipt. FRAUD WINDOW needs their receipts as assumptions, so it runs after
//! them. This runs the leaves on a pool of worker threads; with one worker it
//! runs them in order on the calling thread.
//!
//! The receipts are the same either way: scheduling changes when a proof runs,
//! not what it proves. More workers pay when there is somewhere for them to
//! run — CPU cores, a GPU with memory for several proofs, or a remote prover.
//! risc0's in-process prover puts every proof on the same default GPU, so on a
//! GPU that fits one proof at a time (an 8 GB card at 2^18-cycle segments),
//! keep one worker.

use std::num::NonZeroUsize;
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// How many proofs run at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProverWorkers(NonZeroUsize);

impl ProverWorkers {
    pub fn new(workers: NonZeroUsize) -> Self {
        Self(workers)
    }

    pub fn get(self) -> usize {
        self.0.get()
    }
}

impl Default for ProverWorkers {
    /// One: proofs run in order on the calling thread.
    fn default() -> Self {
        Self(NonZeroUsize::MIN)
    }
}

impl FromStr for ProverWorkers {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse::<usize>()
            .ok()
            .and_then(NonZeroUsize::new)
            .map(Self)
            .ok_or_else(|| format!("expected a positive number of provers, got `{s}`"))
    }
}

/// One proof to run, producing `T`.
pub type Job<'a, T> = Box<dyn FnOnce() -> T + Send + 'a>;

/// Run `jobs` on up to `workers` threads and return their results in the
/// jobs' order. With one worker, or one job, they run in order on the calling
/// thread. A job that panics panics the call, as it would run inline.
pub fn run_jobs<'a, T: Send>(workers: ProverWorkers, jobs: Vec<Job<'a, T>>) -> Vec<T> {
    let threads = workers.get().min(jobs.len());
    if threads <= 1 {
        return jobs.into_iter().map(|job| job()).collect();
    }
    let count = jobs.len();
    let queue: Vec<Mutex<Option<Job<'a, T>>>> =
        jobs.into_iter().map(|job| Mutex::new(Some(job))).collect();
    let results: Vec<Mutex<Option<T>>> = (0..count).map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                if index >= count {
                    break;
                }
                let job = queue[index]
                    .lock()
                    .expect("a job slot is taken once")
                    .take()
                    .expect("each job runs once");
                let result = job();
                *results[index]
                    .lock()
                    .expect("a result slot is written once") = Some(result);
            });
        }
    });
    results
        .into_iter()
        .map(|slot| {
            slot.into_inner()
                .expect("a result slot is written once")
                .expect("every job ran")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    fn workers(n: usize) -> ProverWorkers {
        ProverWorkers::new(NonZeroUsize::new(n).unwrap())
    }

    #[test]
    fn results_come_back_in_job_order_whatever_the_workers() {
        for n in [1, 2, 3, 8] {
            let jobs: Vec<Job<'_, usize>> = (0..10usize)
                .map(|i| {
                    Box::new(move || {
                        // Later jobs finish first, so order isn't by luck.
                        std::thread::sleep(Duration::from_millis((10 - i) as u64));
                        i * i
                    }) as Job<'_, usize>
                })
                .collect();
            assert_eq!(
                run_jobs(workers(n), jobs),
                (0..10).map(|i| i * i).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn jobs_may_borrow_from_the_caller() {
        let inputs = vec![1u32, 2, 3];
        let jobs: Vec<Job<'_, u32>> = inputs
            .iter()
            .map(|x| Box::new(move || x + 1) as Job<'_, u32>)
            .collect();
        assert_eq!(run_jobs(workers(2), jobs), vec![2, 3, 4]);
    }

    #[test]
    fn several_workers_run_at_once() {
        let running = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let jobs: Vec<Job<'_, ()>> = (0..4)
            .map(|_| {
                Box::new(|| {
                    let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(50));
                    running.fetch_sub(1, Ordering::SeqCst);
                }) as Job<'_, ()>
            })
            .collect();
        run_jobs(workers(4), jobs);
        assert!(peak.load(Ordering::SeqCst) > 1);
    }

    #[test]
    fn the_worker_count_parses() {
        assert_eq!("3".parse::<ProverWorkers>().unwrap().get(), 3);
        assert_eq!(ProverWorkers::default().get(), 1);
        assert!("0".parse::<ProverWorkers>().is_err());
        assert!("many".parse::<ProverWorkers>().is_err());
    }
}
