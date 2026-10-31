# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.9.0](https://github.com/Siryu6/OpenRig/compare/v0.8.0...v0.9.0) - 2026-10-31

### Added

- *(ui)* presets and model fields in the strip, editor pages ([#76](https://github.com/Siryu6/OpenRig/pull/76))
- *(hardware)* external I2C encoders on the rack's knob strip ([#72](https://github.com/Siryu6/OpenRig/pull/72))
- *(ui)* add a block with the encoder alone ([#70](https://github.com/Siryu6/OpenRig/pull/70))
- *(rack)* pick a list option from a pull-up list in the knob strip ([#66](https://github.com/Siryu6/OpenRig/pull/66))
- *(editor)* arrange a plugin's parameters and its knob strip ([#65](https://github.com/Siryu6/OpenRig/pull/65))
- *(ui)* presets, Live page, looper and tuner with the encoder alone ([#64](https://github.com/Siryu6/OpenRig/pull/64))
- *(ui)* drive the editor and pages with the encoder alone ([#62](https://github.com/Siryu6/OpenRig/pull/62))
- *(editor)* browse the model folders, TONE3000 first in the list ([#49](https://github.com/Siryu6/OpenRig/pull/49))
- *(ui)* double tap a knob to reset it, hold it to compare A/B ([#51](https://github.com/Siryu6/OpenRig/pull/51))
- *(rack)* undo a removed block, remove it from the editor or by dragging ([#47](https://github.com/Siryu6/OpenRig/pull/47))
- *(os)* pedalboard-report for bug reports, speaker-test in the image

### Fixed

- *(ui)* break the top bar's compact/width binding loop
- *(ui)* keep the plugin editor's parameters visible on 800x480 screens

### Other

- *(deps)* move to Slint 1.18.1 and lilv 0.3.1 ([#75](https://github.com/Siryu6/OpenRig/pull/75))
- *(os)* build the app with Rust 1.99.1 ([#74](https://github.com/Siryu6/OpenRig/pull/74))
- build the tests with line tables only to stay within the runner's memory ([#73](https://github.com/Siryu6/OpenRig/pull/73))
- *(os)* move both images to Linux 6.18 ([#67](https://github.com/Siryu6/OpenRig/pull/67))
- *(store)* drop the unused bundle_path ([#71](https://github.com/Siryu6/OpenRig/pull/71))
- trim the UI's public surface and test-only helpers ([#69](https://github.com/Siryu6/OpenRig/pull/69))
- one atomic JSON save and load for every settings file ([#68](https://github.com/Siryu6/OpenRig/pull/68))
- Merge pull request #63 from Siryu6/fix/top-bar-binding-loop
- Merge pull request #61 from Siryu6/chore/deps-majors
- Merge pull request #60 from Siryu6/feat/model-folders
- *(deps)* update jack, thiserror, dirs, rppal, minisign-verify and blake2
- Merge pull request #57 from Siryu6/feat/delete-undo
- Merge pull request #59 from Siryu6/feat/param-reset-compare
- Merge pull request #58 from Siryu6/chore/neuralaudio-bump
- *(deps)* update NeuralAudio to b98dfbdfdc
- Merge pull request #56 from Siryu6/chore/deps-rust
- *(deps)* update Rust dependencies within semver ranges
- Merge pull request #55 from Siryu6/fix/editor-compact

## [0.8.0](https://github.com/Siryu6/OpenRig/compare/v0.7.0...v0.8.0) - 2026-10-28

### Added

- *(lv2)* load the factory presets of LV2 plugins
- *(lv2)* load files in LV2 plugins with file parameters

### Fixed

- *(ui)* keep the menu button on screen at 800x480

### Other

- Merge pull request #43 from Siryu6/feat/lv2-file-params

## [0.7.0](https://github.com/Siryu6/OpenRig/compare/v0.6.1...v0.7.0) - 2026-10-27

### Added

- *(tone3000)* select all the models of a page, 25 at a time
- *(tone3000)* search, ticked model downloads and request pacing
- *(tone3000)* sign in through the OpenRig application

## [0.6.1](https://github.com/Siryu6/OpenRig/compare/v0.6.0...v0.6.1) - 2026-10-22

### Fixed

- *(os)* keep every stereo pair of 8-output Pi 5 HATs after an xrun

## [0.6.0](https://github.com/Siryu6/OpenRig/compare/v0.5.0...v0.6.0) - 2026-10-22

### Added

- *(store)* install bundles from Debian packages

### Fixed

- *(plugins)* host the LV2 Worker so plugins needing work:schedule load ([#31](https://github.com/Siryu6/OpenRig/pull/31))

### Other

- Merge pull request #33 from Siryu6/feat/store-deb

## [0.5.0](https://github.com/Siryu6/OpenRig/compare/v0.4.0...v0.5.0) - 2026-10-20

### Added

- *(editor)* search in long lists of tones and presets ([#21](https://github.com/Siryu6/OpenRig/pull/21))
- *(nam)* EQ position Pre / Post on the amp blocks ([#23](https://github.com/Siryu6/OpenRig/pull/23))
- *(settings)* show the device IP addresses ([#20](https://github.com/Siryu6/OpenRig/pull/20))

### Fixed

- *(audio)* log the interface ports and the JACK connections ([#19](https://github.com/Siryu6/OpenRig/pull/19))

### Other

- Merge pull request #28 from Siryu6/feat/nam-eq-position
- Merge pull request #29 from Siryu6/fix/jack-connect-logging
- Merge pull request #30 from Siryu6/feat/choice-search

## [0.4.0](https://github.com/Siryu6/OpenRig/compare/v0.3.0...v0.4.0) - 2026-10-20

### Added

- *(store)* show plugin pictures in the rack and the Add effect page
- *(editor)* plugin views inferred from LV2 ports
- *(store)* install LV2 plugins from the signed catalog
- *(presets)* keep LV2 values by port symbol
- *(ui)* light theme

### Fixed

- *(editor)* clippy some_filter on the ratio handle

### Other

- community health files

## [0.3.0](https://github.com/Siryu6/OpenRig/compare/v0.2.0...v0.3.0) - 2026-10-18

### Added

- *(rack)* Send and Return blocks ([#9](https://github.com/Siryu6/OpenRig/pull/9))
- *(audio)* sends and returns on the blocks of a preset ([#9](https://github.com/Siryu6/OpenRig/pull/9))
- *(ui)* routing view for the inputs and outputs ([#9](https://github.com/Siryu6/OpenRig/pull/9))
- *(audio)* use every input and output of the interface ([#9](https://github.com/Siryu6/OpenRig/pull/9))

### Other

- inputs, outputs, sends and returns ([#9](https://github.com/Siryu6/OpenRig/pull/9))
- *(routing)* tidy the sends, returns and routing view ([#9](https://github.com/Siryu6/OpenRig/pull/9))
- point the README to openrig.eu and its user guide
- Merge pull request #7 from Siryu6/ci/shellcheck
- check the OS shell scripts with shellcheck

## [0.2.0](https://github.com/Siryu6/OpenRig/compare/v0.1.0...v0.2.0) - 2026-10-17

### Added

- in-app updates and hardware settings

### Other

- open release PRs only for feat, fix and perf commits
- save the compiler cache of the OS image builds

## [0.1.0](https://github.com/Siryu6/OpenRig/releases/tag/v0.1.0) - 2026-10-16

### Added

- *(tone3000)* follow the TONE3000 branding guidelines

### Other

- add PR checks, release-plz releases and release builds
- Import OpenRig
- Initial commit
