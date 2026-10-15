# lilv 0.2.4, patched

Copy of the [lilv](https://crates.io/crates/lilv) crate 0.2.4 (MIT, see
LICENSE), used through `[patch.crates-io]` in the pedalboard's Cargo.toml.

Change: the UI support callback in `src/ui.rs` takes `*const c_char` instead
of `*const i8`. `c_char` is `u8` on ARM Linux (Raspberry Pi), where the
published crate does not build. A `#[must_use]` on a trait method (a
warning with recent compilers) is removed too. Drop this copy once a fixed release exists.
