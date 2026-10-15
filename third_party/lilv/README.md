# lilv-rs

[![crates.io](https://img.shields.io/crates/v/lilv.svg)](https://crates.io/crates/lilv)
[![docs.rs](https://docs.rs/lilv/badge.svg)](https://docs.rs/lilv)

[![License: MIT](https://img.shields.io/badge/License-MIT-green.svg)](https://opensource.org/licenses/MIT)
[![Tests](https://github.com/wmedrano/lilv-rs/actions/workflows/test.yml/badge.svg)](https://github.com/wmedrano/lilv-rs/actions/workflows/test.yml)

[:heart: Sponsor](https://github.com/sponsors/wmedrano)

This is a Rust wrapper for [Lilv](http://drobilla.net/software/lilv),
the LV2 host library. For a simpler/fuller experience, consider using
the [livi-rs](https://github.com/wmedrano/livi-rs) library.

## Completeness

This crate targets version 0.24.2 of Lilv.

Most components are implemented, but not fully tested yet. There is currently no support for State.
