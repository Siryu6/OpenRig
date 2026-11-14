//! The app without a touchscreen: the UI is drawn in memory by Slint's
//! software renderer and streamed to the full editor of the web remote,
//! whose pointer and keys come back as window events.
//!
//! A Slint platform of its own: one window (MinimalSoftwareWindow, which
//! keeps its picture and reports the areas that changed), and an event
//! loop running Slint's timers and the closures of other threads. Nothing
//! is drawn while no client shows the full editor; then at most
//! `FRAME_MS` apart, and only what changed. Input from the client cuts
//! the wait short: the frame showing its effect leaves as soon as the UI
//! has handled it (no closer than `INPUT_FRAME_MS` to the last one).
//! Pictures are encoded on a thread of their own (see Encoder).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{EventLoopProxy, Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{LogicalPosition, PhysicalSize, PlatformError, Rgb8Pixel};
use tracing::{debug, info, warn};

use pedalboard::remote::frames::{self, Encoding, Rect};
use pedalboard::remote::protocol::{Command, PointerKind};
use pedalboard::remote::Remote;

/// Frames at most every FRAME_MS (about 30 per second): animations and
/// changes nobody asked for
const FRAME_MS: u64 = 33;
/// After input, the next frame waits only this long since the last one
/// (moves come at most once per browser frame, so about 60 per second)
const INPUT_FRAME_MS: u64 = 16;
/// Window size until a client asks for one
pub const DEFAULT_SIZE: (u32, u32) = (1024, 600);

enum Event {
    Invoke(Box<dyn FnOnce() + Send>),
    Quit,
}

struct Proxy(mpsc::Sender<Event>);

impl EventLoopProxy for Proxy {
    fn quit_event_loop(&self) -> Result<(), slint::EventLoopError> {
        self.0
            .send(Event::Quit)
            .map_err(|_| slint::EventLoopError::EventLoopTerminated)
    }

    fn invoke_from_event_loop(
        &self,
        event: Box<dyn FnOnce() + Send>,
    ) -> Result<(), slint::EventLoopError> {
        self.0
            .send(Event::Invoke(event))
            .map_err(|_| slint::EventLoopError::EventLoopTerminated)
    }
}

thread_local! {
    /// The window, for input from the remote (UI thread)
    static WINDOW: RefCell<Option<Rc<MinimalSoftwareWindow>>> = const { RefCell::new(None) };
    /// The remote to stream to, once it runs
    static STREAM: RefCell<Option<Arc<Remote>>> = const { RefCell::new(None) };
    /// The pointer is down (a press not released yet)
    static PRESSED: Cell<bool> = const { Cell::new(false) };
    /// Input arrived since the last frame: draw its effect without waiting
    static INPUT: Cell<bool> = const { Cell::new(false) };
}

struct HeadlessPlatform {
    window: Rc<MinimalSoftwareWindow>,
    sender: mpsc::Sender<Event>,
    receiver: RefCell<Option<mpsc::Receiver<Event>>>,
}

impl Platform for HeadlessPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.window.clone())
    }

    fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
        Some(Box::new(Proxy(self.sender.clone())))
    }

    fn run_event_loop(&self) -> Result<(), PlatformError> {
        let receiver = self
            .receiver
            .borrow_mut()
            .take()
            .ok_or_else(|| PlatformError::Other("event loop already ran".into()))?;
        let mut screen = Screen::new(self.window.clone());
        loop {
            slint::platform::update_timers_and_animations();
            let next_frame = screen.frame();
            let mut wait = slint::platform::duration_until_next_timer_update()
                .unwrap_or(Duration::from_secs(1));
            if let Some(next) = next_frame {
                wait = wait.min(next);
            }
            match receiver.recv_timeout(wait) {
                Ok(Event::Invoke(f)) => f(),
                Ok(Event::Quit) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            // Everything waiting, then timers again
            loop {
                match receiver.try_recv() {
                    Ok(Event::Invoke(f)) => f(),
                    Ok(Event::Quit) => return Ok(()),
                    Err(_) => break,
                }
            }
        }
        Ok(())
    }
}

/// The picture of the window, streamed when clients want it
struct Screen {
    window: Rc<MinimalSoftwareWindow>,
    pixels: Vec<Rgb8Pixel>,
    /// The window as last sent: Slint redraws whole items (a label whose
    /// model row was set again), only the pixels that changed go
    sent: Vec<Rgb8Pixel>,
    size: (u32, u32),
    last: Instant,
    encoding: Encoding,
    encoder: Encoder,
    stats: Stats,
}

/// Frames and bytes sent, time spent on the UI thread and in the encoder,
/// logged every minute
struct Stats {
    frames: usize,
    bytes: usize,
    ui: Duration,
    ui_max: Duration,
    encode: Duration,
    encode_max: Duration,
    since: Instant,
}

impl Stats {
    fn new() -> Self {
        Self {
            frames: 0,
            bytes: 0,
            ui: Duration::ZERO,
            ui_max: Duration::ZERO,
            encode: Duration::ZERO,
            encode_max: Duration::ZERO,
            since: Instant::now(),
        }
    }

    fn encoded(&mut self, done: &Done) {
        self.frames += done.frames;
        self.bytes += done.bytes;
        self.encode += done.spent;
        self.encode_max = self.encode_max.max(done.spent);
    }

    fn drawn(&mut self, spent: Duration) {
        self.ui += spent;
        self.ui_max = self.ui_max.max(spent);
        if self.since.elapsed() > Duration::from_secs(60) {
            let ms = |d: Duration| d.as_millis() as u64;
            info!(
                frames = self.frames,
                kib = self.bytes / 1024,
                ui_ms = ms(self.ui),
                ui_max_ms = ms(self.ui_max),
                encode_ms = ms(self.encode),
                encode_max_ms = ms(self.encode_max),
                seconds = self.since.elapsed().as_secs(),
                "Remote screen stream"
            );
            *self = Self::new();
        }
    }
}

/// A frame for the encoder: the changed rectangles of the window and
/// their pixels, packed one after the other (rows of each in order)
struct Job {
    pixels: Vec<Rgb8Pixel>,
    rects: Vec<Rect>,
    window: (usize, usize),
    encoding: Encoding,
    remote: Arc<Remote>,
}

/// A job done: its buffers, to reuse, and what it cost
struct Done {
    pixels: Vec<Rgb8Pixel>,
    rects: Vec<Rect>,
    frames: usize,
    bytes: usize,
    spent: Duration,
}

/// Encode a job and send its frames, together: the areas of a picture
/// reach the page together. `rgb` is the encoder's own buffer.
fn encode(job: Job, rgb: &mut Vec<u8>) -> Done {
    let started = Instant::now();
    let mut picture = Vec::with_capacity(job.rects.len());
    let mut offset = 0;
    for &rect in &job.rects {
        let n = rect.width * rect.height;
        let Some(pixels) = job.pixels.get(offset..offset + n) else {
            break;
        };
        offset += n;
        rgb.clear();
        rgb.extend(pixels.iter().flat_map(|p| [p.r, p.g, p.b]));
        picture.extend(frames::encode_packed(rgb, job.window, rect, job.encoding));
    }
    let (frames, bytes) = (picture.len(), picture.iter().map(Vec::len).sum());
    job.remote.publish_picture(picture);
    Done {
        pixels: job.pixels,
        rects: job.rects,
        frames,
        bytes,
        spent: started.elapsed(),
    }
}

/// Encodes frames on a thread of its own, so a large picture (a page
/// change in a big window takes tens of ms) never holds up the UI. One
/// job at a time: while it runs, the UI keeps drawing into its window and
/// the next frame takes all that changed meanwhile. Its buffers come back
/// with each job done, so the memory stays that of one frame.
struct Encoder {
    worker: Option<(mpsc::Sender<Job>, mpsc::Receiver<Done>)>,
    /// Buffers of the next job: None while a job runs
    idle: Option<(Vec<Rgb8Pixel>, Vec<Rect>)>,
    /// Encoder buffer when there is no thread (encoding on the UI thread)
    rgb: Vec<u8>,
}

impl Encoder {
    fn new() -> Self {
        let (jobs, job_receiver) = mpsc::channel::<Job>();
        let (done_sender, done) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("remote-encode".into())
            .spawn(move || {
                let mut rgb = Vec::new();
                for job in job_receiver {
                    if done_sender.send(encode(job, &mut rgb)).is_err() {
                        break;
                    }
                    // Wake the UI loop: changes may wait for the next frame
                    let _ = slint::invoke_from_event_loop(|| {});
                }
            });
        let worker = match spawned {
            Ok(_) => Some((jobs, done)),
            Err(e) => {
                warn!("No encoder thread, encoding on the UI thread: {e}");
                None
            }
        };
        Self {
            worker,
            idle: Some((Vec::new(), Vec::new())),
            rgb: Vec::new(),
        }
    }

    /// The buffers for a new job, None while one runs
    fn ready(&mut self, stats: &mut Stats) -> Option<(Vec<Rgb8Pixel>, Vec<Rect>)> {
        if self.idle.is_none() {
            let (_, done) = self.worker.as_ref()?;
            match done.try_recv() {
                Ok(done) => {
                    stats.encoded(&done);
                    self.idle = Some((done.pixels, done.rects));
                }
                Err(mpsc::TryRecvError::Empty) => return None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    warn!("Encoder thread gone, encoding on the UI thread");
                    self.worker = None;
                    self.idle = Some((Vec::new(), Vec::new()));
                }
            }
        }
        self.idle.take()
    }

    fn start(&mut self, job: Job, stats: &mut Stats) {
        let job = match &self.worker {
            Some((jobs, _)) => match jobs.send(job) {
                Ok(()) => return,
                Err(mpsc::SendError(job)) => job,
            },
            None => job,
        };
        let done = encode(job, &mut self.rgb);
        stats.encoded(&done);
        self.idle = Some((done.pixels, done.rects));
    }
}

impl Screen {
    fn new(window: Rc<MinimalSoftwareWindow>) -> Self {
        let size = window.size();
        Self {
            window,
            pixels: Vec::new(),
            sent: Vec::new(),
            size: (size.width, size.height),
            last: Instant::now() - Duration::from_secs(1),
            encoding: Encoding::from_env(),
            encoder: Encoder::new(),
            stats: Stats::new(),
        }
    }

    /// Draw and send what changed, when a client streams and the last
    /// frame is old enough. Returns how long to wait for the next one.
    fn frame(&mut self) -> Option<Duration> {
        let remote = STREAM.with(|s| s.borrow().clone())?;
        if !remote.stream_wanted() {
            return None;
        }
        let since = self.last.elapsed();
        let period = Duration::from_millis(FRAME_MS);
        let gap = frame_gap(INPUT.with(Cell::get));
        if since < gap {
            return Some(gap - since);
        }
        // The encoder wakes the loop when it is done with the last frame
        let Some((mut packed, mut rects)) = self.encoder.ready(&mut self.stats) else {
            return Some(period);
        };
        if let Some((w, h)) = remote.take_stream_size() {
            if (w, h) != self.size {
                info!(width = w, height = h, "Remote screen resized");
                self.window.set_size(PhysicalSize::new(w, h));
                self.size = (w, h);
            }
        }
        INPUT.with(|i| i.set(false));
        let keyframe = remote.take_keyframe();
        // What a client missed, from the window as it is now
        let repaint = remote.take_repaint();
        if keyframe {
            self.window.request_redraw();
        }
        let (w, h) = (self.size.0 as usize, self.size.1 as usize);
        if self.pixels.len() != w * h {
            self.pixels = vec![Rgb8Pixel::default(); w * h];
            self.window.request_redraw();
        }
        let started = Instant::now();
        rects.clear();
        let pixels = &mut self.pixels;
        let drawn = self.window.draw_if_needed(|renderer| {
            let region = renderer.render(pixels, w);
            rects.extend(region.iter().map(|(origin, size)| Rect {
                x: origin.x.max(0) as usize,
                y: origin.y.max(0) as usize,
                width: size.width as usize,
                height: size.height as usize,
            }));
        });
        if keyframe {
            rects.clear();
            rects.push(Rect {
                x: 0,
                y: 0,
                width: w,
                height: h,
            });
        }
        let window = (w, h);
        let clip = |rect: &mut Rect| match frames::clip(*rect, window) {
            Some(clipped) => {
                *rect = clipped;
                true
            }
            None => false,
        };
        rects.retain_mut(clip);
        if keyframe || self.sent.len() != self.pixels.len() {
            self.sent.clone_from(&self.pixels);
        } else {
            let (now, sent) = (&self.pixels, &mut self.sent);
            rects.retain_mut(|rect| match frames::changed(*rect, now, sent, w) {
                Some(changed) => {
                    *rect = changed;
                    true
                }
                None => false,
            });
            for rect in &rects {
                for row in rect.y..rect.y + rect.height {
                    let start = row * w + rect.x;
                    let span = start..start + rect.width;
                    sent[span.clone()].copy_from_slice(&now[span]);
                }
            }
            rects.extend_from_slice(repaint.rects());
            rects.retain_mut(clip);
        }
        // Close areas go as one picture
        frames::merge_close(&mut rects);
        if !drawn && !keyframe && repaint.is_empty() {
            self.encoder.idle = Some((packed, rects));
            // Nothing changed: look again soon (an animation may be running)
            return Some(if self.window.has_active_animations() {
                period
            } else {
                Duration::from_millis(FRAME_MS * 2)
            });
        }
        self.last = Instant::now();
        // Only the areas sent leave the window: copies of their rows
        packed.clear();
        for rect in &rects {
            for row in rect.y..rect.y + rect.height {
                let start = row * w + rect.x;
                packed.extend_from_slice(&self.pixels[start..start + rect.width]);
            }
        }
        if rects.is_empty() {
            self.encoder.idle = Some((packed, rects));
        } else {
            let job = Job {
                pixels: packed,
                rects,
                window: (w, h),
                encoding: self.encoding,
                remote,
            };
            self.encoder.start(job, &mut self.stats);
        }
        self.stats.drawn(started.elapsed());
        Some(period)
    }
}

/// Shortest time between two frames: shorter when input waits to be seen
fn frame_gap(input: bool) -> Duration {
    Duration::from_millis(if input { INPUT_FRAME_MS } else { FRAME_MS })
}

/// Make the app headless: call before the UI is created
pub fn install() -> Result<(), PlatformError> {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    window.set_size(PhysicalSize::new(DEFAULT_SIZE.0, DEFAULT_SIZE.1));
    WINDOW.with(|w| *w.borrow_mut() = Some(window.clone()));
    let (sender, receiver) = mpsc::channel();
    slint::platform::set_platform(Box::new(HeadlessPlatform {
        window,
        sender,
        receiver: RefCell::new(Some(receiver)),
    }))
    .map_err(|e| PlatformError::Other(format!("{e}")))
}

/// Stream the window to the clients of `remote` that want it
pub fn stream_to(remote: Option<Arc<Remote>>) {
    STREAM.with(|s| *s.borrow_mut() = remote);
}

/// Pointer or key from the full editor (UI thread)
pub fn input(command: Command) {
    let Some(window) = WINDOW.with(|w| w.borrow().clone()) else {
        return;
    };
    let event = match command {
        Command::Pointer { kind, x, y, dx, dy } => {
            let position = LogicalPosition::new(x, y);
            match kind {
                PointerKind::Down => {
                    PRESSED.with(|p| p.set(true));
                    WindowEvent::PointerPressed {
                        position,
                        button: PointerEventButton::Left,
                    }
                }
                PointerKind::Up => {
                    PRESSED.with(|p| p.set(false));
                    WindowEvent::PointerReleased {
                        position,
                        button: PointerEventButton::Left,
                    }
                }
                PointerKind::Move => WindowEvent::PointerMoved { position },
                PointerKind::Scroll => WindowEvent::PointerScrolled {
                    position,
                    delta_x: dx,
                    delta_y: dy,
                },
                PointerKind::Leave => {
                    // A press the page lost (finger off the canvas) ends here
                    if PRESSED.with(|p| p.replace(false)) {
                        window.dispatch_event(WindowEvent::PointerReleased {
                            position,
                            button: PointerEventButton::Left,
                        });
                    }
                    WindowEvent::PointerExited
                }
            }
        }
        Command::Key { text, down } => {
            let text: slint::SharedString = text.into();
            if down {
                WindowEvent::KeyPressed { text }
            } else {
                WindowEvent::KeyReleased { text }
            }
        }
        other => {
            debug!(?other, "Not an input event");
            return;
        }
    };
    window.dispatch_event(event);
    // The loop draws right after this (it runs frame() once the events
    // waiting are handled): show the result without the full period
    INPUT.with(|i| i.set(true));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_shortens_the_wait_for_the_next_frame() {
        assert_eq!(frame_gap(false), Duration::from_millis(FRAME_MS));
        assert!(frame_gap(true) < frame_gap(false));
        // Still a cap: never a frame per event
        assert!(frame_gap(true) >= Duration::from_millis(8));
    }
}
