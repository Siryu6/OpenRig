//! MIDI on the UI thread: the commands of `pedalboard::midi` (presets,
//! setlists, scenes, footswitches, looper, tap tempo, tuner), the controls
//! learned by the blocks, the clock, and Settings → MIDI.
//!
//! Learning, in the block editor: tap MIDI, then touch a control and move a
//! pedal or knob of the controller (in either order). The learned control
//! stays shown with its range (heel / toe), and its controller then plays
//! as usual so the range can be tried.

use std::cell::RefCell;
use std::sync::Arc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model};

use pedalboard::midi::{MidiAssign, MidiTarget, TOE};
use pedalboard::state::AppState;
#[cfg(feature = "midi")]
use pedalboard::{
    midi::{self, MidiAction, MidiMessage},
    state::PresetStep,
};

#[cfg(feature = "midi")]
use crate::{live_ui, show_status, EDITOR_VIEW};
use crate::{report, App};

/// The activity LED stays lit this long after a message
const ACTIVITY: Duration = Duration::from_millis(120);
/// No clock for this long: the tempo is back in the player's hands
const CLOCK_LOST: Duration = Duration::from_secs(1);
/// `midi-learn-target` when no control is touched
const NO_TARGET: i32 = -2;
/// `midi-learn-target` of the block's on/off switch
const BYPASS_TARGET: i32 = -1;

#[derive(Default)]
#[cfg_attr(not(feature = "midi"), allow(dead_code))]
struct Midi {
    /// Group of 128 presets chosen by CC 0
    group: usize,
    last_message: Option<Instant>,
    last_clock: Option<Instant>,
    /// Tempo of the running clock
    clock_bpm: Option<f32>,
    /// Learning: the controller moved, or the control touched, first
    learn_controller: Option<u8>,
    learn_target: Option<MidiTarget>,
    timer: Option<slint::Timer>,
    #[cfg(feature = "midi")]
    io: Option<crate::midi_io::MidiIo>,
}

thread_local! {
    static MIDI: RefCell<Midi> = RefCell::default();
}

pub fn setup(app: &App, state: &Arc<AppState>) {
    let settings = state.midi_settings();
    app.set_midi_channel(i32::from(settings.channel));
    app.set_midi_clock(settings.clock);

    let (weak, st) = (app.as_weak(), state.clone());
    app.on_midi_channel_changed(move |channel| {
        let Some(app) = weak.upgrade() else { return };
        st.set_midi_channel(channel.clamp(0, 16) as u8);
        app.set_midi_channel(i32::from(st.midi_settings().channel));
    });
    let (weak, st) = (app.as_weak(), state.clone());
    app.on_midi_clock_changed(move |on| {
        let Some(app) = weak.upgrade() else { return };
        st.set_midi_clock(on);
        app.set_midi_clock(on);
        if !on {
            app.set_tempo_clock(false);
        }
    });
    let weak = app.as_weak();
    app.on_midi_learn_toggled(move |on| {
        if let Some(app) = weak.upgrade() {
            start_learning(&app, on);
        }
    });
    let (weak, st) = (app.as_weak(), state.clone());
    app.on_midi_learn_touch(move |target| {
        if let Some(app) = weak.upgrade() {
            learn_touch(&app, &st, target);
        }
    });
    let (weak, st) = (app.as_weak(), state.clone());
    app.on_midi_learn_range(move |heel, toe| {
        let Some(app) = weak.upgrade() else { return };
        let Some(assign) = shown_assign(&app, &st) else {
            return;
        };
        let clamp = |v: i32| v.clamp(0, i32::from(TOE)) as u8;
        st.set_midi_range(assign.instance_id, assign.target, clamp(heel), clamp(toe));
        show_target(&app, &st, assign.target);
    });
    let (weak, st) = (app.as_weak(), state.clone());
    app.on_midi_learn_clear(move |target| {
        let Some(app) = weak.upgrade() else { return };
        let (Some(id), Some(target)) = (selected(&app), learn_target(target)) else {
            return;
        };
        st.clear_midi(id, target);
        crate::refresh_chain(&app, &st);
        reset_learning(&app, "Cleared. Touch another control, or Done.");
    });

    // Activity LED and clock loss
    let weak = app.as_weak();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(100),
        move || {
            let Some(app) = weak.upgrade() else { return };
            let now = Instant::now();
            MIDI.with(|m| {
                let mut m = m.borrow_mut();
                let old = |t: Option<Instant>, d| t.is_none_or(|t| now.duration_since(t) > d);
                if app.get_midi_activity() && old(m.last_message, ACTIVITY) {
                    app.set_midi_activity(false);
                }
                if m.clock_bpm.is_some() && old(m.last_clock, CLOCK_LOST) {
                    m.clock_bpm = None;
                    app.set_tempo_clock(false);
                }
            });
        },
    );
    MIDI.with(|m| m.borrow_mut().timer = Some(timer));

    #[cfg(feature = "midi")]
    {
        let (weak, st) = (app.as_weak(), state.clone());
        let send = move |event| {
            let (weak, st) = (weak.clone(), st.clone());
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(app) = weak.upgrade() {
                    on_event(&app, &st, event);
                }
            });
        };
        let st = state.clone();
        let forward = move |bytes: &[u8]| st.forward_midi(bytes);
        match crate::midi_io::start(send, forward) {
            Ok(io) => {
                app.set_midi_available(true);
                MIDI.with(|m| m.borrow_mut().io = Some(io));
            }
            Err(e) => {
                tracing::warn!("MIDI disabled: {:#}", e);
                app.set_midi_available(false);
                app.set_midi_devices(format!("MIDI unavailable: {e}").into());
            }
        }
    }
    #[cfg(not(feature = "midi"))]
    app.set_midi_available(false);
}

#[cfg(feature = "midi")]
fn on_event(app: &App, state: &AppState, event: crate::midi_io::MidiEvent) {
    use crate::midi_io::MidiEvent;
    match event {
        MidiEvent::Devices(names) => app.set_midi_devices(names.join(", ").into()),
        MidiEvent::Clock(bpm) => clock(app, state, bpm),
        MidiEvent::Message(message) => on_message(app, state, message),
    }
}

/// A clock is running (with its tempo when it changed): it sets the tempo,
/// including after a preset change brought its own
#[cfg(feature = "midi")]
fn clock(app: &App, state: &AppState, bpm: Option<f32>) {
    if !state.midi_settings().clock {
        return;
    }
    let bpm = MIDI.with(|m| {
        let mut m = m.borrow_mut();
        m.last_clock = Some(Instant::now());
        if bpm.is_some() {
            m.clock_bpm = bpm;
        }
        m.clock_bpm
    });
    app.set_tempo_clock(bpm.is_some());
    if let Some(bpm) = bpm {
        if state.tempo() != pedalboard::state::tempo::clamp_bpm(bpm) {
            live_ui::show_tempo(app, state, state.set_tempo(bpm));
        }
    }
}

#[cfg(feature = "midi")]
fn on_message(app: &App, state: &AppState, message: MidiMessage) {
    MIDI.with(|m| m.borrow_mut().last_message = Some(Instant::now()));
    app.set_midi_activity(true);
    let settings = state.midi_settings();

    // Learning in the editor: controllers go to the control being learned
    if app.get_midi_learning() && app.get_current_view() == EDITOR_VIEW {
        if let MidiMessage::Control {
            channel,
            controller,
            ..
        } = message
        {
            if settings.listens(channel) && learn_controller(app, state, controller) {
                return;
            }
        }
    }

    let Some(action) = midi::action(message, &settings) else {
        return;
    };
    match action {
        MidiAction::Program(n) => program(app, state, usize::from(n)),
        MidiAction::BankMsb(group) => MIDI.with(|m| m.borrow_mut().group = usize::from(group)),
        MidiAction::Bank(0) => {
            if state.current_scene_id().is_some() {
                state.close_setlist();
                crate::refresh_scenes(app, state);
            }
        }
        MidiAction::Bank(n) => match state.list_scenes().get(usize::from(n) - 1) {
            Some(setlist) => {
                if state.current_scene_id().as_ref() != Some(&setlist.id) {
                    report(app, state.load_scene(&setlist.id));
                    crate::refresh_scenes(app, state);
                }
            }
            None => show_status(app, format!("No setlist {}", n).into()),
        },
        MidiAction::Footswitch(slot) => live_ui::action(app, state, slot),
        MidiAction::Looper(action) => {
            use pedalboard::midi::LooperAction;
            pedalboard::audio::Looper::control(&state.looper, |looper| match action {
                LooperAction::Record => looper.record(),
                LooperAction::Overdub => looper.overdub(),
                LooperAction::Play => looper.play(),
                LooperAction::Stop => looper.stop(),
                LooperAction::Undo => looper.undo(),
            })
        }
        MidiAction::Tap => live_ui::tap(app, state),
        MidiAction::Tuner => live_ui::toggle_tuner(app),
        MidiAction::Scene(slot) => live_ui::select_scene(app, state, slot),
        MidiAction::SceneStep(step) => live_ui::step_scene(app, state, step),
        MidiAction::Mode(mode) => live_ui::set_mode(app, state, mode),
        MidiAction::ModeStep(step) => live_ui::step_mode(app, state, step),
        MidiAction::PresetStep(step) => match state.preset_step(step) {
            Some(PresetStep::Slot(slot)) => crate::setlist_slot(app, state, slot),
            Some(PresetStep::Preset(id)) => crate::load_preset(app, state, &id),
            None => {}
        },
        MidiAction::Controller { controller, value } => controlled(app, state, controller, value),
    }
}

/// Program Change: a slot of the setlist, or a preset of the list. The
/// preset already playing is not reloaded (a controller resending its PC
/// would restart the tails).
#[cfg(feature = "midi")]
fn program(app: &App, state: &AppState, n: usize) {
    if state.current_scene_id().is_some() {
        if state.active_setlist_slot() != Some(n) {
            crate::setlist_slot(app, state, n);
        }
        return;
    }
    let index = MIDI.with(|m| m.borrow().group) * 128 + n;
    match state.list_presets().get(index) {
        Some(preset) if state.current_preset_id().as_ref() == Some(&preset.id) => {}
        Some(preset) => crate::load_preset(app, state, &preset.id),
        None => show_status(app, format!("No preset {}", index + 1).into()),
    }
}

/// A learned controller moved: show what it changed
#[cfg(feature = "midi")]
fn controlled(app: &App, state: &AppState, controller: u8, value: u8) {
    let changed = state.midi_controller(controller, value);
    if changed.is_empty() {
        return;
    }
    let chain = state.chain();
    let selected = app.get_selected_instance_id();
    let mut bypass = false;
    for (id, target) in changed {
        match target {
            MidiTarget::Bypass => {
                crate::show_bypass(app, id as i32, chain.is_enabled(id) == Some(false));
                bypass = true;
            }
            MidiTarget::Parameter(index) if id as i32 == selected => {
                let Some(value) = chain.params.get(&id).and_then(|p| p.get(index)) else {
                    continue;
                };
                // In place, so the knob moves without rebuilding the page
                let rows = app.get_selected_parameters();
                for row in 0..rows.row_count() {
                    if let Some(mut p) = rows.row_data(row) {
                        if p.index == index as i32 {
                            p.value = *value;
                            rows.set_row_data(row, p);
                        }
                    }
                }
            }
            MidiTarget::Parameter(_) => {}
        }
    }
    if bypass {
        live_ui::refresh(app, state);
    }
}

fn selected(app: &App) -> Option<usize> {
    usize::try_from(app.get_selected_instance_id()).ok()
}

fn learn_target(target: i32) -> Option<MidiTarget> {
    match target {
        BYPASS_TARGET => Some(MidiTarget::Bypass),
        i => usize::try_from(i).ok().map(MidiTarget::Parameter),
    }
}

fn start_learning(app: &App, on: bool) {
    app.set_midi_learning(on);
    reset_learning(
        app,
        if on {
            "Touch a control, then move a pedal or knob of your controller."
        } else {
            ""
        },
    );
}

fn reset_learning(app: &App, message: &str) {
    MIDI.with(|m| {
        let mut m = m.borrow_mut();
        m.learn_controller = None;
        m.learn_target = None;
    });
    app.set_midi_learn_target(NO_TARGET);
    app.set_midi_learn_cc(-1);
    app.set_midi_learn_assigned(false);
    app.set_midi_learn_message(message.into());
}

/// A control of the editor was touched while learning
fn learn_touch(app: &App, state: &AppState, target: i32) {
    let Some(learned) = learn_target(target) else {
        return;
    };
    match MIDI.with(|m| m.borrow().learn_controller) {
        Some(controller) => assign(app, state, controller, learned),
        None => {
            MIDI.with(|m| m.borrow_mut().learn_target = Some(learned));
            show_target(app, state, learned);
            app.set_midi_learn_message(
                format!(
                    "{}: now move a pedal or knob of your controller.",
                    target_name(app, learned)
                )
                .into(),
            );
        }
    }
}

/// A controller moved while learning; false when it is the one of the
/// control just learned, which then plays as usual (to try its range)
#[cfg(feature = "midi")]
fn learn_controller(app: &App, state: &AppState, controller: u8) -> bool {
    let waiting = MIDI.with(|m| m.borrow().learn_target);
    if waiting.is_none() && shown_assign(app, state).is_some_and(|a| a.controller == controller) {
        return false;
    }
    if midi::is_global(controller) {
        app.set_midi_learn_message(
            format!(
                "CC {} is used for {}: pick another control on your controller.",
                controller,
                midi::global_name(controller)
            )
            .into(),
        );
        return true;
    }
    match waiting {
        Some(target) => assign(app, state, controller, target),
        None => {
            MIDI.with(|m| m.borrow_mut().learn_controller = Some(controller));
            app.set_midi_learn_target(NO_TARGET);
            app.set_midi_learn_assigned(false);
            app.set_midi_learn_cc(i32::from(controller));
            app.set_midi_learn_message(
                format!("CC {}: now touch the control it drives.", controller).into(),
            );
        }
    }
    true
}

fn assign(app: &App, state: &AppState, controller: u8, target: MidiTarget) {
    let Some(id) = selected(app) else { return };
    let name = target_name(app, target);
    if report(app, state.learn_midi(controller, id, target)).is_some() {
        crate::refresh_chain(app, state);
        // The learned control stays shown, with its range to set
        reset_learning(
            app,
            &format!(
                "CC {} → {}. Set its range, touch another control, or Done.",
                controller, name
            ),
        );
        show_target(app, state, target);
    }
}

/// Show a touched control in the banner, with its range when it has a
/// controller
fn show_target(app: &App, state: &AppState, target: MidiTarget) {
    app.set_midi_learn_target(match target {
        MidiTarget::Bypass => BYPASS_TARGET,
        MidiTarget::Parameter(index) => index as i32,
    });
    let assign = selected(app).and_then(|id| state.midi_assign(id, target));
    app.set_midi_learn_assigned(assign.is_some());
    if let Some(assign) = assign {
        app.set_midi_learn_heel(i32::from(assign.heel));
        app.set_midi_learn_toe(i32::from(assign.toe));
    }
}

/// Assignment of the control shown in the banner
#[cfg_attr(not(feature = "midi"), allow(dead_code))]
fn shown_assign(app: &App, state: &AppState) -> Option<MidiAssign> {
    let target = learn_target(app.get_midi_learn_target())?;
    state.midi_assign(selected(app)?, target)
}

fn target_name(app: &App, target: MidiTarget) -> String {
    match target {
        MidiTarget::Bypass => "On / Off".to_string(),
        MidiTarget::Parameter(index) => app
            .get_selected_parameters()
            .iter()
            .find(|p| p.index == index as i32)
            .map(|p| p.name.to_string())
            .unwrap_or_else(|| format!("Parameter {}", index + 1)),
    }
}
