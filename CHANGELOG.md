# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `Scene` and `Adjustable`: the scene table — 37 light modes with names, ids,
  per-class availability and, per scene, whether `speed` and `dimming` do
  anything. `Scene::all` is a `const`, so a scene picker needs no bulb;
  `Scene::for_class`, `BulbType::scenes` and `Bulb::scenes` narrow it to what a
  given class, module or device can play.
- `Scene::from_name` matches leniently — case, spaces and punctuation are all
  ignored, so `Deep dive`, `deep-dive` and `DEEPDIVE` are one scene, and the
  spellings used by `pywizlight`, openHAB and Adafruit all resolve.
- `Pilot::scene` names the scene a bulb reports playing, where the table knows
  the id.
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

- **`SceneId::new` is fallible**, and checks the id against the scene table
  rather than against a range. There is no useful range to check: measured on
  `ESP25_SHRGB_01` fw 1.38.0, the bulb accepts every id in `1..=248` — the ~200
  that name no scene included — and answers `success`, so a range check would
  wave through ids that do nothing. `TryFrom<u16>` replaces `From<u16>`, and
  `SceneId::scene` goes the other way without a `Result`, since an id that names
  nothing cannot be a `SceneId`. Ids a bulb may *report* but not accept — `0`
  for "no scene", `256..=265` for a custom mode made in the app — stay
  expressible on the read side, where `Pilot::scene_id` is a plain `u16`.
- **`speed` is refused where it can do nothing**: alongside `r`/`g`/`b`/`c`/`w`
  or `temp`, which stop any scene, and alongside a scene the table calls static.
  On its own it is still allowed, and retunes whatever scene is already running.
  A scene no source documents is *not* treated as static.
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

### Measured against hardware

Every parameter range below was swept to its edges on two `ESP25_SHRGB_01`
bulbs running firmware 1.38.0, which agreed on all of it. Three of the values
inherited from `pywizlight` turned out to be wrong.

- `Kelvin` accepts `1000..=12000`, was `1000..=10000`. The crate was refusing
  temperatures the bulb takes.
- `Dimming` still enforces `1..=100`, but that is a **client-side policy, not a
  wire bound**: the bulb accepts every `u8` and silently clamps. The previous
  claim that `0` is "out of range on the wire" was wrong.
- `SceneId` docs claimed custom slots at `256..=265` and Rhythm at `1000`. Both
  are **refused** on this firmware; a write is accepted for `1..=248`, and that
  accepted range is contiguous, so it is a range check rather than a table of
  valid scenes.
- `Speed` `10..=200`, `Ratio` `0..=100` and `Devices` `1..=3` were all correct,
  and are no longer marked unverified. `3` working on a single-head bulb where
  `2` does not is the evidence for `3` meaning "every head".
- `getPilot` addresses heads **zero-based** while replies tag them one-based,
  confirming `Devices` must not be reused for reads.
- Key order inside `params` makes no difference, which is what justifies not
  enabling `serde_json/preserve_order`.
- `success: false` could not be provoked by any means tried; every failure is
  an `error` envelope. Handling it remains defensive against an unobserved case.

### Notes on the pilot surface

- Colour, colour temperature and scene are **mutually exclusive, and a builder
  that sets two of them fails** in `set_pilot` / `set_state` / `params` rather
  than silently keeping the last one. A caller that sets both has a bug, and a
  bulb that receives both has no defined behaviour.
- Only requests carry the validated newtypes. Results carry plain integers,
  because a bulb may report a value it would refuse to be sent — `dimming: 0`
  on an off bulb — and a validating parse would turn that into an error.
- `Channel` construction is infallible, so it does not return a `Result` that can
  never be `Err`. `SceneId` was too, and is not any more: see above.
- `reboot` **does not work on the measured hardware**: it is refused with
  `-32600 Invalid Request` in every spelling of `params`, and the bulb keeps
  running. Not `-32601`, so the firmware knows the method and declines it. The
  method is kept for models that may implement it, and now returns
  `Error::Device` there rather than pretending to succeed — the mock bulb was
  previously inventing a `{"success": true}` no hardware has ever sent.
  `reset` is untested and assumed to match.
- `reboot` and `reset` stay fire-and-forget for *silence*: a device that really
  did reboot has an obvious reason not to answer, so a timeout is success while
  an explicit refusal is not.
- `set_pilot` / `set_state` return `()` and raise `Error::Device` if the bulb
  acknowledges with `success: false`, rather than handing back an ack that is
  easy to ignore.

### Where the scene table comes from

None of it is measured, and **acceptance cannot measure it**: the bulb takes any
id in `1..=248`, so a `success` is no evidence that a scene exists.

- Ids `1..=33` — names, per-class availability, and whether `speed` and
  `dimming` apply — are WiZ's own [light-mode table][light-modes], published with
  the Pro API. It is the only source that states the last two.
- Ids `34..=36` and `40` are `pywizlight`'s. Nothing found says whether they
  animate, so `Scene::adjustable` reports `None` for them rather than a guess.
  `37`, `38` and `39` name nothing anywhere.
- Where the sources disagree, the vendor's wins and the disagreement is
  documented on the module. Its tunable-white column matches `pywizlight`
  exactly; its dimmable-white column adds three ids `pywizlight` leaves out, and
  contradicts the vendor's own prose in the same document. Adafruit's library
  disagrees about three more on whether they animate.
- `1000` (Rhythm) and `256..=265` (the app's custom modes) are **not** in the
  table, because a write of either is refused on the measured firmware.
  `pywizlight`'s mapping for the slots comes from hardware *reporting* them, not
  accepting them, so `Scene::from_id` answers `None` and nothing pretends they
  can be set.

Settling any of this needs someone looking at a bulb of each class, which is a
hardware pass rather than a test.

### Known gaps

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

[light-modes]: https://docs.pro.wizconnected.com/#light-modes
[Unreleased]: https://github.com/LucasAmion/wizlight-rs/compare/v0.1.0-alpha.1...HEAD
[0.1.0-alpha.1]: https://github.com/LucasAmion/wizlight-rs/releases/tag/v0.1.0-alpha.1
