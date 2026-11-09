//! Looper page: what it shows (state, bar and beat, waveform), and its
//! keys beyond the transport (wired in `setup_callbacks`): multiply, half
//! speed and the saved loops. Saving and loading run on a worker thread;
//! the audio thread only ever sees the finished layer.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use slint::{ComponentHandle, Model, VecModel};

use pedalboard::audio::loop_files::{self, SavedLoop};
use pedalboard::audio::looper::{Looper, LooperState as Engine, SnapshotError};
use pedalboard::state::AppState;
use pedalboard::sync::MutexExt;

use crate::{model, show_status, App, LooperState, SavedLoopInfo};

/// Quieter loops are drawn at this level, relative to their peak, at least
const MIN_WAVE_SCALE: f32 = 0.02;

thread_local! {
    /// Version of the waveform shown, and its points (updated in place, so
    /// only the points that changed are drawn again)
    static WAVE_SHOWN: Cell<u64> = const { Cell::new(u64::MAX) };
    static WAVE: RefCell<Option<Rc<VecModel<f32>>>> = const { RefCell::new(None) };
}

pub fn setup(app: &App, state: &Arc<AppState>) {
    macro_rules! on {
        ($setter:ident, |$app:ident, $state:ident $(, $arg:ident)*| $body:expr) => {{
            let (weak, st) = (app.as_weak(), state.clone());
            app.$setter(move |$($arg),*| {
                if let Some($app) = weak.upgrade() {
                    let $state: &Arc<AppState> = &st;
                    $body;
                }
            });
        }};
    }
    on!(on_looper_multiply, |app, state| {
        if !state.looper_control(|l| l.multiply(2)) {
            show_status(&app, "The loop cannot be doubled: 30 s at most".into());
        }
    });
    on!(on_looper_half_speed_changed, |_app, state, on| {
        state.looper_control(|l| l.set_half_speed(on))
    });
    on!(on_looper_loops_opened, |app, state| {
        app.set_looper_default_name(loop_files::default_name(chrono::Local::now()).into());
        refresh_loops(&app, state, |_| None);
    });
    on!(on_looper_save, |app, state, name| save(
        &app,
        state,
        name.to_string()
    ));
    on!(on_looper_load, |app, state, id| load(
        &app,
        state,
        id.to_string()
    ));
    on!(on_looper_delete, |app, state, id| {
        let id = id.to_string();
        refresh_loops(&app, state, move |dir| {
            loop_files::delete(dir, &id).err().map(|e| format!("{e:#}"))
        });
    });
}

/// "4/4", or "6/8" for the compound bars (as Meter.signature)
fn signature(beats: u8) -> String {
    let unit = if matches!(beats, 6 | 9 | 12) { 8 } else { 4 };
    format!("{beats}/{unit}")
}

/// "m:ss.t"
fn time(seconds: f32) -> String {
    let tenths = (seconds.max(0.0) * 10.0).round() as u32;
    format!("{}:{:02}.{}", tenths / 600, tenths / 10 % 60, tenths % 10)
}

/// Run `work` in the loops folder (a message for the page, if any), then
/// read the saved loops and show them, on a worker thread: the files are
/// read from disk, possibly a slow SD card
fn refresh_loops(
    app: &App,
    state: &Arc<AppState>,
    work: impl FnOnce(&Path) -> Option<String> + Send + 'static,
) {
    let (weak, state) = (app.as_weak(), state.clone());
    let spawned = std::thread::Builder::new()
        .name("loop-list".into())
        .spawn(move || {
            let dir = state.loops_dir();
            let status = work(&dir);
            let loops = loop_files::list(&dir);
            let _ = weak.upgrade_in_event_loop(move |app| {
                if let Some(status) = status {
                    show_status(&app, status.into());
                }
                show_loops(&app, loops);
            });
        });
    if spawned.is_err() {
        show_status(app, "Loops not read".into());
    }
}

fn show_loops(app: &App, loops: Vec<SavedLoop>) {
    let loops = loops
        .into_iter()
        .map(|l| {
            let detail = match l.bars {
                0 => time(l.seconds),
                bars => format!(
                    "{} · {} BPM {} · {}",
                    if bars == 1 {
                        "1 bar".into()
                    } else {
                        format!("{bars} bars")
                    },
                    l.bpm.round(),
                    signature(l.beats_per_bar),
                    time(l.seconds)
                ),
            };
            SavedLoopInfo {
                id: l.id.into(),
                name: l.name.into(),
                detail: detail.into(),
            }
        })
        .collect();
    app.set_looper_saved(model(loops));
}

fn save(app: &App, state: &Arc<AppState>, name: String) {
    let (weak, state) = (app.as_weak(), state.clone());
    let spawned = std::thread::Builder::new()
        .name("loop-save".into())
        .spawn(move || {
            let result = match Looper::snapshot(&state.looper) {
                Err(SnapshotError::NoLoop) => {
                    Err("Nothing to save: record a loop first".to_string())
                }
                Err(SnapshotError::Overdubbing) => {
                    Err("Loop not saved: finish the overdub first".to_string())
                }
                Err(SnapshotError::Changed) => {
                    Err("Loop not saved: it kept changing, save it again".to_string())
                }
                Ok(audio) => {
                    loop_files::save(&state.loops_dir(), &name, &audio, chrono::Local::now())
                        .map(|saved| saved.name)
                        .map_err(|e| format!("Loop not saved: {e:#}"))
                }
            };
            let loops = loop_files::list(&state.loops_dir());
            let _ = weak.upgrade_in_event_loop(move |app| {
                match result {
                    Ok(name) => show_status(&app, format!("Loop saved: {name}").into()),
                    Err(e) => show_status(&app, e.into()),
                }
                show_loops(&app, loops);
            });
        });
    if spawned.is_err() {
        show_status(app, "Loop not saved".into());
    }
}

fn load(app: &App, state: &Arc<AppState>, id: String) {
    let (weak, state) = (app.as_weak(), state.clone());
    let spawned = std::thread::Builder::new()
        .name("loop-load".into())
        .spawn(move || {
            let result = loop_files::load(&state.loops_dir(), &id)
                .map_err(|e| format!("{e:#}"))
                .and_then(|audio| Looper::load(&state.looper, &audio));
            let _ = weak.upgrade_in_event_loop(move |app| match result {
                Ok(()) => app.set_looper_show_loops(false),
                Err(e) => show_status(&app, format!("Loop not loaded: {e}").into()),
            });
        });
    if spawned.is_err() {
        show_status(app, "Loop not loaded".into());
    }
}

/// Refresh the page from the looper (every poll)
pub fn poll(app: &App, state: &AppState) {
    // (copied out in one lock: the audio thread skips the looper for a
    // cycle when the lock is taken)
    let (status, wave) = {
        let looper = state.looper.locked();
        let status = looper.status();
        let changed = WAVE_SHOWN.get() != status.wave_version;
        (status, changed.then(|| looper.waveform()))
    };
    app.set_looper_state(match status.state {
        Engine::Stopped => LooperState::Stopped,
        Engine::CountIn => LooperState::CountIn,
        Engine::Recording => LooperState::Recording,
        Engine::Playing => LooperState::Playing,
        Engine::Overdubbing => LooperState::Overdubbing,
    });
    app.set_looper_length(status.length);
    app.set_looper_position(status.position);
    app.set_looper_layers(status.layers as i32);
    let (bar, beat) = status.beat.unwrap_or((0, 0));
    app.set_looper_bar(bar as i32);
    app.set_looper_beat(beat as i32);
    app.set_looper_beats(status.beats_per_bar as i32);
    app.set_looper_bars(status.bars as i32);
    app.set_looper_ending(status.ending);
    app.set_looper_half_speed(status.half_speed);
    app.set_looper_can_double(status.can_double);
    if let Some(wave) = wave {
        WAVE_SHOWN.set(status.wave_version);
        show_wave(app, &scaled(wave));
    }
}

/// Peaks scaled to the loudest one, so a quiet loop still shows its shape
fn scaled(mut wave: Vec<f32>) -> Vec<f32> {
    let top = wave.iter().fold(0.0f32, |a, &b| a.max(b));
    let scale = 1.0 / top.max(MIN_WAVE_SCALE);
    for point in &mut wave {
        *point = (*point * scale).min(1.0);
    }
    wave
}

fn show_wave(app: &App, wave: &[f32]) {
    WAVE.with(|shown| {
        let mut shown = shown.borrow_mut();
        match shown.as_ref() {
            Some(points) if points.row_count() == wave.len() => {
                for (i, &value) in wave.iter().enumerate() {
                    if points.row_data(i) != Some(value) {
                        points.set_row_data(i, value);
                    }
                }
            }
            _ => {
                let points = Rc::new(VecModel::from(wave.to_vec()));
                app.set_looper_wave(points.clone().into());
                *shown = Some(points);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loop_details_read_like_the_page() {
        assert_eq!(signature(4), "4/4");
        assert_eq!(signature(6), "6/8");
        assert_eq!(time(0.0), "0:00.0");
        assert_eq!(time(75.36), "1:15.4");
    }

    #[test]
    fn quiet_loops_are_drawn_to_their_peak() {
        assert_eq!(scaled(vec![0.1, 0.05, 0.0]), vec![1.0, 0.5, 0.0]);
        // Near silence stays near flat
        assert_eq!(scaled(vec![0.001, 0.0]), vec![0.05, 0.0]);
        assert!(scaled(vec![]).is_empty());
    }
}
