# wizlight

An async Rust client for **Philips WiZ** smart bulbs, speaking the local UDP
protocol directly — no cloud, no account, no hub. The crate ships both a library
and a `wizlight` command-line tool.

It is a port of the parts of [`pywizlight`][pywizlight] that matter for
real-time control, and it is the protocol layer underneath
[WiZzard](https://github.com/LucasAmion/wizzard).

> **Status: early development, and the published versions are alphas.** The
> request/response transport, discovery and typed pilot/config methods are in,
> and so is the everyday CLI surface — `discover`, `status`, `info`, `on`,
> `off`, `toggle`, `set` and `scenes`. `watch` and `bench` wait on the push
> listener and the streaming write path, and are still stubbed. The API will
> change without warning until `0.1.0`.
>
> Alphas are published to keep the release path exercised rather than to be
> depended on, so **every version below has to be spelled out in full**. Cargo
> does not match a prerelease against an ordinary requirement: `wizlight = "0.1"`
> resolves to nothing, and plain `cargo install wizlight` fails, until `0.1.0`
> is out.

## Planned scope

- ~~Discovery by UDP broadcast~~ — done, though the CLI does not yet derive the
  broadcast address from the local interfaces; `--broadcast` names it, and the
  all-subnets default reaches everything on the attached network
- ~~`getPilot` / `setPilot` / `setState` / `getSystemConfig` and friends, as typed
  requests and responses~~ — done
- ~~Bulb model parsing: capabilities, scene support and Kelvin range~~ — done
- A rate-limited streaming path for driving bulbs from live audio or video
- `syncPilot` push updates

**Not planned: RGB ↔ RGB+CW conversion.** A WiZ RGB bulb has five emitters, and
deciding how to spread a colour across them is a judgement call, not a protocol
detail. This crate lets you send `r`/`g`/`b`, or all five channels, and holds no
opinion about which — see [`PilotBuilder`]. `pywizlight`'s "trapezoid" was ported
and measured against raw RGB on hardware before being dropped: it renders a
near-white better, ties or loses elsewhere, and cannot reach some colours at all.
Anything that good is worth an application deciding for itself.

## Library usage

```toml
[dependencies]
wizlight = { version = "0.1.0-alpha.2", default-features = false }
```

The prerelease has to be spelled out in full: a plain `"0.1"` requirement does
not match `0.1.0-alpha.2`, and will keep doing nothing until `0.1.0` ships.

**`default-features = false` matters.** The `cli` feature is on by default so
that `cargo install wizlight` produces a working binary, and it pulls in `clap`,
`anyhow` and `tracing-subscriber`. Library consumers do not want any of those.

```rust,no_run
use std::net::{IpAddr, Ipv4Addr};

use wizlight::protocol::{Channel, Dimming, PilotBuilder};
use wizlight::Bulb;

#[tokio::main]
async fn main() -> Result<(), wizlight::Error> {
    let bulb = Bulb::connect(IpAddr::V4(Ipv4Addr::new(192, 168, 0, 5))).await?;

    let pilot = bulb.get_pilot().await?;
    println!("on={:?}, dimming={:?}", pilot.state, pilot.dimming);

    bulb.set_pilot(
        &PilotBuilder::new()
            .rgb(Channel::new(255), Channel::new(80), Channel::new(0))
            .dimming(Dimming::new(40)?),
    )
    .await?;
    Ok(())
}
```

Every channel value is a valid one, so `Channel::new` cannot fail. The types
that *do* have a range — `Dimming`, `Kelvin`, `Speed`, `Ratio`, `Devices`, and
`SceneId` against the scene table — return a `Result`, because the bulb is not a
reliable validator: it silently clamps an out-of-range `dimming` and reports
success.

Colour, colour temperature and scene are mutually exclusive in one request, and
asking for two of them fails at build time rather than silently picking one:

```rust
use wizlight::protocol::{Channel, Kelvin, PilotBuilder};

let clash = PilotBuilder::new()
    .rgb(Channel::new(255), Channel::new(0), Channel::new(0))
    .temp(Kelvin::new(2700).expect("2700 K is in range"))
    .set_pilot();
assert!(clash.is_err());
```

Scenes — WiZ calls them light modes — are effects the bulb animates by itself,
and the table of them is a `const`, so a scene picker needs no bulb and nothing
to await:

```rust
use wizlight::protocol::{BulbClass, Scene};

// Names are matched ignoring case and punctuation, so the spellings used by
// pywizlight, openHAB and WiZ's own docs all resolve.
let scene = Scene::from_name("deep-dive").expect("Deep dive is a scene");
assert_eq!(scene.id().get(), 23);
assert_eq!(scene.name(), "Deep dive");

// 39 scenes for a colour bulb, 17 for tunable white, 11 for dimmable white.
assert_eq!(Scene::for_class(BulbClass::Tw).count(), 17);

// Animating and taking a `speed` are different questions: Wake up ramps over
// minutes at a rate set in the app, so the builder refuses a `speed` for it.
assert!(scene.animates() && scene.adjustable().speed);
let wake_up = Scene::from_id(9).expect("9 is Wake up");
assert!(wake_up.animates() && !wake_up.adjustable().speed);
```

The table is **measured, not transcribed**: every id was written to an
`ESP25_SHRGB_01` on fw 1.38.0 and read back, and the WiZ app was walked through
mode by mode to name them. The bulb reports `speed` and `dimming` only where the
running scene uses them, which makes both directly observable — and corrects
WiZ's own published table in two places.

A `SceneId` therefore has to be an id the bulb will actually *play*, which is
narrower than what it accepts. `37` sets a colour temperature instead of a
scene, and `42`–`248` are silently clamped to `41`; both answer `success`, and
both are refused here with a message saying what they really do. Custom light
modes made in the app are addressable as user slots:

```rust
use wizlight::protocol::SceneId;

let first_custom_mode = SceneId::user_slot(1).expect("there are ten slots");
assert_eq!(first_custom_mode.get(), 256);

assert!(SceneId::new(37).is_err());   // really sets 2200 K
assert!(SceneId::new(100).is_err());  // really plays 41
```

Bulbs are found by broadcasting, and reported as they answer rather than in a
batch at the end — a discovery run keeps re-broadcasting, so a bulb switched on
halfway through is still found:

```rust,no_run
use std::time::Duration;

use wizlight::Discovery;

#[tokio::main]
async fn main() -> Result<(), wizlight::Error> {
    for bulb in Discovery::new().collect(Duration::from_secs(5)).await? {
        println!("{} at {}", bulb.mac, bulb.addr);
    }
    Ok(())
}
```

Requests are retried and paced to what the hardware can actually absorb: three
attempts, each given 500 ms to be answered, and no two datagrams closer together
than 20 ms — so an unreachable bulb fails in under two seconds. See
[`RetryPolicy`](https://docs.rs/wizlight/latest/wizlight/struct.RetryPolicy.html)
to change that.

## CLI

```console
$ wizlight discover
9877d523a4da  192.168.0.8  ESP25_SHRGB_01  1.38.0
9877d5230f0a  192.168.0.7  ESP25_SHRGB_01  1.38.0

$ wizlight status 9877d5230f0a
on  scene Warm white (11)  2700 K  100%  -49 dBm

$ wizlight on 9877d5230f0a --rgb 255,80,0 --brightness 60
$ wizlight on --all --scene "deep dive" --speed 120
$ wizlight off --all
```

**Address a bulb by MAC, not by IP.** A `<target>` accepts either, but DHCP
moves a bulb's address without warning and the MAC is the only stable identity
the protocol exposes. A MAC costs one short scan to resolve, and it stops as
soon as that bulb answers.

| Command | What it does |
| --- | --- |
| `discover` | Every bulb that answers, with MAC, address, model and firmware |
| `status <target>` | What the bulb says it is doing |
| `info <target>` | Model, firmware, class, Kelvin range and what it can do |
| `scenes <target>` | Only the scenes that bulb's class actually plays |
| `on` / `off` / `toggle` | `on` also takes `--rgb`, `--hsv`, `--kelvin`, `--scene`, `--speed`, `--brightness` |
| `set <target>` | The same options, sent as `setState`. It does **not** leave a bulb that was off alone: measured on `ESP25_SHRGB_01` fw 1.38.0, `setState` turns it on exactly as `setPilot` does |
| `watch` / `bench` | Not yet — they wait on the push listener and the streaming write path |

`--all` replaces the target on any of them and fans out to every bulb a scan
finds, concurrently. One bulb failing does not abort the rest, and does not let
the run exit `0` either.

It always costs the full scan, though — around five seconds — because there is
no way to know that the last bulb has answered. When you know how many to
expect, `--wait 1` is plenty on a quiet network: both bulbs here answer a
broadcast in about 100 ms.

The ways of naming a colour are mutually exclusive, and clap rejects two of them
before anything is sent. `--scene` takes an id or a name, matched ignoring case
and punctuation. Asking a bulb for something it has no hardware for — colour on
a dimmable white — fails with a message naming its class, because the bulb will
not refuse it: it answers `success` and does nothing.

### Output

- **stdout is results, stderr is everything else** — logs, diagnostics and
  errors, including the JSON ones. Redirect stdout and you get data or nothing.
- **`--json` on every command**, with one envelope for both outcomes:

  ```json
  {"ok": true,  "command": "status", "target": "9877d5230f0a", "result": {"state": true, "dimming": 100}}
  {"ok": false, "command": "status", "target": "9877d5230f0a", "error": "no reply from …"}
  ```

  `result` is shaped by the command; under `--all` it is a list of
  `{"target", "ok", "result"}` or `{"target", "ok", "error"}`, one per bulb.
- **Exit codes**: `0` success, `1` failed, `2` usage, `3` nothing answered to
  that target, `4` a bulb was there and stopped answering. The last two are
  separate because they call for different reactions — re-scan, or retry.
- `-v` raises the log level (`-v` info, `-vv` debug, `-vvv` trace) and
  `RUST_LOG` overrides it entirely.
- Colour is dropped when stderr is not a terminal, when `NO_COLOR` is set to
  anything non-empty, and under `--json`.

`--timeout` is how long to wait for each reply before retrying, and `--wait`
is how long a scan lasts. The CLI is deliberately more patient than the
library's default: a bulb at the far end of a flat has a round trip past a
second.

### Installing

From `0.1.0-alpha.3` onward each release carries prebuilt binaries, so a Rust
toolchain is not a prerequisite. Substitute the newest tag from the
[releases page](https://github.com/LucasAmion/wizlight-rs/releases) — the usual
`releases/latest/download/…` URL cannot be used yet, because GitHub does not
count a prerelease as the latest release.

```console
$ curl --proto '=https' --tlsv1.2 -LsSf https://github.com/LucasAmion/wizlight-rs/releases/download/v0.1.0-alpha.3/wizlight-installer.sh | sh
```

On Windows, in PowerShell:

```console
> irm https://github.com/LucasAmion/wizlight-rs/releases/download/v0.1.0-alpha.3/wizlight-installer.ps1 | iex
```

Or with Homebrew, on macOS and Linux:

```console
$ brew install LucasAmion/tap/wizlight
```

Binaries are built for macOS (Apple Silicon and Intel), Linux (x86-64 and
arm64) and Windows (x86-64). The Linux builds are statically linked against
musl, so they carry no glibc requirement and run on Alpine as happily as on
Debian.

From source, with a Rust toolchain, the version has to be spelled out.
`cargo install` resolves `*`, and `*` does not match a prerelease, so the bare
form fails outright rather than finding the alpha:

```console
$ cargo install wizlight
error: could not find `wizlight` in registry `crates-io` with version `*`

$ cargo install wizlight --version 0.1.0-alpha.2
```

Plain `cargo install wizlight` starts working when `0.1.0` ships.

#### macOS: "cannot be verified"

Only if you download an archive from the releases page with a **browser**. The
warning comes from the `com.apple.quarantine` attribute, which browsers set and
`curl`, Homebrew and `cargo` do not — so the commands above never trigger it.
To clear it:

```console
$ xattr -d com.apple.quarantine ./wizlight
```

## Compatibility

- MSRV **1.85** (Rust 2024 edition)
- Tested on Linux, macOS and Windows

## Licence

MIT — see [LICENSE](https://github.com/LucasAmion/wizlight-rs/blob/main/LICENSE).

[pywizlight]: https://github.com/sbidy/pywizlight
