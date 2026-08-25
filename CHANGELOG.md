# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `ColourStrategy`: how a colour becomes channel values, **chosen by the
  caller**. `ColourStrategy::Rgb` sends the three colour channels and leaves
  both whites dark; `ColourStrategy::Trapezoid` is `pywizlight`'s algorithm,
  which blends a white emitter in as saturation falls. Neither is a default and
  neither is hidden: which one looks better is a question about a bulb in a
  room, and M2 answers it on hardware. Explicit five-channel control through
  `PilotBuilder::rgbww` is untouched and remains the way to bypass both.
- `Hs`, a hue in degrees and a saturation in percent. The hue wraps, so `-30`,
  `330` and `690` are one colour; the saturation does not, because a percentage
  outside `0..=100` is arithmetic gone wrong rather than a colour.
- `Rgbcw`, the five channel values a conversion produces, and
  `PilotBuilder::colour`, which puts them on the wire — all five of them,
  zeroes included, so the request describes the whole light instead of a change
  to part of it.
- `Rgbcw::to_hs` reads channels back as a colour (`pywizlight`'s `rgbcw2hs`),
  with `Pilot::colour` and `Pilot::hs` for doing it to what a bulb reported.
- `Scene`, `Category` and `Adjustable`: the scene table — 40 light modes with
  ids, names, per-class availability, the colour temperature the white ones run
  at, and per scene whether `speed` and `dimming` do anything. `Scene::all` is a
  `const`, so a scene picker needs no bulb; `Scene::for_class`,
  `BulbType::scenes` and `Bulb::scenes` narrow it to what a given class, module
  or device can play.
- `Scene::animates` and `Adjustable::speed` answer **different** questions.
  Wake up, Bedtime, Candlelight and Alarm all animate and take no `speed`: their
  rate is set in the WiZ app, not on the wire. Hide a rate control on the
  second; decide whether a scene is "moving" with the first.
- `Scene::from_name` matches leniently — case, spaces and punctuation are all
  ignored, so `Deep dive`, `deep-dive` and `DEEPDIVE` are one scene, and the
  spellings used by `pywizlight`, openHAB and Adafruit all resolve.
- `SceneId::user_slot` addresses the ten custom light modes saved from the WiZ
  app, and `SceneId::as_user_slot` recognises one.
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

- **`SceneId::new` is fallible**, and checks the id against what the bulb will
  actually *play* rather than against what it accepts. Those differ, and
  silently: `37` is accepted and sets a 2200 K colour temperature instead of a
  scene, and `42..=248` are accepted and clamped to `41`. Both answer `success`,
  so both are refused here with a message saying what they really do.
  `TryFrom<u16>` replaces `From<u16>`. `SceneId::scene` returns an `Option`,
  because a user slot has no table entry. `0` — "no scene, colour is active" —
  stays expressible where it occurs, on the read side, where `Pilot::scene_id`
  is a plain `u16`.
- **`speed` is refused where it can do nothing**: alongside `r`/`g`/`b`/`c`/`w`
  or `temp`, which stop any scene, and alongside a scene whose rate cannot be
  set. On its own it is still allowed, and retunes whatever scene is already
  running — measured, and the way the WiZ app does it. A user slot is allowed a
  `speed` too, since a custom mode may well be a dynamic one.
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
  were refused, and only Rhythm stayed refused: the slots work as soon as a
  custom mode is saved into one. A write is accepted for `1..=248`, but only
  `1..=41` does anything — see the scene table below.
- `Speed` `10..=200`, `Ratio` `0..=100` and `Devices` `1..=3` were all correct,
  and are no longer marked unverified. `3` working on a single-head bulb where
  `2` does not is the evidence for `3` meaning "every head".
- `getPilot` addresses heads **zero-based** while replies tag them one-based,
  confirming `Devices` must not be reused for reads.
- Key order inside `params` makes no difference, which is what justifies not
  enabling `serde_json/preserve_order`.
- `success: false` could not be provoked by any means tried; every failure is
  an `error` envelope. Handling it remains defensive against an unobserved case.
- A running scene is **undisturbed by `dimming`-only traffic**: 138 packets over
  15 s at 9.2/s, all acknowledged, none lost, the animation smooth throughout and
  the scene and its speed intact afterwards. Brightness can be modulated
  underneath a scene, at least to 10 Hz — except on the three scenes that ignore
  `dimming` entirely.

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

### Notes on the RGB+CW conversion

`ColourStrategy::Trapezoid` is a port, and a deliberately faithful one: it is
checked against 361 values recorded from `pywizlight` 0.6.6
(`tests/data/rgbcw_golden.json`, regenerated by the script beside it) and it
reproduces every one of them, including the places where the original is
peculiar. The inputs are *chosen* rather than swept — one pass round the hue
circle, then the boundaries between the algorithm's branches — and the choice
was checked by mutation: nine deliberate breakages of the port, and each one
fails a test. Density beyond that only made the table harder to review.
The dense checking that is worth doing needs no recorded answers, and sweeps a
million colours for range, stability and round-trip error.

It has since been **checked against hardware** — see the section below, which is
what decides when to use which.

- **The white channel is the caller's choice**, because `pywizlight`
  contradicts itself: its algorithm computes a value it calls `cw` and documents
  as the cold white (~6200 K), and its client sends that number on `w`, the warm
  white (~2800 K). Measured, both are right for half the hue circle — see below.
- **Saturation is discontinuous at 0.5**, which is the point of the algorithm:
  above it the colour stays saturated and the white ramps from 128 to 0, below
  it the white is pinned at 128 and the colour fades to black. The white
  channel never exceeds 128 of a possible 255.
- **Zero saturation is a lit white, not darkness.** `(0, 0, 0)` converts to a
  full cold white. No colour means no colour, not no light; off is `state`'s
  job.
- **A dim RGB triple is read as a pale colour, not a dim one.** The conversion
  recovers saturation from the *length* of the triple, so `(128, 0, 0)` is a
  half-saturated red and comes back as a full red plus a full white — brighter
  than what went in, and pink. Dim with `dimming`, or say what the saturation
  is with `apply_hs`.
- **The inverse is approximate, and the tests pin how approximate.** Swept over
  every hue at 0.25° and every saturation at 0.5%: above the step, saturation
  round-trips within 0.375 points and the channels are a fixed point; at the
  step and below, saturation reads as much as 6.7 points low, because it is
  recovered from a vector the forward conversion had rescaled to fit the gamut
  and nothing undoes that. Hue holds to 0.5° while saturation is at least 25,
  and falls apart below 5, where almost no colour is left to read.
- **The port is deterministic across platforms where `pywizlight` is not.**
  The three primaries are frozen constants rather than `cos`/`sin` calls,
  because the last bit of `cos(2π/3)` is not the same on macOS as on Linux —
  CI caught it. `pywizlight` inherits whatever its platform's `libm` says; this
  does not, which is what lets the `rgb2rgbcw` golden test demand an exact byte
  everywhere instead of allowing a tolerance.
- Two rounding quirks are inherited on purpose. A hue exactly between two
  primaries yields 254 rather than 255 on one channel, and the conversion
  truncates where it might have rounded. Both are what `pywizlight` does, so
  both are what the port does; the golden table would fail if either were
  "fixed" quietly.

`ColourStrategy::Rgb` is not a port of anything: from an RGB triple it is a
pass-through, and from `Hs` it is standard HSV at full value, sharing the hue
geometry so that the two strategies agree exactly at full saturation.

### Measured against hardware: which colour strategy to use

Two `ESP25_SHRGB_01` on firmware 1.38.0, judged by eye at `dimming: 100`, first
side by side and then one lamp at a time with the two candidates blinded.
**Blinding reversed two of six verdicts**, so only the blinded pass is quoted;
seeing two lit lamps at once, and knowing which was which, was worth a
measurable amount of preference.

- **Cold or warm is not one answer — it depends on the hue.** The emitter whose
  colour temperature matches the hue preserves it; the opposing one washes the
  colour to white. Warm for warm hues, cold for cool, on every colour tried and
  under both methods. `pywizlight`'s code (always warm) and its comments (a cold
  white) are therefore each right for half the hue circle, and `WhiteChannel`
  stays the caller's choice for a measured reason rather than for lack of
  evidence. Choosing the emitter *from the hue* would beat either fixed choice;
  that is a new algorithm, so it is not in this port.
- **A near-white is better from the blend**, unanimously, across every run. That
  is the case the algorithm decisively wins.
- **"Far better pastels" did not survive blinding.** With the hue-matched
  emitter, three of the pastels disagreed with themselves between runs, and a
  warm orange never once preferred the blend. The claim narrows to near-whites.
- **Lighting both white emitters at once is not the missing piece**: the single
  hue-matched white won 4 of 5, and both at full was consistently "too white".
- **The white emitters are far more luminous per unit than the colour ones**:
  `c=33` out of a possible 128 visibly whitens a fully saturated orange. The
  shared power budget is real, even though the ~6200 K / ~2800 K figures are
  still inherited rather than measured.
- **The blend cannot reach some colours at all.** Below saturation 0.5 it keeps
  one primary and leaves the white to supply the rest, so a pale pink comes back
  orange under the warm emitter and white under the cold one — never pink.
  `ColourStrategy::Rgb` renders it correctly, keeping all three primaries.

Two bulbs, one model, one observer, one room. Enough to overturn an assumption
that had nothing behind it; not enough to state as a property of WiZ hardware in
general. Method and full results:
[`docs/captures/colour-esp25-shrgb-01-fw1.38.0.json`][colour-capture].

### Where the scene table comes from

**Measured.** Every id was written to an `ESP25_SHRGB_01` on fw 1.38.0 and read
back, and the WiZ app was then walked through mode by mode with `getPilot`
polled throughout, so the vendor's own software supplied the names and the
grouping. What that found contradicts every published source somewhere.

The bulb makes the behaviour observable by reporting only the parameters the
running scene uses — `speed` if and only if its rate can be set, `dimming` if
and only if its brightness can be. So `Adjustable` is watched rather than
transcribed, and it corrects [WiZ's own light-mode table][light-modes] twice:
that table claims an adjustable speed for **Cozy** and **Candlelight** and
neither reports one, Cozy having been confirmed by eye to hold still. It also
flags only Night light as undimmable, where **Wake up** and **Alarm** ignore
`dimming` as well.

- **The scene space is `1..=41`, not `1..=248`.** Ids `42..=248` are accepted
  and clamped to `41`. An earlier pass here recorded only accept/refuse, never
  read the state back, and so concluded the bulb takes ~200 ids that name
  nothing. It does not. Of those, `1..=36` and `38..=40` are worth sending;
  `37` and `41` are not, for different reasons.
- **`38` and `39` are real scenes no source lists** — static whites at 3500 K
  and 5000 K, named here and nowhere else: **Soft white** and **Crisp white**,
  descriptions of where each was measured to sit rather than names WiZ uses.
  Both reach full brightness, checked against their own colour temperatures.
- **`41` is a scene the crate refuses to send.** It plays a white of roughly
  6200 K — placed by comparison against a reference bulb — but at `dimming: 100`
  it emits about a *third* of what every other white does. It obeys `dimming`
  below that ceiling, so the parameter is not ignored; the scale is a different
  one, and a caller asking for full brightness quietly gets a third of it, with
  no way to recover the rest. `SceneId::new(41)` fails and points at
  `temp: 6200`. It is still explained on the read side, because `42..=248` are
  all clamped onto it.
- **`37` is not a scene.** It is accepted and sets a 2200 K colour temperature,
  reporting `sceneId: 0`.
- **The user slots work.** An earlier measurement found `256` refused and
  concluded this firmware has no user slots. The slot was empty: saving a custom
  mode in the app makes its id writable, and saving a second — never played —
  made `257` work while `258..=265` stayed refused. `pywizlight` was right, for
  writing as well as reading. `1000` (Rhythm) is still refused, and appears to
  have stopped being a `sceneId` at all.
- **Per-class availability is the one part still unverified**, since only colour
  hardware was on hand. It is WiZ's table for `1..=33` and `pywizlight`'s for
  `34..=36` and `40`; the two agree exactly on tunable white and disagree on
  dimmable white, where WiZ also lists Cool white, Golden white and Diwali. WiZ
  is followed, though its own prose contradicts its own table.

### Known gaps

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

[colour-capture]: https://github.com/LucasAmion/wiz-workspace/blob/main/docs/captures/colour-esp25-shrgb-01-fw1.38.0.json
[light-modes]: https://docs.pro.wizconnected.com/#light-modes
[Unreleased]: https://github.com/LucasAmion/wizlight-rs/compare/v0.1.0-alpha.1...HEAD
[0.1.0-alpha.1]: https://github.com/LucasAmion/wizlight-rs/releases/tag/v0.1.0-alpha.1
