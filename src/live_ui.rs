//! Live page and footswitches. In the mode chosen on the page, switch n
//! recalls scene n of the preset, turns a block on or off, or loads preset n
//! of the setlist. The last switch can tap the tempo instead; held, it opens
//! the tuner, and any switch closes it again.
//!
//! Switches act when pressed, never on release: that is when the foot lands
//! on the beat.

use std::cell::RefCell;
use std::sync::Arc;
use std::time::Duration;

use slint::ComponentHandle;

use pedalboard::midi::Step;
use pedalboard::state::{AppState, FootswitchMode, SCENE_COUNT};
use pedalboard::ui::slint_bridge as views;

use crate::{model, report, rgb, App, LiveTile, MenuItem, LIVE_VIEW, TUNER_VIEW};

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
    tuner_return: Option<i32>,
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
    app.on_assign_stomp(move |slot, id| {
        let Some(app) = weak.upgrade() else { return };
        if let Ok(slot) = usize::try_from(slot) {
            report(&app, st.assign_stomp(slot, id.parse().ok()));
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
    let weak = app.as_weak();
    app.on_footswitch_up(move |slot| {
        if let (Some(app), Ok(slot)) = (weak.upgrade(), usize::try_from(slot)) {
            footswitch_up(&app, slot);
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
        if app.get_current_view() == TUNER_VIEW {
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
            if held && app.get_current_view() != TUNER_VIEW {
                let back = app.get_current_view();
                SWITCHES.with(|s| s.borrow_mut().tuner_return = Some(back));
                app.set_current_view(TUNER_VIEW);
            }
        });
        return;
    }
    action(app, state, slot);
}

/// A footswitch came back up
pub fn footswitch_up(app: &App, slot: usize) {
    if let Some(bit) = 1u32.checked_shl(slot as u32) {
        SWITCHES.with(|s| s.borrow_mut().down &= !bit);
        show_down(app);
    }
}

/// Forget the footswitches held (the hardware changed)
pub fn release_footswitches(app: &App) {
    SWITCHES.with(|s| s.borrow_mut().down = 0);
    show_down(app);
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
    if app.get_current_view() == 1 {
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
        FootswitchMode::Stomp => {
            if let Some((id, bypassed)) = report(app, state.toggle_stomp(slot)) {
                crate::show_bypass(app, id as i32, bypassed);
                refresh(app, state);
            }
        }
        FootswitchMode::Setlist => crate::setlist_slot(app, state, slot),
    }
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
    if app.get_current_view() == TUNER_VIEW {
        let back = SWITCHES.with(|s| s.borrow_mut().tuner_return.take());
        app.set_current_view(back.unwrap_or(LIVE_VIEW));
    } else {
        let back = app.get_current_view();
        SWITCHES.with(|s| s.borrow_mut().tuner_return = Some(back));
        app.set_current_view(TUNER_VIEW);
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
                }
            })
            .collect(),
        FootswitchMode::Stomp => (0..SCENE_COUNT)
            .map(|slot| {
                let assigned = matches!(chain.stomps.get(slot), Some(Some(_)));
                let block = chain.stomp_target(slot).and_then(|id| {
                    let uri = crate::block_uri(&chain, id)?;
                    Some((id, uri))
                });
                match block {
                    Some((id, uri)) => LiveTile {
                        title: crate::block_name(state, &chain, id, &uri).into(),
                        subtitle: if assigned { "Assigned" } else { "" }.into(),
                        active: chain.is_enabled(id) == Some(true),
                        empty: false,
                        color: rgb(crate::block_tint(state, &chain, id, &uri)),
                        switch: switch(slot),
                    },
                    None => LiveTile {
                        title: "No block".into(),
                        subtitle: "".into(),
                        active: false,
                        empty: true,
                        color: rgb(ACCENT),
                        switch: switch(slot),
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
            })
            .collect(),
    };

    let choices: Vec<MenuItem> = chain
        .plugins()
        .into_iter()
        .filter_map(|n| match n {
            pedalboard::audio::ChainNode::Plugin {
                instance_id, uri, ..
            } => Some(MenuItem {
                id: instance_id.to_string().into(),
                label: crate::block_name(state, &chain, *instance_id, uri).into(),
                icon: "footswitch".into(),
                enabled: true,
            }),
            _ => None,
        })
        .collect();

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
