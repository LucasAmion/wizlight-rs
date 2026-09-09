# WiZ local UDP protocol

This document describes the protocol surface implemented by `wizlight`. It is
not a complete description of every WiZ product or firmware version.

Statements marked **measured** were observed on two `ESP25_SHRGB_01` bulbs
running firmware 1.38.0. Model tables and older-firmware behavior inherited from
[`pywizlight`](https://github.com/sbidy/pywizlight) are marked **unverified**
where no matching hardware was available. A successful reply only means that a
packet was accepted; several parameters are silently clamped or ignored.

## Transport and envelopes

Bulbs listen for UTF-8 JSON datagrams over UDP port **38899**. Requests have a
method and a params object:

```json
{"method":"getPilot","params":{}}
{"method":"setPilot","params":{"r":255,"g":80,"b":0,"dimming":40}}
```

Replies carry either `result` or `error`:

```json
{"method":"getPilot","env":"pro","result":{"state":true,"dimming":40}}
{"method":"setPilot","env":"pro","result":{"success":true}}
{"method":"setPilot","env":"pro","error":{"code":-32602,"message":"Invalid params"}}
```

Unknown reply fields must be ignored. There is no request identifier that can
correlate concurrent calls. The `id` used by discovery is inside the
`registration` params and is not echoed by the bulb. A socket must therefore
serialize requests with the same method; otherwise one call can consume
another call's reply.

Observed JSON-RPC-style errors:

| Code | Meaning | Notes |
| ---: | --- | --- |
| `-32700` | Parse error | The reply may have no `method`. |
| `-32601` | Method not found | Used when firmware does not implement a method. |
| `-32602` | Invalid params | Missing, out-of-range, or unsupported params. |
| `-32600` | Invalid Request | For example, an empty `setPilot` params object. |

Key order inside `params` made no difference in measured traffic.

## Discovery

Discovery sends this packet to an IPv4 broadcast address on port 38899:

```json
{"method":"registration","params":{"phoneMac":"AAAAAAAAAAAA","register":false,"phoneIp":"<local-ip>","id":"1"}}
```

A WiZ device answers with `result.mac`. Anything without that field is not a
discovered bulb. The sender should bind an ephemeral port: bulbs reply to the
source port, and this permits concurrent scans. Binding the WiZ port itself can
also receive the sender's own broadcast.

The library default is the limited broadcast `255.255.255.255:38899`. The CLI
derives the directed broadcast address of every viable local IPv4 subnet and
lets repeatable `--broadcast` flags override those targets.

A scan repeats the packet once per second and normally waits five seconds.
This is not defensive excess: **measured** over twenty broadcasts, the two test
bulbs answered 19/20 and 11/20, with one missing four consecutive broadcasts,
while both answered spaced unicast requests without loss. Results should be
reported as they arrive and deduplicated by MAC, not by DHCP address.

`getSystemConfig` may be requested after each reply to obtain model and firmware
information. Failure of that follow-up does not make the discovery reply cease
to be a bulb.

`register: false` has a global side effect at the bulb. **Measured:** discovery
clears an existing push registration even when it was created from a different
socket. Call `PushManager::refresh()` after a scan that overlaps active push
subscriptions.

## Methods

| Method | Purpose | Request params | Typed result |
| --- | --- | --- | --- |
| `getPilot` | Read current state | Empty, or zero-based `devices` for a head | `Pilot` |
| `setPilot` | Apply state | Pilot fields below | `success` |
| `setState` | Apply state | Same as `setPilot` | `success` |
| `getSystemConfig` | Identity, firmware, legacy driver config | Empty | `SystemConfig` |
| `getModelConfig` | Modern capability ranges | Empty | `ModelConfig` |
| `getUserConfig` | User and older-firmware white ranges | Empty | `UserConfig` |
| `getPower` | Firmware-defined power reading | Empty | `Power` |
| `reboot` | Request a reboot | Empty | `success`, refusal, or silence |
| `reset` | Request a factory reset | Empty | `success`, refusal, or silence |
| `registration` | Discovery or push registration | Registration fields | Discovery result or `success` |
| `syncPilot` | Unsolicited state update | MAC and pilot state | No reply |
| `firstBeat` | Unsolicited boot/reconnect announcement | MAC | No reply |

### `getPilot`

The result is partial and firmware-dependent. Known fields are:

| Field | Type | Meaning |
| --- | --- | --- |
| `state` | boolean | Power state. |
| `r`, `g`, `b` | integer | RGB emitters. |
| `c`, `w` | integer | Cold- and warm-white emitters. |
| `dimming` | integer | Brightness, normally 1–100. An off bulb may report 0. |
| `temp` | integer | Active colour temperature in Kelvin. |
| `sceneId` | integer | Active scene; 0 means no scene. |
| `speed` | integer | Scene speed, only reported when applicable. |
| `ratio` | integer | Dual-head balance. |
| `devices` | integer | One-based head tag in the reply. |
| `rssi` | integer | Received signal strength. |

For a per-head read, `devices` uses a different convention from writes.
**Measured on a single-head bulb:** querying `devices: 0` succeeds and the reply
contains `devices: 1`; querying 1, 2, or 3 is refused.

### `setPilot` and `setState`

Both methods accept the same fields. **Measured:** either one turns an off bulb
on when the request contains RGB/white channels, `temp`, or `sceneId`.
`setState` is not a non-waking variant. A `dimming`-only write to an off bulb is
discarded.

The color modes are mutually exclusive:

1. raw channels (`r`/`g`/`b`/`c`/`w`);
2. colour temperature (`temp`);
3. bulb-side scene (`sceneId`).

Power, dimming, speed, ratio, and device selection may accompany a color mode.

| Param | Client range | Measured wire behavior |
| --- | ---: | --- |
| `state` | boolean | Switches power. |
| `r`, `g`, `b`, `c`, `w` | `0..=255` | Values larger than a byte are truncated by the measured bulb. |
| `dimming` | `1..=100` | Any byte is accepted, then silently clamped to 1–100. `0` is not off. |
| `speed` | `10..=200` | `9` and `201` are refused with `-32602`. |
| `temp` | `1000..=12000` | Values outside the model's usable CCT range are silently clamped. |
| `ratio` | `0..=100` | Accepted and ignored by the measured single-head bulb. |
| `sceneId` | playable scene or user slot | See [Scenes](#scenes). |
| `devices` | `1..=3` | Write-side selector: first head, second head, or all heads. |

The five raw channels form **one replacing instruction**. Omitted channels go
dark; `c: 64` does not add cold white to the RGB already showing. Send every
wanted channel in one packet. An all-zero channel group is discarded, and can
still be acknowledged if another field in the request succeeds.

A model's usable Kelvin range comes from `getModelConfig.cctRange`, falling back
to `getUserConfig.extRange` and then `whiteRange`. The measured RGB bulb reports
2200–6500 K but accepts the wire endpoints 1000 and 12000, reading them back as
2200 and 6500. The broad wire range and the useful hardware range answer
different questions.

### Config and maintenance methods

`getSystemConfig` supplies `mac`, `moduleName`, `fwVersion`, and optional legacy
fields including `typeId` and `drvConf`. On pre-1.22 firmware, `drvConf` is
`[white_to_color_ratio, white_channel_count]`.

`getModelConfig` is the modern source of `wcr`, `nowc`, `cctRange`, `fanSpeed`,
and device count. Older firmware may answer `-32601`; `getUserConfig` then
supplies `extRange` or `whiteRange`.

`getPower` is model-specific. **Measured:** `ESP25_SHRGB_01` implements it but
always returned zero, whether on or off. Treat its unit and usefulness as
unverified for every model.

`reboot` was **measured** as implemented but refused with `-32600` on firmware
1.38.0. `reset` is deliberately **unmeasured** because a successful factory
reset clears pairing and Wi-Fi credentials. Both calls treat silence as
possible success: a device that actually rebooted cannot acknowledge afterward,
while an explicit error remains a failure.

## `moduleName` and capabilities

The grammar is:

```text
<family>_<identifier>[_<revision>]
ESP25_SHRGB_01
^^^^^ ^^^^^ ^^
  |     |    revision
  |     head count and class
  module family
```

The identifier contains a head marker (`SH` or `DH`) and a class marker:

| Marker | Class |
| --- | --- |
| `RGB` | Full colour plus tunable white |
| `TW` | Tunable white |
| no recognized marker | Dimmable white |
| `SOCKET` | On/off socket |
| `FANDIM` | Fan with dimmable light |
| `DDTW` | Fan with tunable-white light |

Classification follows the crate's ordered identifier checks; in particular,
`DDTW` must be tested before `TW` because it contains the shorter marker.
Firmware before 1.9 may omit `moduleName`; known `typeId` values are then a fallback. Unknown type IDs are conservatively treated
as dimmable white and reported as assumed rather than measured.

Only `ESP25_SHRGB_01` has been checked against hardware here. Other model
mappings and per-class scene availability are inherited from `pywizlight` and
WiZ's published table and remain **unverified**.

## Scenes

A scene is an effect run by the bulb. One packet starts it:

```json
{"method":"setPilot","params":{"sceneId":4,"speed":100}}
```

No continuing color traffic is required. `dimming` remains independent for all
but Wake up, Night light, and Alarm, which **measured** as accepting and ignoring
it. Party remained smooth and retained its scene and speed while 138
dimming-only packets were sent over 15 seconds.

The crate names 39 sendable scenes: ids `1..=36` and `38..=40`. Scene behavior
was measured on RGB hardware; TW/DW availability is inherited and unverified.

| ID | Name | Category | Classes | Speed | Dimming | Kelvin |
| ---: | --- | --- | --- | :---: | :---: | ---: |
| 1 | Ocean | Dynamic | RGB | yes | yes | — |
| 2 | Romance | Dynamic | RGB | yes | yes | — |
| 3 | Sunset | Dynamic | RGB | yes | yes | — |
| 4 | Party | Dynamic | RGB | yes | yes | — |
| 5 | Fireplace | Dynamic | RGB | yes | yes | — |
| 6 | Cozy | Functional | RGB, TW | no | yes | — |
| 7 | Forest | Dynamic | RGB | yes | yes | — |
| 8 | Pastel colors | Dynamic | RGB | yes | yes | — |
| 9 | Wake up | Progressive | RGB, TW, DW | no | no | — |
| 10 | Bedtime | Progressive | RGB, TW, DW | no | yes | — |
| 11 | Warm white | White | RGB, TW | no | yes | 2700 |
| 12 | Daylight | White | RGB, TW | no | yes | 4200 |
| 13 | Cool white | White | RGB, TW, DW | no | yes | 6500 |
| 14 | Night light | Functional | RGB, TW, DW | no | no | — |
| 15 | Focus | Functional | RGB, TW | no | yes | — |
| 16 | Relax | Functional | RGB, TW | no | yes | — |
| 17 | True colors | Functional | RGB | no | yes | — |
| 18 | TV time | Functional | RGB, TW | no | yes | — |
| 19 | Plant growth | Functional | RGB | no | yes | — |
| 20 | Spring | Dynamic | RGB | yes | yes | — |
| 21 | Summer | Dynamic | RGB | yes | yes | — |
| 22 | Fall | Dynamic | RGB | yes | yes | — |
| 23 | Deep dive | Dynamic | RGB | yes | yes | — |
| 24 | Jungle | Dynamic | RGB | yes | yes | — |
| 25 | Mojito | Dynamic | RGB | yes | yes | — |
| 26 | Club | Dynamic | RGB | yes | yes | — |
| 27 | Christmas | Dynamic | RGB | yes | yes | — |
| 28 | Halloween | Dynamic | RGB | yes | yes | — |
| 29 | Candlelight | Dynamic | RGB, TW, DW | no | yes | — |
| 30 | Golden white | Dynamic | RGB, TW, DW | yes | yes | — |
| 31 | Pulse | Dynamic | RGB, TW, DW | yes | yes | — |
| 32 | Steampunk | Dynamic | RGB, TW, DW | yes | yes | — |
| 33 | Diwali | Dynamic | RGB, TW, DW | yes | yes | — |
| 34 | White | White | RGB, DW | no | yes | 4000 |
| 35 | Alarm | Dynamic | RGB, TW, DW | no | no | — |
| 36 | Snowy sky | Dynamic | RGB | yes | yes | — |
| 38 | Soft white | White | RGB | no | yes | 3500 |
| 39 | Crisp white | White | RGB | no | yes | 5000 |
| 40 | Dim-to-warm | White | RGB, TW | no | yes | varies |

Every id through 41 was written and read back on the measured RGB hardware:

- `37` leaves scene mode and selects 2200 K;
- `38` and `39` are unnamed static whites measured at 3500 K and 5000 K, named
  “Soft white” and “Crisp white” by this crate;
- `41` produces roughly 6200 K at about one third of the normal brightness scale;
- `42..=248` are accepted and silently clamp to 41;
- `256..=265` are user slots and work only after the WiZ app has saved a custom
  mode into that slot;
- `1000` (“Rhythm” in some third-party code) was refused.

`SceneId` validates what is worth sending rather than what the wire accepts.
Scene names, app categories, animation behavior, and whether `speed` or
`dimming` applies are available through `Scene` and `Adjustable`.

Per-class scene availability remains **unverified** without TW or DW hardware.
The current table yields 39 scenes for RGB, 17 for tunable white, and 11 for
dimmable white.

## Raw RGB and white emitters

RGB bulbs have red, green, blue, cold-white, and warm-white emitters sharing a
power budget. The protocol exposes all five directly. It does not define an RGB
to RGB+CW conversion, and this crate intentionally does not invent one.

`pywizlight`'s trapezoid conversion was ported and compared by eye against raw
RGB before being removed. **Measured:** it improved a near-white, but destroyed
some pale colors by discarding two primaries; a fixed white emitter also washed
out colors far from that emitter's temperature. Both white emitters at a fixed
50/50 split lost to selecting the nearer emitter in four of five comparisons.
Those observations belong in application-level color mixing, not the transport
crate.

This is distinct from `temp`: raw `c`/`w` values choose emitter levels directly,
possibly alongside RGB, while `temp` selects the bulb's separate CCT mode and
lets firmware mix the white emitters.

## Push updates

Push traffic uses UDP port **38900**. Register one bulb by sending:

```json
{"method":"registration","params":{"phoneMac":"AAAAAAAAAAAA","register":true,"phoneIp":"<listener-ip>"}}
```

The registration must be renewed. The measured implementation uses a 20-second
keepalive. Send the same packet with `register: false` to unsubscribe.

The bulb sends unsolicited messages including:

- `syncPilot`, carrying a MAC and pilot state;
- `firstBeat`, announcing a bulb after boot or reconnect;
- literal `test` datagrams, which should be ignored.

One process-wide listener routes events by normalized MAC into bounded
per-bulb subscriptions. A slow subscriber must not block other bulbs; later
events may be dropped when its buffer is full. Only one process can normally
own port 38900, so failure to bind is a typed reason to fall back to polling.

The advertised `phoneIp` must be the source address selected by the route toward
the target bulb, especially on multi-homed hosts. Discovery can cancel these
registrations; refresh them after scanning.

## Rate limiting

Reliable requests and real-time streaming intentionally use different paths.

`Bulb::request()` retries and awaits a matching response. **Measured:** requests
spaced by at least 20 ms had no loss; an unpaced burst lost 78%. Round trips were
p50 101 ms, p99 173 ms, and at most 235 ms in that run. The default policy uses
a 20 ms network pace, a 500 ms attempt timeout, and three attempts. One exchange
runs at a time because replies have no correlation id.

`Bulb::stream()` sends fire-and-forget `setPilot` updates with no retries. It has
a capacity-one pending slot: a newer frame replaces a stale unsent one, so a
producer never queues behind the network. Shutdown flushes the newest frame.

**Hardware measurement still required:** the maximum sustained update rate
before visible stutter or loss, packet-to-photon latency, and whether bulbs
interpolate internally have not been measured. The default 50 ms interval
(20 Hz) is a working placeholder, not a hardware result. `wizlight bench` will
fill in these values and replace the placeholder.
