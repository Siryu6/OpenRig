//! Live page and footswitches. In the mode chosen on the page, switch n
//! recalls scene n of the preset, turns a block on or off, or loads preset n
//! of the setlist. The last switch can tap the tempo instead; held, it opens
//! the tuner, and any switch closes it again.
//!
//! Switches act when pressed, never on release: that is when the foot lands
//! on the beat. A momentary stomp switch also acts on release: what its
//! press turned on, the release turns off.

use std::cell::RefCell;
use std::sync::Arc;
use std::time::Duration;

use slint::ComponentHandle;

use pedalboard::audio::ChainNode;
use pedalboard::midi::{MidiTarget, Step};
use pedalboard::state::stomp::Held;
use pedalboard::state::{AppState, EffectChainState, FootswitchMode, SCENE_COUNT};
use pedalboard::ui::slint_bridge as views;

use crate::{model, report, rgb, App, LiveTile, MenuItem, View};

/// How long the tap switch is held to open the tuner
const HOLD: Duration = Duration::from_millis(800);
/// The accent colour (Theme.accent), for the scene LEDs
const ACCENT: u32 = 0xf0a43c;

/// Switches held down, presses counted (a hold belongs to one press)
#[derive(Default)]
struct Switches {
    down: u32,
    presses: u64,
    /// View to go back to when the tuner was opened from a switch
    tuner_return: Option<View>,
    /// Momentary stomp switches held down (by a footswitch, a tile, MIDI or
    /// the encoder), with what their release undoes
    held: [Option<Held>; SCENE_COUNT],
}

thread_local! {
    static SWITCHES: RefCell<Switches> = RefCell::default();
}

pub fn setup(app: &App, state: &Arc<AppState>) {
    let wire = |f: fn(&App, &AppState)| {
        let (weak, state) = (app.as_weak(), state.clone());
        move || {
            if let Some(app) = weak.upgrade() {
                f(&app, &state);
            }
        }
    };
    app.on_tap_tempo(wire(tap));

    let (weak, st) = (app.as_weak(), state.clone());
    app.on_live_mode_changed(move |mode| {
        let Some(app) = weak.upgrade() else { return };
        match FootswitchMode::from_name(&mode) {
            Some(mode) => set_mode(&app, &st, mode),
            None => refresh(&app, &st),
        }
    });
    let (weak, st) = (app.as_weak(), state.clone());
    app.on_live_tile_pressed(move |slot| {
        if let (Some(app), Ok(slot)) = (weak.upgrade(), usize::try_from(slot)) {
            action(&app, &st, slot);
        }
    });
    let (weak, st) = (app.as_weak(), state.clone());
    app.on_live_tile_released(move |slot| {
        if let (Some(app), Ok(slot)) = (weak.upgrade(), usize::try_from(slot)) {
            release(&app, &st, slot);
        }
    });
    let (weak, st) = (app.as_weak(), state.clone());
    app.on_set_tempo(move |bpm| {
        if let Some(app) = weak.upgrade() {
            show_tempo(&app, &st, st.set_tempo(bpm));
        }
    });
    let (weak, st) = (app.as_weak(), state.clone());
    app.on_rename_scene(move |slot, name| {
        let Some(app) = weak.upgrade() else { return };
        if let Ok(slot) = usize::try_from(slot) {
            report(&app, st.rename_scene(slot, &name));
        }
        refresh(&app, &st);
    });
    let (weak, st) = (app.as_weak(), state.clone());
    app.on_assign_stomp(move |slot, choice| {
        let Some(app) = weak.upgrade() else { return };
        if let Ok(slot) = usize::try_from(slot) {
            release(&app, &st, slot);
            // "id" (the block on and off) or "id:symbol" (a switch of it)
            let (id, param) = match choice.split_once(':') {
                Some((id, symbol)) => (id, Some(symbol)),
                None => (choice.as_str(), None),
            };
            report(&app, st.assign_stomp(slot, id.parse().ok(), param));
        }
        refresh(&app, &st);
    });
    let (weak, st) = (app.as_weak(), state.clone());
    app.on_stomp_momentary(move |slot, momentary| {
        let Some(app) = weak.upgrade() else { return };
        if let Ok(slot) = usize::try_from(slot) {
            release(&app, &st, slot);
            report(&app, st.set_stomp_momentary(slot, momentary));
        }
        refresh(&app, &st);
    });
    let (weak, st) = (app.as_weak(), state.clone());
    // Keys 1-8: as many as the switches set in Settings → Hardware
    app.on_footswitch_down(move |slot| {
        if let (Some(app), Ok(slot)) = (weak.upgrade(), usize::try_from(slot)) {
            if slot < usize::try_from(app.get_footswitch_count()).unwrap_or(0) {
                footswitch_down(&app, &st, slot);
            }
        }
    });
    let (weak, st) = (app.as_weak(), state.clone());
    app.on_footswitch_up(move |slot| {
        if let (Some(app), Ok(slot)) = (weak.upgrade(), usize::try_from(slot)) {
            footswitch_up(&app, &st, slot);
        }
    });
}

/// Switch that taps the tempo, if any: the last one
fn tap_switch(app: &App, state: &AppState) -> Option<usize> {
    let count = usize::try_from(app.get_footswitch_count()).unwrap_or(0);
    (state.live_settings().tap_switch && count >= 2).then(|| count - 1)
}

/// A footswitch went down
pub fn footswitch_down(app: &App, state: &Arc<AppState>, slot: usize) {
    let Some(bit) = 1u32.checked_shl(slot as u32) else {
        return;
    };
    let press = SWITCHES.with(|s| {
        let mut s = s.borrow_mut();
        if s.down & bit != 0 {
            // Key repeat, or a bounce the debounce let through
            return None;
        }
        s.down |= bit;
        s.presses += 1;
        Some(s.presses)
    });
    let Some(press) = press else { return };
    show_down(app);

    // The tuner opened from a switch: any switch closes it
    if let Some(view) = SWITCHES.with(|s| s.borrow_mut().tuner_return.take()) {
        if app.get_current_view() == View::Tuner {
            app.set_current_view(view);
            return;
        }
    }

    if tap_switch(app, state) == Some(slot) {
        tap(app, state);
        let weak = app.as_weak();
        slint::Timer::single_shot(HOLD, move || {
            let held = SWITCHES.with(|s| {
                let s = s.borrow();
                s.down & bit != 0 && s.presses == press
            });
            let Some(app) = weak.upgrade() else { return };
            if held && app.get_current_view() != View::Tuner {
                let back = app.get_current_view();
                SWITCHES.with(|s| s.borrow_mut().tuner_return = Some(back));
                app.set_current_view(View::Tuner);
            }
        });
        return;
    }
    action(app, state, slot);
}

/// A footswitch came back up
pub fn footswitch_up(app: &App, state: &AppState, slot: usize) {
    if let Some(bit) = 1u32.checked_shl(slot as u32) {
        SWITCHES.with(|s| s.borrow_mut().down &= !bit);
        show_down(app);
        release(app, state, slot);
    }
}

/// Release the footswitches held (the hardware changed: a switch moved or
/// removed never reports its release)
pub fn release_footswitches(app: &App, state: &AppState) {
    SWITCHES.with(|s| s.borrow_mut().down = 0);
    show_down(app);
    release_all(app, state);
}

/// The switches held, for the LEDs of Settings → Hardware
fn show_down(app: &App) {
    let down = SWITCHES.with(|s| s.borrow().down);
    app.set_footswitch_down_mask(down as i32);
}

pub(crate) fn tap(app: &App, state: &AppState) {
    if let Some(bpm) = state.tap_tempo() {
        show_tempo(app, state, bpm);
    }
}

pub(crate) fn show_tempo(app: &App, state: &AppState, bpm: f32) {
    app.set_tempo(bpm);
    // Blocks following the tempo show the new value in the editor
    if app.get_current_view() == View::Editor {
        crate::refresh_chain(app, state);
    }
}

/// What switch (or tile) `slot` does in the current mode
pub(crate) fn action(app: &App, state: &AppState, slot: usize) {
    if slot >= SCENE_COUNT {
        return;
    }
    match state.live_settings().mode {
        FootswitchMode::Scenes => select_scene(app, state, slot),
        FootswitchMode::Stomp => press_stomp(app, state, slot),
        FootswitchMode::Setlist => crate::setlist_slot(app, state, slot),
    }
}

/// CC 49–56: a momentary stomp switch is down while the value is high, up
/// when it goes low; any value presses the other switches
pub(crate) fn midi_switch(app: &App, state: &AppState, slot: usize, high: bool) {
    let momentary = state.live_settings().mode == FootswitchMode::Stomp
        && state.stomp(slot).is_some_and(|(_, mode)| mode.momentary);
    if !high && (momentary || is_held(slot)) {
        release(app, state, slot);
    } else if high || !momentary {
        action(app, state, slot);
    }
}

fn is_held(slot: usize) -> bool {
    SWITCHES.with(|s| s.borrow().held.get(slot).is_some_and(Option::is_some))
}

/// Press stomp switch `slot`. Pressed again while held (the encoder, which
/// has no release), it is released.
fn press_stomp(app: &App, state: &AppState, slot: usize) {
    if is_held(slot) {
        release(app, state, slot);
        return;
    }
    let Some(pressed) = report(app, state.press_stomp(slot)) else {
        return;
    };
    SWITCHES.with(|s| {
        if let Some(held) = s.borrow_mut().held.get_mut(slot) {
            *held = pressed.held;
        }
    });
    show_stomp(app, state, pressed.instance_id, pressed.target);
}

/// Switch or tile `slot` released: a momentary stomp switch held down puts
/// back what its press changed
pub(crate) fn release(app: &App, state: &AppState, slot: usize) {
    let held = SWITCHES.with(|s| s.borrow_mut().held.get_mut(slot).and_then(Option::take));
    if let Some(held) = held {
        if let Some(Some(held)) = report(app, state.release_stomp(held)) {
            show_stomp(app, state, held.instance_id, held.target);
        }
    }
}

/// Release every momentary stomp switch held down
fn release_all(app: &App, state: &AppState) {
    for slot in 0..SCENE_COUNT {
        release(app, state, slot);
    }
}

fn show_stomp(app: &App, state: &AppState, instance_id: usize, target: MidiTarget) {
    crate::show_controlled(app, state, [(instance_id, target)]);
    refresh(app, state);
}

/// Recall scene `slot` of the preset
pub(crate) fn select_scene(app: &App, state: &AppState, slot: usize) {
    if report(app, state.select_scene(slot)).is_some() {
        crate::refresh_chain(app, state);
    }
}

/// Scene after or before the current one, among those in use
#[cfg_attr(not(feature = "midi"), allow(dead_code))]
pub(crate) fn step_scene(app: &App, state: &AppState, step: Step) {
    let chain = state.chain();
    let used: Vec<usize> = (0..SCENE_COUNT).filter(|&s| chain.scene_used(s)).collect();
    let Some(at) = used.iter().position(|&s| s == chain.scene) else {
        return;
    };
    let next = match step {
        Step::Next => (at + 1) % used.len(),
        Step::Previous => (at + used.len() - 1) % used.len(),
    };
    if used[next] != chain.scene {
        select_scene(app, state, used[next]);
    }
}

/// Open the tuner, or close it and go back to the page shown before
#[cfg_attr(not(feature = "midi"), allow(dead_code))]
pub(crate) fn toggle_tuner(app: &App) {
    if app.get_current_view() == View::Tuner {
        let back = SWITCHES.with(|s| s.borrow_mut().tuner_return.take());
        app.set_current_view(back.unwrap_or(View::Live));
    } else {
        let back = app.get_current_view();
        SWITCHES.with(|s| s.borrow_mut().tuner_return = Some(back));
        app.set_current_view(View::Tuner);
    }
}

/// Footswitch mode, in the order of the Live page
#[cfg_attr(not(feature = "midi"), allow(dead_code))]
const MODES: [FootswitchMode; 3] = [
    FootswitchMode::Scenes,
    FootswitchMode::Stomp,
    FootswitchMode::Setlist,
];

pub(crate) fn set_mode(app: &App, state: &AppState, mode: FootswitchMode) {
    // What a held switch turned on does not outlive its mode
    release_all(app, state);
    state.set_footswitch_mode(mode);
    refresh(app, state);
}

#[cfg_attr(not(feature = "midi"), allow(dead_code))]
pub(crate) fn step_mode(app: &App, state: &AppState, step: Step) {
    let at = MODES
        .iter()
        .position(|&m| m == state.live_settings().mode)
        .unwrap_or(0);
    let next = match step {
        Step::Next => (at + 1) % MODES.len(),
        Step::Previous => (at + MODES.len() - 1) % MODES.len(),
    };
    set_mode(app, state, MODES[next]);
}

/// What a stomp switch can play: each block on and off, and each switch
/// (toggle parameter) of its plugin, as "Block · Hold"
fn stomp_choices(state: &AppState, chain: &EffectChainState) -> Vec<MenuItem> {
    let item = |id: String, label: String, icon: &str| MenuItem {
        id: id.into(),
        label: label.into(),
        icon: icon.into(),
        enabled: true,
        badge: false,
    };
    let scanner = state.lv2_host.scanner();
    let mut choices = Vec::new();
    for node in chain.plugins() {
        let ChainNode::Plugin {
            instance_id, uri, ..
        } = node
        else {
            continue;
        };
        let name = crate::block_name(state, chain, *instance_id, uri);
        choices.push(item(instance_id.to_string(), name.clone(), "power"));
        let Some(info) = scanner.find_by_uri(uri) else {
            continue;
        };
        for port in info.control_inputs().filter(|p| p.toggled) {
            choices.push(item(
                format!("{instance_id}:{}", port.symbol),
                format!("{name} · {}", port.name),
                "footswitch",
            ));
        }
    }
    choices
}

/// Show the switches, scenes and tempo
pub fn refresh(app: &App, state: &AppState) {
    let chain = state.chain();
    let live = state.live_settings();
    let count = usize::try_from(app.get_footswitch_count()).unwrap_or(0);
    let tap = tap_switch(app, state);
    let switch = |slot: usize| {
        if slot < count && Some(slot) != tap {
            slot as i32 + 1
        } else {
            0
        }
    };

    // The picker entry of a stomp switch (see stomp_choices)
    let choice = |slot: usize| -> slint::SharedString {
        match chain.stomps.get(slot) {
            Some(stomp) => match (stomp.block, &stomp.mode.param) {
                (Some(id), Some(symbol)) => format!("{id}:{symbol}").into(),
                (Some(id), None) => id.to_string().into(),
                (None, _) => "".into(),
            },
            None => "".into(),
        }
    };
    let tiles: Vec<LiveTile> = match live.mode {
        FootswitchMode::Scenes => (0..SCENE_COUNT)
            .map(|slot| {
                let used = chain.scene_used(slot);
                LiveTile {
                    title: chain.scene_name(slot).into(),
                    subtitle: if used { "" } else { "Empty" }.into(),
                    active: slot == chain.scene,
                    empty: !used,
                    color: rgb(ACCENT),
                    switch: switch(slot),
                    momentary: false,
                    choice: "".into(),
                }
            })
            .collect(),
        FootswitchMode::Stomp => (0..SCENE_COUNT)
            .map(|slot| {
                let assigned = chain.stomps.get(slot).is_some_and(|s| s.block.is_some());
                let block = chain.stomp(slot).and_then(|(id, mode)| {
                    let uri = crate::block_uri(&chain, id)?;
                    Some((id, uri, mode))
                });
                match block {
                    Some((id, uri, mode)) => {
                        let enabled = chain.is_enabled(id) == Some(true);
                        // The plugin switch it plays: its name, and on or off
                        let param = mode.param.as_deref().and_then(|symbol| {
                            let (index, port) = state.stomp_switch(id, symbol).ok()?;
                            let value = chain.params.get(&id)?.get(index).copied()?;
                            Some((port.name, value > (port.min + port.max) / 2.0))
                        });
                        let mut subtitle: Vec<&str> = Vec::new();
                        match &param {
                            Some((name, _)) => subtitle.push(name),
                            None if assigned && !mode.momentary => subtitle.push("Assigned"),
                            None => {}
                        }
                        if mode.momentary {
                            subtitle.push("Momentary");
                        }
                        LiveTile {
                            title: crate::block_name(state, &chain, id, &uri).into(),
                            subtitle: subtitle.join(" · ").into(),
                            active: enabled && param.as_ref().is_none_or(|(_, on)| *on),
                            empty: false,
                            color: rgb(crate::block_tint(state, &chain, id, &uri)),
                            switch: switch(slot),
                            momentary: mode.momentary,
                            choice: choice(slot),
                        }
                    }
                    None => LiveTile {
                        title: "No block".into(),
                        subtitle: "".into(),
                        active: false,
                        empty: true,
                        color: rgb(ACCENT),
                        switch: switch(slot),
                        momentary: chain.stomps.get(slot).is_some_and(|s| s.mode.momentary),
                        choice: choice(slot),
                    },
                }
            })
            .collect(),
        FootswitchMode::Setlist => views::scene_slot_views(state)
            .into_iter()
            .take(SCENE_COUNT)
            .map(|s| LiveTile {
                title: if s.is_empty {
                    "Empty".to_string()
                } else {
                    s.preset_name
                }
                .into(),
                subtitle: "".into(),
                active: s.active,
                empty: s.is_empty,
                color: rgb(ACCENT),
                switch: switch(s.slot),
                momentary: false,
                choice: "".into(),
            })
            .collect(),
    };

    let choices = stomp_choices(state, &chain);

    app.set_live_mode(live.mode.name().into());
    app.set_live_tiles(model(tiles));
    app.set_live_scene_name(chain.scene_name(chain.scene).into());
    app.set_live_scene_index(chain.scene as i32);
    app.set_tempo(chain.bpm);
    app.set_tap_switch_label(
        tap.map(|slot| format!("FS {}", slot + 1))
            .unwrap_or_default()
            .into(),
    );
    app.set_tap_switch(live.tap_switch);
    app.set_stomp_choices(model(choices));
}
