# Contributing to OpenRig

Thanks for helping! Every kind of help is welcome: bug reports, hardware
reports, ideas, documentation and code. The full guide is on the site:
[openrig.eu/docs/dev/contributing](https://openrig.eu/docs/dev/contributing/).

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
   [site repository](https://github.com/Siryu6/openrig-site).

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

## License

OpenRig is GPL-3.0-or-later. By contributing, you agree to license your
contribution under the same terms.
