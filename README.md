# wizlight

An async Rust client for **Philips WiZ** smart bulbs, speaking the local UDP
protocol directly — no cloud, no account, no hub. The crate ships both a library
and a `wizlight` command-line tool.

It is a port of the parts of [`pywizlight`][pywizlight] that matter for
real-time control, and it is the protocol layer underneath
[WiZzard](https://github.com/LucasAmion/wizzard).

> **Status: early development.** The request/response transport is in; the rest
> of the list below is not, and the CLI has no commands yet. The API is unstable
> until `0.1.0` is published.

## Planned scope

- Discovery by UDP broadcast, with the broadcast address detected from the local
  interfaces rather than hardcoded
- `getPilot` / `setPilot` / `setState` / `getSystemConfig` and friends, as typed
  requests and responses
- Bulb model parsing: capabilities, scene support and Kelvin range
- RGB ↔ RGB+CW conversion, cross-checked against `pywizlight`
- A rate-limited streaming path for driving bulbs from live audio or video
- `syncPilot` push updates

## Library usage

```toml
[dependencies]
wizlight = { version = "0.1", default-features = false }
```

**`default-features = false` matters.** The `cli` feature is on by default so
that `cargo install wizlight` produces a working binary, and it pulls in `clap`,
`anyhow` and `tracing-subscriber`. Library consumers do not want any of those.

```rust,no_run
use std::net::{IpAddr, Ipv4Addr};

use wizlight::{Bulb, Request};

#[tokio::main]
async fn main() -> Result<(), wizlight::Error> {
    let bulb = Bulb::connect(IpAddr::V4(Ipv4Addr::new(192, 168, 0, 5))).await?;
    let pilot = bulb.request(&Request::new("getPilot")).await?;
    println!("{:?}", pilot.result);
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
$ cargo install wizlight
$ wizlight discover
$ wizlight status 192.168.1.42
$ wizlight on 192.168.1.42 --rgb 255,80,0 --brightness 60
$ wizlight watch --all
```

Every command takes `--json` for scripting, and `<target>` accepts either an IP
address or a MAC (resolved through discovery).

## Compatibility

- MSRV **1.85** (Rust 2024 edition)
- Tested on Linux, macOS and Windows

## Licence

MIT — see [LICENSE](https://github.com/LucasAmion/wizlight-rs/blob/main/LICENSE).

[pywizlight]: https://github.com/sbidy/pywizlight
