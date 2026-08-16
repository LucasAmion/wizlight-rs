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
- Bulb model parsing: capabilities, scene support and Kelvin range
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
            .rgb(
                Channel::new(255)?,
                Channel::new(80)?,
                Channel::new(0)?,
            )
            .dimming(Dimming::new(40)?),
    )
    .await?;
    Ok(())
}
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

The binary installs and parses the CLI tree, including the global flags and the
stable output contract. The actual bulb operations are still intentionally
stubbed, and the command runner exits non-zero with a clear message while the
protocol layer is implemented. It ships in the alphas so that packaging,
installation and scripting hooks are exercised before the commands land.

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
