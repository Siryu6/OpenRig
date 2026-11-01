//! LV2 Worker (`work:schedule`): plugins hand slow jobs (loading a model or
//! a file, applying a state) to a thread of their own instead of doing them
//! in `run`.
//!
//! The plugin schedules a job from `run` (audio thread): it is copied into a
//! lock-free ring and the worker thread is woken. The worker calls the
//! plugin's `work`, whose responses go back through a second ring; they are
//! handed to the plugin after its next `run`, followed by `end_run`. Nothing
//! on the audio side allocates or locks.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::{JoinHandle, Thread};

pub const SCHEDULE: &str = "http://lv2plug.in/ns/ext/worker#schedule";
pub const INTERFACE: &str = "http://lv2plug.in/ns/ext/worker#interface";

/// Bytes of each ring (two per instance): jobs and responses are small (a
/// pointer, a path, a state value), several can wait at once. A larger one
/// is refused with ERR_NO_SPACE.
const RING_BYTES: usize = 1 << 13;
/// Size prefix of each message in a ring
const HEADER: usize = std::mem::size_of::<u32>();

const SUCCESS: u32 = 0;
const ERR_UNKNOWN: u32 = 1;
const ERR_NO_SPACE: u32 = 2;

type Respond = unsafe extern "C" fn(*mut c_void, u32, *const c_void) -> u32;

/// LV2_Worker_Interface, provided by the plugin
#[repr(C)]
pub struct Interface {
    work:
        Option<unsafe extern "C" fn(*mut c_void, Respond, *mut c_void, u32, *const c_void) -> u32>,
    work_response: Option<unsafe extern "C" fn(*mut c_void, u32, *const c_void) -> u32>,
    end_run: Option<unsafe extern "C" fn(*mut c_void) -> u32>,
}

/// LV2_Worker_Schedule, the feature given to the plugin
#[repr(C)]
struct Schedule {
    handle: *mut c_void,
    schedule_work: unsafe extern "C" fn(*mut c_void, u32, *const c_void) -> u32,
}

/// Single-producer single-consumer ring of size-prefixed messages. The
/// bytes between `read` and `write` belong to the reader, the rest to the
/// writer; the positions publish them (acquire / release), so the bytes
/// themselves are relaxed atomics: no unsafe, no lock.
struct Ring {
    buffer: Box<[AtomicU8]>,
    /// Positions written and read so far (wrapping), each owned by one side
    write: AtomicUsize,
    read: AtomicUsize,
}

impl Ring {
    fn new(capacity: usize) -> Self {
        Self {
            buffer: (0..capacity.max(1)).map(|_| AtomicU8::new(0)).collect(),
            write: AtomicUsize::new(0),
            read: AtomicUsize::new(0),
        }
    }

    fn capacity(&self) -> usize {
        self.buffer.len()
    }

    /// The byte at position `at`, wrapping around the end
    fn byte(&self, at: usize) -> Option<&AtomicU8> {
        self.buffer.get(at % self.capacity())
    }

    /// Copy `data` in at position `at`
    fn copy_in(&self, at: usize, data: &[u8]) {
        for (i, value) in data.iter().enumerate() {
            if let Some(byte) = self.byte(at.wrapping_add(i)) {
                byte.store(*value, Ordering::Relaxed);
            }
        }
    }

    /// Copy the bytes at position `at` out into `out`
    fn copy_out(&self, at: usize, out: &mut [u8]) {
        for (i, value) in out.iter_mut().enumerate() {
            if let Some(byte) = self.byte(at.wrapping_add(i)) {
                *value = byte.load(Ordering::Relaxed);
            }
        }
    }

    fn is_empty(&self) -> bool {
        self.write.load(Ordering::Acquire) == self.read.load(Ordering::Acquire)
    }

    /// Queue a message; false if there is no room for it
    fn push(&self, message: &[u8]) -> bool {
        let write = self.write.load(Ordering::Relaxed);
        let read = self.read.load(Ordering::Acquire);
        // None if the positions make no sense (see `pop`)
        let Some(free) = self.capacity().checked_sub(write.wrapping_sub(read)) else {
            return false;
        };
        let Ok(size) = u32::try_from(message.len()) else {
            return false;
        };
        if HEADER.saturating_add(message.len()) > free {
            return false;
        }
        self.copy_in(write, &size.to_ne_bytes());
        self.copy_in(write.wrapping_add(HEADER), message);
        self.write.store(
            write.wrapping_add(HEADER + message.len()),
            Ordering::Release,
        );
        true
    }

    /// Take the oldest message into `out` (as large as the ring); its size
    fn pop(&self, out: &mut [u8]) -> Option<usize> {
        let read = self.read.load(Ordering::Relaxed);
        let write = self.write.load(Ordering::Acquire);
        if write == read {
            return None;
        }
        let used = write.wrapping_sub(read);
        let mut size = [0u8; HEADER];
        self.copy_out(read, &mut size);
        let size = u32::from_ne_bytes(size) as usize;
        // Never larger than what was written (`push` checks), nor than
        // `out` then. Else two threads pushed at once (a plugin breaking
        // the Worker contract) and mixed up the messages: drop them all.
        let sane = used <= self.capacity() && used >= HEADER && size <= used - HEADER;
        let Some(out) = out.get_mut(..size).filter(|_| sane) else {
            self.read.store(write, Ordering::Release);
            return None;
        };
        self.copy_out(read.wrapping_add(HEADER), out);
        self.read
            .store(read.wrapping_add(HEADER + size), Ordering::Release);
        Some(size)
    }
}

/// What the plugin, the audio thread and the worker thread share
pub struct Channel {
    jobs: Ring,
    responses: Ring,
    /// Jobs scheduled and not worked yet
    pending: AtomicUsize,
    worker: OnceLock<Thread>,
    stop: AtomicBool,
}

/// `schedule_work`: called by the plugin, from `run` most of the time
///
/// # Safety
/// `handle` is the one of our `Schedule` (a live `Channel`, kept by the
/// `Feature` that outlives the instance); `data` holds `size` bytes.
unsafe extern "C" fn schedule_work(handle: *mut c_void, size: u32, data: *const c_void) -> u32 {
    // SAFETY: see the function's contract
    let channel = unsafe { &*(handle as *const Channel) };
    // SAFETY: see the function's contract
    let Some(job) = (unsafe { message(size, data) }) else {
        return ERR_UNKNOWN;
    };
    // Counted before the worker can take it
    channel.pending.fetch_add(1, Ordering::AcqRel);
    if !channel.jobs.push(job) {
        channel.pending.fetch_sub(1, Ordering::AcqRel);
        return ERR_NO_SPACE;
    }
    if let Some(worker) = channel.worker.get() {
        worker.unpark();
    }
    SUCCESS
}

/// `respond`: called by the plugin from `work`, on the worker thread
///
/// # Safety
/// `handle` is the `Channel` the worker thread passed to `work`, alive while
/// it runs; `data` holds `size` bytes.
unsafe extern "C" fn respond(handle: *mut c_void, size: u32, data: *const c_void) -> u32 {
    // SAFETY: see the function's contract
    let channel = unsafe { &*(handle as *const Channel) };
    // SAFETY: see the function's contract
    match unsafe { message(size, data) } {
        Some(response) if channel.responses.push(response) => SUCCESS,
        Some(_) => ERR_NO_SPACE,
        None => ERR_UNKNOWN,
    }
}

/// The bytes a plugin passes, if they make sense
///
/// # Safety
/// A non-null `data` points to `size` readable bytes that stay unchanged
/// for `'a` (the LV2 Worker contract: until the call returns).
unsafe fn message<'a>(size: u32, data: *const c_void) -> Option<&'a [u8]> {
    match (size, data.is_null()) {
        (0, _) => Some(&[]),
        (_, true) => None,
        // SAFETY: see the function's contract
        (size, false) => Some(unsafe { std::slice::from_raw_parts(data.cast(), size as usize) }),
    }
}

/// The `work:schedule` feature of one plugin instance. Kept with the other
/// features, so it outlives the instance.
pub struct Feature {
    channel: Arc<Channel>,
    schedule: Box<Schedule>,
}

// SAFETY: the raw pointer of `schedule` only points to the Channel, which is
// Sync and kept alive by `channel`.
unsafe impl Send for Feature {}

impl Feature {
    pub fn new() -> Self {
        let channel = Arc::new(Channel {
            jobs: Ring::new(RING_BYTES),
            responses: Ring::new(RING_BYTES),
            pending: AtomicUsize::new(0),
            worker: OnceLock::new(),
            stop: AtomicBool::new(false),
        });
        let schedule = Box::new(Schedule {
            handle: Arc::as_ptr(&channel) as *mut c_void,
            schedule_work,
        });
        Self { channel, schedule }
    }

    /// Data of the LV2 feature
    pub fn data(&self) -> *mut c_void {
        &*self.schedule as *const Schedule as *mut c_void
    }
}

impl Default for Feature {
    fn default() -> Self {
        Self::new()
    }
}

/// The plugin handle, used by the worker thread and the audio thread: the
/// Worker extension allows `work` to run alongside `run`
#[derive(Clone, Copy)]
struct Handle(*mut c_void);

// SAFETY: the Worker extension lets `work` run on another thread than `run`
// (and the plugin's `instantiate` thread); the handle is only used so.
unsafe impl Send for Handle {}

/// Worker thread of one plugin instance. Must be dropped before the
/// instance: dropping it waits for the job in progress.
pub struct Worker {
    channel: Arc<Channel>,
    interface: NonNull<Interface>,
    handle: Handle,
    /// Response being handed to the plugin (audio thread)
    response: Box<[u8]>,
    thread: Option<JoinHandle<()>>,
}

// SAFETY: the interface is static plugin data; the handle is only used as
// the Worker extension allows (see `Handle`).
unsafe impl Send for Worker {}

impl Worker {
    /// Start the worker of `handle`, an instance created with `feature`
    ///
    /// # Safety
    /// `interface` and `handle` must stay valid until the Worker is dropped.
    pub unsafe fn start(
        feature: &Feature,
        interface: NonNull<Interface>,
        handle: *mut c_void,
    ) -> std::io::Result<Self> {
        let channel = feature.channel.clone();
        // SAFETY: valid until the Worker is dropped (the caller's contract)
        let work = unsafe { interface.as_ref() }.work;
        let (thread_channel, thread_handle) = (channel.clone(), Handle(handle));
        let thread = std::thread::Builder::new()
            .name("lv2-worker".into())
            .spawn(move || {
                let (channel, handle) = (thread_channel, thread_handle);
                let mut job = vec![0u8; RING_BYTES];
                let respond_to = Arc::as_ptr(&channel) as *mut c_void;
                loop {
                    match channel.jobs.pop(&mut job) {
                        Some(size) => {
                            if let Some(work) = work {
                                // SAFETY: the plugin's own `work`, on its
                                // handle (valid while the Worker lives: this
                                // thread is joined on drop), with our
                                // `respond` and its live Channel, and `size`
                                // bytes of `job`
                                unsafe {
                                    work(
                                        handle.0,
                                        respond,
                                        respond_to,
                                        size as u32,
                                        job.as_ptr().cast(),
                                    )
                                };
                            }
                            channel.pending.fetch_sub(1, Ordering::AcqRel);
                        }
                        None if channel.stop.load(Ordering::Acquire) => return,
                        None => std::thread::park(),
                    }
                }
            })?;
        let _ = channel.worker.set(thread.thread().clone());
        // A job scheduled before the thread was known woke nobody
        thread.thread().unpark();
        Ok(Self {
            channel,
            interface,
            handle: Handle(handle),
            response: vec![0u8; RING_BYTES].into_boxed_slice(),
            thread: Some(thread),
        })
    }

    /// After each `run` (audio thread): hand the responses of the finished
    /// jobs to the plugin, then end its cycle
    pub fn end_run(&mut self) {
        // SAFETY: valid until the Worker is dropped (`start`'s contract)
        let interface = unsafe { self.interface.as_ref() };
        while let Some(size) = self.channel.responses.pop(&mut self.response) {
            if let Some(work_response) = interface.work_response {
                // SAFETY: the plugin's own callback on its live handle, from
                // the audio thread after `run`, with `size` bytes
                unsafe { work_response(self.handle.0, size as u32, self.response.as_ptr().cast()) };
            }
        }
        if let Some(end_run) = interface.end_run {
            // SAFETY: as above
            unsafe { end_run(self.handle.0) };
        }
    }
}

impl Worker {
    /// True once every job scheduled so far is worked and its responses
    /// handed to the plugin (by `end_run`)
    pub fn is_idle(&self) -> bool {
        self.channel.pending.load(Ordering::Acquire) == 0 && self.channel.responses.is_empty()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.channel.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}

// A fake C plugin drives the worker through raw pointers, as lilv would
#[cfg(test)]
#[allow(unsafe_op_in_unsafe_fn, clippy::undocumented_unsafe_blocks)]
mod tests {
    use super::*;
    use crate::sync::MutexExt;
    use std::time::{Duration, Instant};

    #[test]
    fn ring_keeps_messages_in_order_across_the_end() {
        let ring = Ring::new(64);
        let mut out = vec![0u8; 64];
        for round in 0..50u8 {
            let message = [round; 13];
            assert!(ring.push(&message));
            assert!(ring.push(&[]));
            assert_eq!(ring.pop(&mut out), Some(13));
            assert_eq!(&out[..13], &message);
            assert_eq!(ring.pop(&mut out), Some(0));
        }
        assert_eq!(ring.pop(&mut out), None);
    }

    #[test]
    fn ring_refuses_what_does_not_fit() {
        let ring = Ring::new(32);
        assert!(ring.push(&[1; 28]));
        assert!(!ring.push(&[]));
        let mut out = vec![0u8; 32];
        assert_eq!(ring.pop(&mut out), Some(28));
        assert!(ring.push(&[2; 20]));
    }

    /// A fake plugin: `work` answers each job with the job doubled, the
    /// responses and the `end_run` calls are counted
    #[derive(Default)]
    struct Plugin {
        received: std::sync::Mutex<Vec<Vec<u8>>>,
        end_runs: AtomicUsize,
    }

    unsafe extern "C" fn work(
        _handle: *mut c_void,
        respond: Respond,
        respond_to: *mut c_void,
        size: u32,
        data: *const c_void,
    ) -> u32 {
        let job = std::slice::from_raw_parts(data as *const u8, size as usize);
        let answer: Vec<u8> = job.iter().map(|b| b * 2).collect();
        respond(respond_to, answer.len() as u32, answer.as_ptr().cast())
    }

    unsafe extern "C" fn work_response(handle: *mut c_void, size: u32, data: *const c_void) -> u32 {
        let plugin = &*(handle as *const Plugin);
        let response = std::slice::from_raw_parts(data as *const u8, size as usize);
        plugin.received.locked().push(response.to_vec());
        SUCCESS
    }

    unsafe extern "C" fn end_run(handle: *mut c_void) -> u32 {
        let plugin = &*(handle as *const Plugin);
        plugin.end_runs.fetch_add(1, Ordering::Relaxed);
        SUCCESS
    }

    static INTERFACE_IMPL: Interface = Interface {
        work: Some(work),
        work_response: Some(work_response),
        end_run: Some(end_run),
    };

    #[test]
    fn jobs_are_worked_and_answered_after_a_run() {
        let plugin = Plugin::default();
        let feature = Feature::new();
        let schedule = unsafe { &*(feature.data() as *const Schedule) };
        // Scheduled before the worker starts, as during instantiation
        assert_eq!(
            unsafe { (schedule.schedule_work)(schedule.handle, 2, [1u8, 2].as_ptr().cast()) },
            SUCCESS
        );
        let mut worker = unsafe {
            Worker::start(
                &feature,
                NonNull::from(&INTERFACE_IMPL),
                &plugin as *const Plugin as *mut c_void,
            )
        }
        .unwrap();
        assert_eq!(
            unsafe { (schedule.schedule_work)(schedule.handle, 1, [5u8].as_ptr().cast()) },
            SUCCESS
        );

        assert!(!worker.is_idle());
        let deadline = Instant::now() + Duration::from_secs(5);
        while !worker.is_idle() && Instant::now() < deadline {
            worker.end_run();
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(*plugin.received.locked(), vec![vec![2u8, 4], vec![10]]);
        assert!(plugin.end_runs.load(Ordering::Relaxed) >= 1);
        drop(worker);
    }

    #[test]
    fn a_full_ring_is_reported_to_the_plugin() {
        let feature = Feature::new();
        let schedule = unsafe { &*(feature.data() as *const Schedule) };
        let big = vec![0u8; RING_BYTES];
        assert_eq!(
            unsafe {
                (schedule.schedule_work)(schedule.handle, big.len() as u32, big.as_ptr().cast())
            },
            ERR_NO_SPACE
        );
        assert_eq!(
            unsafe { (schedule.schedule_work)(schedule.handle, 4, std::ptr::null()) },
            ERR_UNKNOWN
        );
    }
}
