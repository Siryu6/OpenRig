# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.17.0](https://github.com/Siryu6/OpenRig/compare/v0.16.0...v0.17.0) - 2026-11-16

### Added

- play a plugin switch from a momentary footswitch ([#228](https://github.com/Siryu6/OpenRig/pull/228))

### Fixed

- say "1 beat" and "2 beats" on unit labels ([#226](https://github.com/Siryu6/OpenRig/pull/226))

## [0.16.0](https://github.com/Siryu6/OpenRig/compare/v0.15.0...v0.16.0) - 2026-11-16

### Added

- draw a step grid view from a plugin's StepGrid stream ([#222](https://github.com/Siryu6/OpenRig/pull/222))
- print a plate name in its glitch style ([#221](https://github.com/Siryu6/OpenRig/pull/221))
- add a Pitch family for pitch shifters ([#215](https://github.com/Siryu6/OpenRig/pull/215))
- *(audio)* give plugins a running bar and beat position ([#214](https://github.com/Siryu6/OpenRig/pull/214))
- read the view, roles and strip controls OpenRig FX plugins state ([#209](https://github.com/Siryu6/OpenRig/pull/209))
- show OpenRig FX faces and badges in the Add effect page and the store ([#200](https://github.com/Siryu6/OpenRig/pull/200)) ([#201](https://github.com/Siryu6/OpenRig/pull/201))
- show signature OpenRig FX plugins as mini pedals in the rack ([#198](https://github.com/Siryu6/OpenRig/pull/198)) ([#199](https://github.com/Siryu6/OpenRig/pull/199))
- draw signature OpenRig FX plugins on their plate ([#197](https://github.com/Siryu6/OpenRig/pull/197))
- read OpenRig FX faceplates and give basic plugins their look ([#194](https://github.com/Siryu6/OpenRig/pull/194))

### Fixed

- show whole knob rows on plates and a legible page count ([#224](https://github.com/Siryu6/OpenRig/pull/224))
- give a plate without a view its height for its controls ([#218](https://github.com/Siryu6/OpenRig/pull/218))
- show the knob strip in the rack's order on the web remote ([#217](https://github.com/Siryu6/OpenRig/pull/217))
- show a drive's measured tone in a dark window on its plate ([#205](https://github.com/Siryu6/OpenRig/pull/205))
- show short modes of OpenRig FX plugins as a switch at every size ([#207](https://github.com/Siryu6/OpenRig/pull/207))

### Other

- fold the OpenRig FX look into the block face and colour rules ([#203](https://github.com/Siryu6/OpenRig/pull/203))

## [0.15.0](https://github.com/Siryu6/OpenRig/compare/v0.14.3...v0.15.0) - 2026-11-15

### Added

- *(lv2)* bypass plugins through their lv2:enabled port ([#174](https://github.com/Siryu6/OpenRig/pull/174))
- compensate plugin latency and report it to JACK ([#147](https://github.com/Siryu6/OpenRig/pull/147))
- back up and restore presets, setlists, rigs, loops and settings ([#138](https://github.com/Siryu6/OpenRig/pull/138))
- *(audio)* global output EQ and limiter per output ([#135](https://github.com/Siryu6/OpenRig/pull/135))

### Fixed

- *(hardware)* build with the gpio feature again, and check it in CI ([#186](https://github.com/Siryu6/OpenRig/pull/186))
- *(os)* vendor the app's crates before each build so pedalboard-rebuild sees a new Cargo.lock ([#184](https://github.com/Siryu6/OpenRig/pull/184))
- *(os)* give the app an OpenRig folder on ext4 USB drives ([#154](https://github.com/Siryu6/OpenRig/pull/154))
- drop the USB drive messages when the drive goes ([#152](https://github.com/Siryu6/OpenRig/pull/152))
- eject only the USB drive the panel shows ([#150](https://github.com/Siryu6/OpenRig/pull/150))
- *(os)* mount USB drives and eject them from the app ([#148](https://github.com/Siryu6/OpenRig/pull/148))
- *(ui)* draw rack wires as rectangles ([#143](https://github.com/Siryu6/OpenRig/pull/143))
- *(remote)* stop the Editor lagging on slow links ([#140](https://github.com/Siryu6/OpenRig/pull/140))

### Other

- build the app binary and its unit tests one at a time, so the Tests runner keeps enough memory ([#192](https://github.com/Siryu6/OpenRig/pull/192))
- *(ui)* wire block view handles and plots to PlotEvents once ([#188](https://github.com/Siryu6/OpenRig/pull/188))
- build the tests without debug info and with two jobs, so the runner keeps enough memory ([#190](https://github.com/Siryu6/OpenRig/pull/190))
- *(tone3000)* save model downloads through the shared download helper ([#182](https://github.com/Siryu6/OpenRig/pull/182))
- *(audio)* fold the guard and latency patch layers into the chain ([#172](https://github.com/Siryu6/OpenRig/pull/172))
- *(ui)* share the EQ curve builder and give EQ plots their own callbacks ([#170](https://github.com/Siryu6/OpenRig/pull/170))
- *(t3k)* one TONE3000 format table and one load rule ([#164](https://github.com/Siryu6/OpenRig/pull/164))
- *(os)* Linux 6.18.63 on x86_64, latest rpi-6.18.y on the Pi ([#180](https://github.com/Siryu6/OpenRig/pull/180))
- list the image's headless and remote features, drop the lilv bindings ([#162](https://github.com/Siryu6/OpenRig/pull/162))
- share the checked download and the encoder constants ([#178](https://github.com/Siryu6/OpenRig/pull/178))
- *(ui)* merge the duplicated UI glue and name the views ([#176](https://github.com/Siryu6/OpenRig/pull/176))
- *(drives)* one drive model for the rigs panel, backup and eject ([#168](https://github.com/Siryu6/OpenRig/pull/168))
- *(remote)* one frame pipeline for the Editor stream ([#166](https://github.com/Siryu6/OpenRig/pull/166))
- *(deps)* update crates, jack 0.14, argon2 0.6 and getrandom 0.4 ([#160](https://github.com/Siryu6/OpenRig/pull/160))
- *(remote)* send only the pixels that changed ([#158](https://github.com/Siryu6/OpenRig/pull/158))
- *(remote)* size the frame budget to the link ([#156](https://github.com/Siryu6/OpenRig/pull/156))
- *(remote)* encode Editor frames off the UI thread ([#145](https://github.com/Siryu6/OpenRig/pull/145))

## [0.14.3](https://github.com/Siryu6/OpenRig/compare/v0.14.2...v0.14.3) - 2026-11-14

### Fixed

- *(audio)* switch off a plugin that overruns the period now and then ([#130](https://github.com/Siryu6/OpenRig/pull/130))
- *(t3k)* load a capture in every block of its kind, whatever its gear ([#128](https://github.com/Siryu6/OpenRig/pull/128))

### Other

- Open the T3K browser on every gear, narrowed to the block's format ([#133](https://github.com/Siryu6/OpenRig/pull/133))

## [0.14.2](https://github.com/Siryu6/OpenRig/compare/v0.14.1...v0.14.2) - 2026-11-10

### Fixed

- *(rack)* draw the dragged block above the bottom panels and the trash ([#126](https://github.com/Siryu6/OpenRig/pull/126))
- *(ui)* show the encoder focus only while the encoder is in use ([#124](https://github.com/Siryu6/OpenRig/pull/124))

## [0.14.1](https://github.com/Siryu6/OpenRig/compare/v0.14.0...v0.14.1) - 2026-11-09

### Fixed

- *(audio)* switch off a plugin that overloads the CPU or puts out NaN ([#120](https://github.com/Siryu6/OpenRig/pull/120))

## [0.14.0](https://github.com/Siryu6/OpenRig/compare/v0.13.0...v0.14.0) - 2026-11-09

### Added

- *(looper)* click, multiply, half speed, waveform and saved loops ([#118](https://github.com/Siryu6/OpenRig/pull/118))
- *(ui)* reorganize Settings, Routing and Updates in the ⋮ menu, Hardware page ([#117](https://github.com/Siryu6/OpenRig/pull/117))
- *(remote)* full screen button in the web remote ([#114](https://github.com/Siryu6/OpenRig/pull/114))
- *(ui)* add a Quit OpenRig menu entry outside the OS image ([#115](https://github.com/Siryu6/OpenRig/pull/115))

### Fixed

- *(os)* wait for the Wi-Fi card before giving up on the hotspot ([#110](https://github.com/Siryu6/OpenRig/pull/110))
- *(os)* ship rtpmidid again by breaking its Kconfig dependency loop ([#107](https://github.com/Siryu6/OpenRig/pull/107)) ([#108](https://github.com/Siryu6/OpenRig/pull/108))
- *(remote)* draw rounded gradient shapes flat in the remote view ([#105](https://github.com/Siryu6/OpenRig/pull/105))

### Other

- *(remote)* stream the editor at 30 fps and draw right after input ([#112](https://github.com/Siryu6/OpenRig/pull/112))

## [0.13.0](https://github.com/Siryu6/OpenRig/compare/v0.12.1...v0.13.0) - 2026-11-09

### Added

- web remote control, phone or tablet as the pedal's screen ([#99](https://github.com/Siryu6/OpenRig/pull/99))
- network MIDI over RTP-MIDI with device approval ([#97](https://github.com/Siryu6/OpenRig/pull/97))

### Fixed

- *(os)* ship xkb keymap data so a physical keyboard no longer crashes the app ([#101](https://github.com/Siryu6/OpenRig/pull/101))

## [0.12.1](https://github.com/Siryu6/OpenRig/compare/v0.12.0...v0.12.1) - 2026-11-07

### Fixed

- *(os)* join WPA3 Wi-Fi networks and log why a join fails ([#94](https://github.com/Siryu6/OpenRig/pull/94))

## [0.12.0](https://github.com/Siryu6/OpenRig/compare/v0.11.0...v0.12.0) - 2026-11-07

### Added

- *(rigs)* find my rigs with /me/rigs and read favourited from the rig detail ([#91](https://github.com/Siryu6/OpenRig/pull/91))
- *(rigs)* browse, install, favourite and publish rigs on openrig.eu ([#89](https://github.com/Siryu6/OpenRig/pull/89))

## [0.11.0](https://github.com/Siryu6/OpenRig/compare/v0.10.1...v0.11.0) - 2026-11-06

### Added

- *(looper)* count-in and bar-quantized recording ([#81](https://github.com/Siryu6/OpenRig/pull/81)) ([#82](https://github.com/Siryu6/OpenRig/pull/82))
- *(rig)* export and import shareable rigs ([#85](https://github.com/Siryu6/OpenRig/pull/85))

### Fixed

- *(tone3000)* show the pictures TONE3000 now serves from assets.tone3000.com ([#84](https://github.com/Siryu6/OpenRig/pull/84))
- *(gpio)* leave GPIOs held by a driver alone ([#83](https://github.com/Siryu6/OpenRig/pull/83))

### Other

- *(release)* give the rig crate its own tag name so release-plz finds the app's releases ([#87](https://github.com/Siryu6/OpenRig/pull/87))
- point to docs.openrig.eu and Siryu6/openrig-doc ([#86](https://github.com/Siryu6/OpenRig/pull/86))

## [0.10.1](https://github.com/Siryu6/OpenRig/compare/v0.10.0...v0.10.1) - 2026-11-03

### Fixed

- *(os)* Raspberry Pi Touch Display 2 on the Pi 5 ([#79](https://github.com/Siryu6/OpenRig/pull/79))

## [0.10.0](https://github.com/Siryu6/OpenRig/compare/v0.9.0...v0.10.0) - 2026-11-01

### Added

- *(audio)* split branches run on two cores and long chains are cut into pipeline stages over up to 4 cores, with no added latency and sample-identical sound; Settings → Processing → DSP cores (Auto or 1–4), "· N cores" in the resource monitor ([#77](https://github.com/Siryu6/OpenRig/pull/77))
- *(os)* Settings → Audio & system: change pedalboard.txt from the screen (audio interface, sample rate, buffer, periods, screen rotation, Wi-Fi country) ([#77](https://github.com/Siryu6/OpenRig/pull/77))
- `pedalboard-bench`, in the images, times a rig of neural models offline ([#77](https://github.com/Siryu6/OpenRig/pull/77))

### Changed

- *(os)* the x86_64 image is built for x86-64-v3: neural models run about 1.8 times faster; it needs a CPU with AVX2 (Intel N100, Haswell or newer) ([#77](https://github.com/Siryu6/OpenRig/pull/77))
- structured logs, without time or colours under syslog ([#77](https://github.com/Siryu6/OpenRig/pull/77))

### Fixed

- a plugin that crashes in the audio callback no longer stops the audio: the dry signal plays and the chain is rebuilt ([#77](https://github.com/Siryu6/OpenRig/pull/77))
- no more crashes on bad input: presets, IR WAV files, plugin ranges, store index and archives, MIDI and I2C data, more than 16 audio ports ([#77](https://github.com/Siryu6/OpenRig/pull/77))
- a store index could write outside the cache, and a corrupt TONE3000 settings file could show a token in the logs ([#77](https://github.com/Siryu6/OpenRig/pull/77))

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
