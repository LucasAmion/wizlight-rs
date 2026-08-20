# wizlight

An async Rust client for **Philips WiZ** smart bulbs, speaking the local UDP
protocol directly — no cloud, no account, no hub. The crate ships both a library
and a `wizlight` command-line tool.

It is a port of the parts of [`pywizlight`][pywizlight] that matter for
real-time control, and it is the protocol layer underneath
[WiZzard](https://github.com/LucasAmion/wizzard).

> **Status: early development, and the published versions are alphas.** The
> request/response transport, discovery and typed pilot/config methods are in;
> the CLI now has a parser and global flags, but the actual protocol-backed
> commands are still intentionally stubbed and return a non-zero error until the
> rest of the command surface is wired up. The API will change without warning
> until `0.1.0`.
>
> Alphas are published to keep the release path exercised rather than to be
> depended on, so **every version below has to be spelled out in full**. Cargo
> does not match a prerelease against an ordinary requirement: `wizlight = "0.1"`
> resolves to nothing, and plain `cargo install wizlight` fails, until `0.1.0`
> is out.

## Planned scope

- ~~Discovery by UDP broadcast~~ — done, though the CLI does not yet detect the
  broadcast address from the local interfaces
- ~~`getPilot` / `setPilot` / `setState` / `getSystemConfig` and friends, as typed
  requests and responses~~ — done
- ~~Bulb model parsing: capabilities, scene support and Kelvin range~~ — done
- RGB ↔ RGB+CW conversion, cross-checked against `pywizlight`
- A rate-limited streaming path for driving bulbs from live audio or video
- `syncPilot` push updates

## Library usage

```toml
[dependencies]
wizlight = { version = "0.1.0-alpha.1", default-features = false }
```

The prerelease has to be spelled out in full: a plain `"0.1"` requirement does
not match `0.1.0-alpha.1`, and will keep doing nothing until `0.1.0` ships.

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
assert_eq!(scene.name(), Some("Deep dive"));

// 40 scenes for a colour bulb, 17 for tunable white, 11 for dimmable white.
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

## CLI scaffold

The binary installs, parses the command tree and renders output. **No command
does anything to a bulb yet** — every one of them exits 1 with a message saying
so. It ships in the alphas so that packaging and installation are exercised
before the commands land.

What does work is the plumbing around them:

- Results go to stdout, everything else to stderr — logs, diagnostics and
  errors, including the JSON ones. Redirect stdout and you get data or nothing.
- `--json` renders errors as JSON too, so a script never has to parse prose.
  The shape is not stable yet; it settles when the commands do.
- `-v` raises the log level (`-v` info, `-vv` debug, `-vvv` trace) and
  `RUST_LOG` overrides it entirely.
- Colour is dropped when stderr is not a terminal, when `NO_COLOR` is set to
  anything non-empty, and under `--json`.
- Exit codes are 0 for success, 2 for a usage error, and 1 otherwise. The
  distinct codes for *not found* and *timed out* arrive with the commands that
  can produce them.
- `-V`/`--version` reports the crate version, and `-h` and `--help` print the
  same thing.

`--timeout` and `--broadcast` are parsed and logged but nothing consumes them
yet; they are listed here because they are part of the settled surface, not
because they currently do anything.

Installing it needs the version spelled out. `cargo install` resolves `*`, and
`*` does not match a prerelease, so the bare form fails outright rather than
finding the alpha:

```console
$ cargo install wizlight
error: could not find `wizlight` in registry `crates-io` with version `*`

$ cargo install wizlight --version 0.1.0-alpha.1
```

Plain `cargo install wizlight` starts working when `0.1.0` ships.

The shape it is being built towards:

```console
$ wizlight discover
$ wizlight status 192.168.1.42
$ wizlight on 192.168.1.42 --rgb 255,80,0 --brightness 60
$ wizlight watch --all
```

Every command will take `--json` for scripting, and `<target>` will accept
either an IP address or a MAC (resolved through discovery).

## Compatibility

- MSRV **1.85** (Rust 2024 edition)
- Tested on Linux, macOS and Windows

## Licence

MIT — see [LICENSE](https://github.com/LucasAmion/wizlight-rs/blob/main/LICENSE).

[pywizlight]: https://github.com/sbidy/pywizlight
