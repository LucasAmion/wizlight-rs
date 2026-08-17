# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Typed pilot surface: `PilotBuilder`, validated newtypes (`Channel`, `Dimming`,
  `Kelvin`, `Speed`, `Ratio`, `SceneId`, `Devices`), and `Pilot` /
  `Success` result types.
- Typed config results: `SystemConfig`, `ModelConfig`, `UserConfig`, `Power`.
- `Bulb` methods for `get_pilot`, `set_pilot`, `set_state`, `get_system_config`,
  `get_model_config`, `get_user_config`, `get_power`, `reboot`, `reset`, and a
  `kelvin_range` helper that falls back from `getModelConfig` to `getUserConfig`
  on older firmware.
- CLI: `--version` / `-V`. There was none at all — an installed binary could
  not report which version it was.
- CLI: `-v`/`-vv`/`-vvv` set the log level, overridable by `RUST_LOG`. Logs go
  to stderr and are coloured only when stderr is a terminal, `NO_COLOR` is
  unset, and `--json` was not given.
- CLI: `--json` renders errors as JSON as well as results.

### Changed

- CLI: results go to stdout and everything else to stderr, so redirecting
  stdout yields data or nothing at all. A failure used to print a payload to
  stdout *and* an `anyhow` line to stderr, which meant `--json` emitted valid
  JSON followed by prose on another stream.
- CLI: a bare `wizlight` is a usage error (exit 2) rather than a success that
  happens to print help. `main` returns an `ExitCode` so the code and the
  rendered message are decided in one place.

### Fixed

- CLI: `wizlight --help` opened with "Global CLI flags shared across all
  commands.", the Rust doc comment on the arg struct, while `-h` showed the
  real description. clap promotes a doc comment to the long description unless
  told not to.
- CLI: `tracing-subscriber` was a dependency of the `cli` feature but nothing
  used it, and `-v`/`--verbose` was parsed and discarded.
- CLI: `color_disabled` was written, exported and never called, so
  `tracing-subscriber` wrote ANSI escapes into pipes and log files.

### Notes on the pilot surface

- Colour, colour temperature and scene are **mutually exclusive, and a builder
  that sets two of them fails** in `set_pilot` / `set_state` / `params` rather
  than silently keeping the last one. A caller that sets both has a bug, and a
  bulb that receives both has no defined behaviour.
- Only requests carry the validated newtypes. Results carry plain integers,
  because a bulb may report a value it would refuse to be sent — `dimming: 0`
  on an off bulb — and a validating parse would turn that into an error.
- `Channel` and `SceneId` construction is infallible, so it does not return a
  `Result` that can never be `Err`.
- `reboot` and `reset` are fire-and-forget: they return `()` and treat a
  timeout as success. Neither has been run against hardware, and a device that
  is rebooting or clearing its credentials has every reason not to answer.
- `set_pilot` / `set_state` return `()` and raise `Error::Device` if the bulb
  acknowledges with `success: false`, rather than handing back an ack that is
  easy to ignore.

### Known gaps

- No bulb model / capability parsing beyond the raw config structs, and no scene
  tables yet.
- No RGB ↔ RGB+CW conversion.
- No streaming path and no `syncPilot` push listener.
- `getPilot` per-head reads are not implemented. `devices` uses a one-based
  convention for writes and a zero-based one for reads, and only the former is
  modelled.
- The `wizlight` binary still stubs the command runner.

## [0.1.0-alpha.1] — 2026-08-14

First published version. It is an alpha in the literal sense: the protocol
surface is a third built, the CLI is a stub, and every public item may still be
renamed. It is on crates.io so that the release workflow, trusted publishing and
the docs.rs build are proven on something disposable rather than on `0.1.0`.

Cargo does not resolve prereleases from an ordinary requirement, so this version
is only reachable by asking for `0.1.0-alpha.1` exactly.

### Added

- Crate skeleton: package metadata, MSRV 1.85 and the `cli` feature layout.
- `Bulb` and `Bulb::request()`: the reliable request/response path, with paced
  datagrams, retries and reply matching.
- `Request` / `Response` / `DeviceError`: the protocol envelope, parsed so that
  unknown fields are ignored.
- `Error`, a typed error for each way an exchange can fail, and `RetryPolicy`,
  which configures how hard `request()` tries.
- `Discovery`: bulbs are found by repeated UDP broadcast and reported as they
  answer, deduplicated by MAC.

### Known gaps

- No typed `getPilot` / `setPilot`; requests are built from raw method names.
- No bulb model parsing, scene tables or RGB ↔ RGB+CW conversion.
- No streaming path and no `syncPilot` push listener.
- The `wizlight` binary installs and runs, but has no commands and exits with an
  error explaining that.

[Unreleased]: https://github.com/LucasAmion/wizlight-rs/compare/v0.1.0-alpha.1...HEAD
[0.1.0-alpha.1]: https://github.com/LucasAmion/wizlight-rs/releases/tag/v0.1.0-alpha.1
