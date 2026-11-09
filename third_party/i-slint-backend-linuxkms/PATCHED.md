# i-slint-backend-linuxkms 1.18.1, patched

Copy of Slint's [linuxkms backend](https://crates.io/crates/i-slint-backend-linuxkms)
1.18.1 (licenses in LICENSES), used through `[patch.crates-io]` in the
pedalboard's Cargo.toml.

Change, in `display/gbmdisplay.rs`: when adding the GPU-rendered buffer as a
framebuffer with its modifier fails and that modifier is LINEAR, try again
without any (the buffer is presented with no modifier, linear being the
implicit layout). Drivers such as virtio-gpu (QEMU) and bochs take no
modifiers at all, so the UI never showed there ("Error adding gbm buffer as
framebuffer: Invalid argument"). Still present in 1.18.1; drop this copy
once a fixed release exists, and keep its version equal to Slint's.

Change, in `calloop_backend/input.rs` (with `RenderingRotation::screen_to_window_position`
in `display.rs` and `FullscreenWindowAdapter::rotation`): touchscreen and
tablet positions turn with `SLINT_KMS_ROTATION`. They are positions on the
panel, which does not turn, but were scaled to the rotated window as is, so
touch was wrong on a rotated screen (e.g. the portrait Raspberry Pi Touch
Display 2 turned to landscape). Mouse moves are not changed. Still present
in 1.18.1.

Change, in `calloop_backend/input.rs` (`new_xkb_state`): when the keymap
cannot be compiled, keyboard input is ignored with one warning instead of
taking the app down. Without the xkb data (/usr/share/X11/xkb),
`xkb_context_new()` returns NULL, which the xkbcommon crate does not check:
the first key pressed segfaulted in libxkbcommon (and a keymap that failed
to compile panicked). Still present in 1.18.1.

Also, so the app's builds have no warning: in `drmoutput.rs`,
`get_supported_formats` allows dead code when the software display is not
built (femtovg alone, as here); in `display/gbmdisplay.rs`, the catch-all
arm of `filter_gl_config` allows being unreachable (glutin with EGL only,
as in the image).
