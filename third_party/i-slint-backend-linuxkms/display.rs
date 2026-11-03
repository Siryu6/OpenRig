// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0

// cSpell: ignore gbmdisplay swdisplay
use i_slint_core::api::PhysicalSize;

#[allow(unused)]
pub trait Presenter {
    // Present updated front-buffer to the screen
    fn present(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
}

#[cfg(any(feature = "renderer-skia-opengl", feature = "renderer-femtovg"))]
pub mod gbmdisplay;
#[cfg(any(enable_skia, feature = "renderer-software"))]
pub mod swdisplay;

/// This enum describes the way the output is supposed to be rotated to simulate
/// a screen rotation. This is implemented entirely inside the actual renderer.
#[non_exhaustive]
#[derive(Default, Copy, Clone, Eq, PartialEq, Debug)]
pub enum RenderingRotation {
    /// No rotation
    #[default]
    NoRotation,
    /// Rotate 90° to the right
    Rotate90,
    /// 180° rotation (upside-down)
    Rotate180,
    /// Rotate 90° to the left
    Rotate270,
}

impl TryFrom<&str> for RenderingRotation {
    type Error = String;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let angle: usize = value.parse().map_err(|_| {
            format!("Invalid value for rotation. Must be unsigned integral, found {value}")
        })?;
        Ok(match angle {
            0 => Self::NoRotation,
            90 => Self::Rotate90,
            180 => Self::Rotate180,
            270 => Self::Rotate270,
            _ => {
                return Err(
                    "Invalid value for rotation. Must be one of 0, 90, 180, or 270".to_string()
                );
            }
        })
    }
}

impl RenderingRotation {
    pub fn screen_size_to_rotated_window_size(&self, screen_size: PhysicalSize) -> PhysicalSize {
        match self {
            RenderingRotation::NoRotation | RenderingRotation::Rotate180 => screen_size,
            RenderingRotation::Rotate90 | RenderingRotation::Rotate270 => {
                PhysicalSize::new(screen_size.height, screen_size.width)
            }
        }
    }

    pub fn degrees(&self) -> f32 {
        match self {
            RenderingRotation::NoRotation => 0.,
            RenderingRotation::Rotate90 => 90.,
            RenderingRotation::Rotate180 => 180.,
            RenderingRotation::Rotate270 => 270.,
        }
    }

    /// Maps a position on the screen (the panel as the display reports it,
    /// where touchscreens and tablets report their positions too) to the
    /// rotated window. `window_size` is the size of the rotated window; the
    /// position is given by `transform`, called with the screen's width and
    /// height in the same units (like libinput's `x_transformed`).
    #[allow(unused)]
    pub fn screen_to_window_position(
        &self,
        window_size: (f32, f32),
        transform: impl FnOnce(f32, f32) -> (f32, f32),
    ) -> (f32, f32) {
        let (width, height) = match self {
            RenderingRotation::NoRotation | RenderingRotation::Rotate180 => window_size,
            RenderingRotation::Rotate90 | RenderingRotation::Rotate270 => {
                (window_size.1, window_size.0)
            }
        };
        let (x, y) = transform(width, height);
        // The inverse of the renderer's transform: rotate by `degrees()` after
        // translating by `translation_after_rotation`
        match self {
            RenderingRotation::NoRotation => (x, y),
            RenderingRotation::Rotate90 => (y, width - x),
            RenderingRotation::Rotate180 => (width - x, height - y),
            RenderingRotation::Rotate270 => (height - y, x),
        }
    }

    #[allow(unused)]
    pub fn translation_after_rotation(&self, screen_size: PhysicalSize) -> (f32, f32) {
        match self {
            RenderingRotation::NoRotation => (0., 0.),
            RenderingRotation::Rotate90 => (0., -(screen_size.width as f32)),
            RenderingRotation::Rotate180 => {
                (-(screen_size.width as f32), -(screen_size.height as f32))
            }
            RenderingRotation::Rotate270 => (-(screen_size.height as f32), 0.),
        }
    }
}

#[cfg(any(enable_skia, feature = "renderer-software"))]
pub(crate) mod noop_presenter {
    use std::sync::Arc;

    // Used when the underlying renderer/display takes care of the presentation to the display
    // and (hopefully) implements vsync.
    pub(crate) struct NoopPresenter {}

    impl NoopPresenter {
        pub(crate) fn new() -> Arc<Self> {
            Arc::new(Self {})
        }
    }

    impl crate::display::Presenter for NoopPresenter {
        fn present(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Where the renderer draws a window position: rotated by `degrees()`
    /// (clockwise, y down) after the translation
    fn window_to_screen(rotation: RenderingRotation, screen: PhysicalSize, p: (f32, f32)) -> (f32, f32) {
        let (tx, ty) = rotation.translation_after_rotation(screen);
        let (x, y) = (p.0 + tx, p.1 + ty);
        let (sin, cos) = rotation.degrees().to_radians().sin_cos();
        (x * cos - y * sin, x * sin + y * cos)
    }

    #[test]
    fn screen_positions_map_back_to_the_window() {
        let screen = PhysicalSize::new(720, 1280);
        for rotation in [
            RenderingRotation::NoRotation,
            RenderingRotation::Rotate90,
            RenderingRotation::Rotate180,
            RenderingRotation::Rotate270,
        ] {
            let window = rotation.screen_size_to_rotated_window_size(screen);
            let window = (window.width as f32, window.height as f32);
            for p in [(0., 0.), (100., 30.), (window.0, window.1), (window.0 / 3., window.1)] {
                let s = window_to_screen(rotation, screen, p);
                let back = rotation.screen_to_window_position(window, |w, h| {
                    assert_eq!((w, h), (720., 1280.));
                    s
                });
                assert!(
                    (back.0 - p.0).abs() < 0.01 && (back.1 - p.1).abs() < 0.01,
                    "{rotation:?}: {p:?} -> {s:?} -> {back:?}"
                );
            }
        }
    }
}
