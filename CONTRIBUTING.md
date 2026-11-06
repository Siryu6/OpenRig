# Contributing to OpenRig

Thanks for helping! Every kind of help is welcome: bug reports, hardware
reports, ideas, documentation and code. The full guide is on the docs site:
[docs.openrig.eu/dev/contributing](https://docs.openrig.eu/dev/contributing/).

By taking part, you agree to follow the [code of conduct](CODE_OF_CONDUCT.md).

## Issues

[Open an issue](https://github.com/Siryu6/OpenRig/issues/new/choose) and pick
the form that fits: bug report, feature request or hardware report. Give your
OpenRig version (Settings → About), your board and your audio interface.
Reports from x86_64 mini PCs and from other audio interfaces are especially
useful.

Security problems: see [SECURITY.md](SECURITY.md), not a public issue.

## Pull requests

1. Fork, branch from `main`, and open a pull request against `main`.
2. Run the checks that CI runs:

   ```sh
   cargo fmt --check
   cargo clippy --all-targets
   cargo test
   ```

3. Update the docs in `docs/` with the change. User-facing pages live in the
   [docs repository](https://github.com/Siryu6/openrig-doc).

For a large change, open an issue first so we can agree on the approach.

## Commits and releases

Commit messages follow [conventional commits](https://www.conventionalcommits.org):
`feat:`, `fix:`, `perf:`, `docs:`, `ci:`, `chore:`… Versions follow
[semantic versioning](https://semver.org) and are computed from them.

[release-plz](https://release-plz.dev) keeps a release pull request with the
next version and its changelog. Merging it tags the release, and a workflow
builds the binaries, the OS images and the signed updates.

## Real-time code

Code that runs in the audio callback must not allocate, lock (other than
`try_lock`), log or do I/O. When in doubt, do the work on the chain builder
thread and send the result.

## Errors, panics and logs

The app must not crash at runtime. Clippy enforces it (`Cargo.toml`
`[lints]`; tests are exempt through `clippy.toml`):

- no `unwrap`, `expect`, `panic!`, `todo!` outside tests. Return a `Result`
  (`anyhow` in the app, `thiserror` where callers match on the error), or
  handle the case where it happens: a default, an early return, a skipped
  item, with a log line when something was lost;
- no indexing or slicing that can go out of bounds on data from outside
  (files, plugins, the network, the user): `get`, `split_first`, iterators;
- locks: `crate::sync::MutexExt::locked()` (and its `RwLock` / `Condvar`
  siblings) instead of `lock().unwrap()`: a panic elsewhere does not
  poison the rest of the app;
- `unsafe` only where there is no safe way (C libraries: LV2, JACK, NAM,
  libc; lock-free handoff to the DSP workers), each block with a
  `// SAFETY:` comment saying why it is sound, each `unsafe fn` with a
  `# Safety` section;
- an error is either returned or logged, never dropped silently. `let _ =`
  only for results that do not matter, with a comment when it is not
  obvious.

Logs go through `tracing` (syslog on the OS image, the terminal on a
desktop). The target, the module path, names the subsystem, so messages
carry no prefix of their own:

```rust
use tracing::{debug, error, info, warn};

info!(ssid = %ssid, "Joining Wi-Fi network");
warn!(path = %path.display(), error = %e, "Preset skipped");
error!(error = format_args!("{e:#}"), "Audio disabled");
```

- the message is a short sentence, capital first, no final period, with
  what varies in fields (`name = value`; `%` for `Display`, `?` for
  `Debug`); `error` for the error, `format_args!("{e:#}")` for an
  `anyhow::Error` so its causes are kept;
- `error`: a feature is lost (no audio, a save failed); `warn`: something
  went wrong and was worked around (a file skipped, a retry); `info`:
  lifecycle and configuration (start, devices, a preset loaded); `debug`:
  details for a developer (`RUST_LOG=debug`);
- never on the audio thread (see above); nothing that repeats every period.

## License

OpenRig is GPL-3.0-or-later. By contributing, you agree to license your
contribution under the same terms.
