//! Worker threads running parts of the chain on the other CPU cores, so a
//! rig is no longer bound to the one core of the audio callback: the two
//! branches of a split, the old and the new chain of a crossfade.
//!
//! `join(a, b)` runs `a` on the calling thread and `b` on an idle worker,
//! and returns once both are done; without an idle worker (all busy, none
//! started, or the pool turned off), it runs both on the calling thread.
//! It never allocates nor takes a lock: a worker waits for its job by
//! spinning a little, then parked (a futex on Linux), and the caller waits
//! for the result by spinning, since the job runs on another core and takes
//! a fraction of the period. Workers take the scheduling class and priority
//! of the thread handing them work (the JACK thread, `SCHED_FIFO`), so they
//! are as real-time as it is.

use std::cell::UnsafeCell;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread::{self, Thread};
use std::time::{Duration, Instant};

use tracing::{info, warn};

/// Workers at most: one per core besides the audio thread's, up to eight
/// cores (a chain cut finer gains little: every stage waits for the first
/// slice of the one before)
pub const MAX_WORKERS: usize = 7;
/// How long a worker that just finished keeps spinning for the next job
/// before parking: the next split of the same period comes within it
const SPIN_AFTER_JOB: Duration = Duration::from_micros(60);

const IDLE: u8 = 0;
/// Taken by a caller, which is writing the job
const CLAIMED: u8 = 1;
const READY: u8 = 2;
const DONE: u8 = 3;

/// A job: a closure on the caller's stack, run through `run`
#[derive(Clone, Copy)]
struct Job {
    data: *mut (),
    run: unsafe fn(*mut ()),
}

struct Worker {
    state: AtomicU8,
    job: UnsafeCell<Option<Job>>,
    thread: OnceLock<Thread>,
    /// Scheduling of the caller: policy and priority (+1, 0 when unknown)
    sched: AtomicU64,
    /// Jobs run, for the tests and the monitor
    jobs: AtomicUsize,
}

impl Worker {
    /// Give `job` to the worker and wake it, with the scheduling `sched` to
    /// take (`caller_sched`)
    ///
    /// # Safety
    /// The caller owns the worker (moved it from IDLE to CLAIMED), and what
    /// `job` points to lives until the worker is DONE
    #[inline]
    unsafe fn hand(&self, job: Job, sched: u64) {
        // SAFETY: the caller owns the worker (CLAIMED) until READY
        unsafe { *self.job.get() = Some(job) };
        self.sched.store(sched, Ordering::Relaxed);
        self.state.store(READY, Ordering::Release);
        if let Some(thread) = self.thread.get() {
            thread.unpark();
        }
    }
}

// SAFETY: `job` is written only by the caller that moved `state` from IDLE
// to CLAIMED, and read only by the worker after it saw READY (and the
// caller does not touch it again before DONE)
unsafe impl Sync for Worker {}
// SAFETY: the job's pointer is only followed while its caller waits
unsafe impl Send for Worker {}

struct Pool {
    workers: Box<[Worker]>,
    /// Workers to use (0: the pool is off), up to `workers.len()`
    active: AtomicUsize,
}

static POOL: OnceLock<Pool> = OnceLock::new();

/// Workers to start by default: one per core but the audio thread's, up to
/// `MAX_WORKERS`
pub fn default_workers() -> usize {
    thread::available_parallelism()
        .map_or(1, |n| n.get())
        .saturating_sub(1)
        .min(MAX_WORKERS)
}

/// Use `workers` worker threads, started when first needed (none on a
/// single core). `workers` = 0 keeps everything on the audio thread
pub fn start(workers: usize) {
    let workers = workers.min(MAX_WORKERS);
    let pool = POOL.get_or_init(|| Pool {
        workers: (0..MAX_WORKERS)
            .map(|_| Worker {
                state: AtomicU8::new(IDLE),
                job: UnsafeCell::new(None),
                thread: OnceLock::new(),
                sched: AtomicU64::new(0),
                jobs: AtomicUsize::new(0),
            })
            .collect(),
        active: AtomicUsize::new(0),
    });
    // One caller at a time spawns, so no worker gets two threads
    static SPAWNING: Mutex<()> = Mutex::new(());
    let _spawning = SPAWNING.lock().unwrap_or_else(|e| e.into_inner());
    for (k, worker) in pool.workers.iter().enumerate().take(workers) {
        if worker.thread.get().is_some() || worker.state.load(Ordering::Acquire) != IDLE {
            continue;
        }
        let spawned = thread::Builder::new()
            .name(format!("dsp-{}", k + 1))
            .spawn(move || run_worker(worker));
        match spawned {
            Ok(handle) => {
                let _ = worker.thread.set(handle.thread().clone());
            }
            Err(e) => {
                warn!(worker = k + 1, error = %e, "DSP worker not started");
                // Never handed work: claimed for good
                worker.state.store(CLAIMED, Ordering::Release);
            }
        }
    }
    pool.active.store(workers, Ordering::Release);
    info!(cores = workers + 1, "DSP cores set");
}

/// Workers in use (0: the audio thread alone)
pub fn workers() -> usize {
    POOL.get()
        .map_or(0, |pool| pool.active.load(Ordering::Acquire))
}

/// Jobs the workers have run so far
pub fn jobs_run() -> usize {
    POOL.get().map_or(0, |pool| {
        pool.workers
            .iter()
            .map(|w| w.jobs.load(Ordering::Relaxed))
            .sum()
    })
}

/// Run `a` here and `b` on an idle worker, at the same time; returns once
/// both are done. Without an idle worker, runs `a` then `b` here. Returns
/// true if `b` ran on a worker.
pub fn join<A: FnOnce(), B: FnOnce() + Send>(a: A, b: B) -> bool {
    let Some(worker) = claim() else {
        a();
        b();
        return false;
    };
    let mut b = Some(b);
    unsafe fn call<B: FnOnce()>(data: *mut ()) {
        // SAFETY: `data` is the `Option<B>` on the stack of `join`, which
        // waits for DONE before returning or unwinding
        let b = unsafe { (*(data as *mut Option<B>)).take() };
        if let Some(b) = b {
            b();
        }
    }
    let job = Job {
        data: &mut b as *mut Option<B> as *mut (),
        run: call::<B>,
    };
    // SAFETY: claimed above; `_wait` keeps `b` alive until the worker is done
    unsafe { worker.hand(job, caller_sched()) };
    // Waits for the worker even if `a` panics: `b` borrows this stack
    let _wait = WaitDone(worker);
    a();
    true
}

/// Idle workers taken together for one parallel run (`run`); the ones not
/// used are given back when it is dropped
pub struct Crew {
    workers: [Option<&'static Worker>; MAX_WORKERS],
    len: usize,
}

/// Take up to `max` idle workers
pub fn crew(max: usize) -> Crew {
    let mut crew = Crew {
        workers: [None; MAX_WORKERS],
        len: 0,
    };
    while crew.len < max.min(MAX_WORKERS) {
        let Some(worker) = claim() else { break };
        crew.workers[crew.len] = Some(worker);
        crew.len += 1;
    }
    crew
}

impl Crew {
    /// Workers taken
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Run `f(0)` here and `f(1)` … `f(len)` on the workers, at the same
    /// time; returns once all are done
    pub fn run(mut self, f: &(dyn Fn(usize) + Sync)) {
        struct Part<'a> {
            f: &'a (dyn Fn(usize) + Sync),
            index: usize,
        }
        unsafe fn call(data: *mut ()) {
            // SAFETY: a `Part` on the stack of `run`, which waits for DONE
            let part = unsafe { &*(data as *const Part) };
            (part.f)(part.index);
        }
        let mut parts: [Part; MAX_WORKERS] = std::array::from_fn(|k| Part { f, index: k + 1 });
        let sched = caller_sched();
        let mut waits: [Option<WaitDone>; MAX_WORKERS] = [const { None }; MAX_WORKERS];
        for k in 0..self.len {
            let Some(worker) = self.workers[k].take() else {
                continue;
            };
            let job = Job {
                data: &mut parts[k] as *mut Part as *mut (),
                run: call,
            };
            // SAFETY: this crew owns the worker; `waits` keeps `parts`
            // alive until it is done
            unsafe { worker.hand(job, sched) };
            waits[k] = Some(WaitDone(worker));
        }
        f(0);
        // Dropping `waits` waits for every worker (also if `f(0)` panics)
        drop(waits);
    }
}

impl Drop for Crew {
    fn drop(&mut self) {
        for worker in self.workers.iter_mut().filter_map(Option::take) {
            worker.state.store(IDLE, Ordering::Release);
        }
    }
}

/// Waits for the worker's job, then frees the worker
struct WaitDone(&'static Worker);

impl Drop for WaitDone {
    fn drop(&mut self) {
        spin_until(|| self.0.state.load(Ordering::Acquire) == DONE);
        self.0.state.store(IDLE, Ordering::Release);
    }
}

/// Wait for `done` by spinning: what is waited for runs on another core and
/// takes a fraction of the period, far less than waking up from a sleep
pub fn spin_until(done: impl Fn() -> bool) {
    let mut spins = 0u32;
    while !done() {
        spins = spins.wrapping_add(1);
        if spins.is_multiple_of(4096) {
            // Only matters when the other thread cannot get a core (not
            // real-time, or fewer cores than threads): let it run
            thread::yield_now();
        } else {
            std::hint::spin_loop();
        }
    }
}

/// An idle worker among those in use, taken for a job
fn claim() -> Option<&'static Worker> {
    let pool = POOL.get()?;
    let active = pool.active.load(Ordering::Acquire);
    pool.workers[..active.min(pool.workers.len())]
        .iter()
        .find(|w| {
            w.state
                .compare_exchange(IDLE, CLAIMED, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
        })
}

fn run_worker(worker: &'static Worker) {
    super::flush_denormals();
    let mut sched = 0;
    loop {
        // Spin a little after a job, then park until the next one
        let spin_until = Instant::now() + SPIN_AFTER_JOB;
        while worker.state.load(Ordering::Acquire) != READY {
            if Instant::now() < spin_until {
                std::hint::spin_loop();
            } else {
                thread::park();
            }
        }
        let wanted = worker.sched.load(Ordering::Relaxed);
        if wanted != sched {
            set_sched(wanted);
            sched = wanted;
        }
        // SAFETY: READY: the caller wrote the job and waits for DONE
        let job = unsafe { (*worker.job.get()).take() };
        if let Some(job) = job {
            // A panicking job must still report DONE, or its caller spins
            // forever; the rest of the chain goes on
            // SAFETY: see `join`
            // (A log off the audio thread, once per panic: the panic hook
            // has already printed it)
            if catch_unwind(AssertUnwindSafe(|| unsafe { (job.run)(job.data) })).is_err() {
                warn!("DSP job panicked, its part of the period is lost");
            }
        }
        worker.jobs.fetch_add(1, Ordering::Relaxed);
        worker.state.store(DONE, Ordering::Release);
    }
}

/// Scheduling of the calling thread, packed for `set_sched`: policy << 32 |
/// priority, + 1 so that 0 means unknown
#[cfg(unix)]
fn caller_sched() -> u64 {
    let mut policy = 0;
    // SAFETY: plain out-parameters
    let mut param: libc::sched_param = unsafe { std::mem::zeroed() };
    // SAFETY: the calling thread, and out-parameters valid for the call
    let ok = unsafe { libc::pthread_getschedparam(libc::pthread_self(), &mut policy, &mut param) };
    if ok != 0 {
        return 0;
    }
    ((policy as u64) << 32 | param.sched_priority as u32 as u64) + 1
}

#[cfg(not(unix))]
fn caller_sched() -> u64 {
    0
}

#[cfg(unix)]
fn set_sched(packed: u64) {
    if packed == 0 {
        return;
    }
    let packed = packed - 1;
    let policy = (packed >> 32) as libc::c_int;
    // SAFETY: plain in-parameter
    let mut param: libc::sched_param = unsafe { std::mem::zeroed() };
    param.sched_priority = packed as u32 as libc::c_int;
    // SAFETY: the calling thread, and an in-parameter valid for the call
    let err = unsafe { libc::pthread_setschedparam(libc::pthread_self(), policy, &param) };
    if err != 0 {
        // Once per change of the caller's scheduling, not per period
        warn!(
            error = err,
            "DSP worker cannot take the audio thread's priority"
        );
    }
}

#[cfg(not(unix))]
fn set_sched(_packed: u64) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_runs_both_sides_on_and_off_the_pool() {
        // Before start: both here
        let (mut a, mut b) = (0, 0);
        assert!(!join(|| a += 1, || b += 2) || workers() > 0);
        assert_eq!((a, b), (1, 2));

        start(2);
        let before = jobs_run();
        let mut out = [0u64; 2];
        let [x, y] = &mut out;
        let on_worker = join(|| *x = (1..=1000u64).sum(), || *y = (1..=2000u64).sum());
        assert_eq!(out, [500500, 2001000]);
        assert!(on_worker);
        assert!(jobs_run() > before);

        // Nested: a job hands work to another worker, or runs it itself
        let mut sums = [0u64; 4];
        let (left, right) = sums.split_at_mut(2);
        join(
            || {
                let (p, q) = left.split_at_mut(1);
                join(|| p[0] = 1, || q[0] = 2);
            },
            || {
                let (p, q) = right.split_at_mut(1);
                join(|| p[0] = 3, || q[0] = 4);
            },
        );
        assert_eq!(sums, [1, 2, 3, 4]);

        // A crew: every part runs once, on as many threads
        let crew = crew(3);
        let len = crew.len();
        assert!(len >= 1);
        let hits: [AtomicUsize; MAX_WORKERS + 1] = Default::default();
        crew.run(&|k| {
            hits[k].fetch_add(1, Ordering::Relaxed);
        });
        for (k, hit) in hits.iter().enumerate() {
            assert_eq!(hit.load(Ordering::Relaxed), usize::from(k <= len));
        }
        // Its workers are free again
        assert_eq!(super::crew(3).len(), len);

        // Turned off: everything here again
        start(0);
        let mut c = 0;
        assert!(!join(|| (), || c = 5));
        assert_eq!(c, 5);
        start(2);
    }
}
