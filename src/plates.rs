//! Pictures of OpenRig FX plates (signature plugins), drawn by
//! [`Plate::render`] once per plate and size and kept: the UI then draws a
//! single opaque image under its knobs, which is what the software renderer
//! likes best. The web remote's renderer draws plates flat, without one.

use std::cell::{Cell, RefCell};

use pedalboard::plugins::faceplate::{Faceplate, Plate};
use slint::{ComponentHandle, Image, Rgba8Pixel, SharedPixelBuffer};

use crate::{rgb, App, Features};

/// Pictures kept: the editor's plate at a few sizes and the rack's blocks
const KEPT: usize = 24;

type Key = (u64, u32, u32, u32, u32);

thread_local! {
    static CACHE: RefCell<Vec<(Key, Image)>> = const { RefCell::new(Vec::new()) };
    /// The plate of the block in the editor
    static EDITED: Cell<Option<Plate>> = const { Cell::new(None) };
}

/// The picture of `plate` at `width` x `height` physical pixels
pub fn image(plate: &Plate, width: u32, height: u32) -> Image {
    let key = (
        plate.seed,
        plate.color,
        plate.wear.to_bits() ^ (plate.finish as u32) << 31,
        width,
        height,
    );
    CACHE.with_borrow_mut(|cache| {
        if let Some(i) = cache.iter().position(|(k, _)| *k == key) {
            // Most recently used last
            let entry = cache.remove(i);
            let image = entry.1.clone();
            cache.push(entry);
            return image;
        }
        let pixels = plate.render(width, height);
        let buffer =
            SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&pixels, width.max(1), height.max(1));
        let image = Image::from_rgba8(buffer);
        if cache.len() >= KEPT {
            cache.remove(0);
        }
        cache.push((key, image.clone()));
        image
    })
}

/// Physical pixels of a logical length on the app's window
pub fn physical(app: &App, length: f32) -> u32 {
    (length * app.window().scale_factor()).round().max(1.0) as u32
}

/// Whether plates are drawn without their picture (the web remote)
pub fn flat(app: &App) -> bool {
    app.global::<Features>().get_flat()
}

/// The editor's plate, for the selected block's face
pub fn refresh_editor(app: &App, face: Option<&Faceplate>) {
    let plate = face.and_then(|f| f.plate);
    EDITED.set(plate);
    app.set_selected_face_name(face.map(|f| f.name.as_str()).unwrap_or_default().into());
    app.set_selected_tagline(face.map(|f| f.tagline.as_str()).unwrap_or_default().into());
    if let Some(plate) = plate {
        app.set_selected_plate_color(rgb(plate.color));
        app.set_selected_knob_style(plate.knob_style.as_str().into());
        app.set_selected_steel_knobs(plate.steel_caps());
    }
    draw_editor(app);
}

/// Draws the editor's plate at the face's current size
fn draw_editor(app: &App) {
    let (w, h) = (app.get_plate_width(), app.get_plate_height());
    let image = match EDITED.get() {
        Some(plate) if !flat(app) && w >= 1.0 && h >= 1.0 => {
            image(&plate, physical(app, w), physical(app, h))
        }
        _ => Image::default(),
    };
    app.set_selected_plate(image);
}

pub fn setup(app: &App) {
    let weak = app.as_weak();
    app.on_plate_sized(move || {
        if let Some(app) = weak.upgrade() {
            draw_editor(&app);
        }
    });
}
