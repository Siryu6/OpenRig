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

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{EventLoopProxy, Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{LogicalPosition, PhysicalSize, PlatformError, Rgb8Pixel};
use tracing::{debug, info};

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
    rgb: Vec<u8>,
    size: (u32, u32),
    last: Instant,
    encoding: Encoding,
    /// Bytes and frames sent, time spent, logged every minute
    stats: (usize, usize, Duration, Instant),
}

impl Screen {
    fn new(window: Rc<MinimalSoftwareWindow>) -> Self {
        let size = window.size();
        Self {
            window,
            pixels: Vec::new(),
            rgb: Vec::new(),
            size: (size.width, size.height),
            last: Instant::now() - Duration::from_secs(1),
            encoding: Encoding::from_env(),
            stats: (0, 0, Duration::ZERO, Instant::now()),
        }
    }

    /// Draw and send what changed, when a client streams and the last
    /// frame is old enough. Returns how long to wait for the next one.
    fn frame(&mut self) -> Option<Duration> {
        let remote = STREAM.with(|s| s.borrow().clone())?;
        if !remote.stream_wanted() {
            return None;
        }
        if let Some((w, h)) = remote.take_stream_size() {
            if (w, h) != self.size {
                info!(width = w, height = h, "Remote screen resized");
                self.window.set_size(PhysicalSize::new(w, h));
                self.size = (w, h);
            }
        }
        let since = self.last.elapsed();
        let period = Duration::from_millis(FRAME_MS);
        let gap = frame_gap(INPUT.with(Cell::get));
        if since < gap {
            return Some(gap - since);
        }
        INPUT.with(|i| i.set(false));
        let keyframe = remote.take_keyframe();
        if keyframe {
            self.window.request_redraw();
        }
        let (w, h) = (self.size.0 as usize, self.size.1 as usize);
        if self.pixels.len() != w * h {
            self.pixels = vec![Rgb8Pixel::default(); w * h];
            self.window.request_redraw();
        }
        let started = Instant::now();
        let mut rects: Vec<Rect> = Vec::new();
        let pixels = &mut self.pixels;
        let drawn = self.window.draw_if_needed(|renderer| {
            let region = renderer.render(pixels, w);
            rects = region
                .iter()
                .map(|(origin, size)| Rect {
                    x: origin.x.max(0) as usize,
                    y: origin.y.max(0) as usize,
                    width: size.width as usize,
                    height: size.height as usize,
                })
                .collect();
        });
        if keyframe {
            rects = vec![Rect {
                x: 0,
                y: 0,
                width: w,
                height: h,
            }];
        }
        if !drawn && !keyframe {
            // Nothing changed: look again soon (an animation may be running)
            return Some(if self.window.has_active_animations() {
                period
            } else {
                Duration::from_millis(FRAME_MS * 2)
            });
        }
        self.last = Instant::now();
        if rects.is_empty() {
            return Some(period);
        }
        self.rgb.clear();
        self.rgb.reserve(w * h * 3);
        for p in &self.pixels {
            self.rgb.extend_from_slice(&[p.r, p.g, p.b]);
        }
        for rect in rects {
            if let Some(frame) = frames::encode(&self.rgb, (w, h), rect, self.encoding) {
                self.stats.0 += frame.len();
                self.stats.1 += 1;
                remote.publish_frame(frame);
            }
        }
        self.stats.2 += started.elapsed();
        if self.stats.3.elapsed() > Duration::from_secs(60) {
            let (bytes, count, spent, since) = self.stats;
            info!(
                frames = count,
                kib = bytes / 1024,
                encode_ms = spent.as_millis() as u64,
                seconds = since.elapsed().as_secs(),
                "Remote screen stream"
            );
            self.stats = (0, 0, Duration::ZERO, Instant::now());
        }
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
