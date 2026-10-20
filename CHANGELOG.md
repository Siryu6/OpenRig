# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
