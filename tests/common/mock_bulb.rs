//! An in-process UDP bulb emulator.
//!
//! Speaks the real WiZ protocol on an ephemeral port so the whole test suite can
//! run on any machine, on all three CI platforms, with no hardware and no fixed
//! ports.
//!
//! # Fidelity
//!
//! Behaviour marked *measured* was recorded from an `ESP25_SHRGB_01` on firmware
//! 1.38.0; the quirks are deliberate, because code written against a
//! better-behaved fake would break on real hardware:
//!
//! - Setting colour, temperature or a scene turns the bulb **on**, even if it
//!   was off, and `setState` behaves exactly like `setPilot` in this. Setting
//!   only `dimming` does not — an off bulb ignores it completely.
//! - An out-of-range `dimming` is **silently clamped** and still reports
//!   success, while a `temp` outside the wire's 1000–12000, a `speed` outside
//!   10–200, or a `sceneId` outside 1–248 is rejected with `-32602`. The bulb
//!   cannot be trusted to validate.
//! - The accepted `sceneId` range is **wider than the scenes that exist**: all
//!   of 1–248 is taken, including the ~200 ids naming nothing, so a `success`
//!   is no evidence a scene ran. `1000` (Rhythm) and the `256..=265` custom
//!   slots are refused, which is the opposite of what `pywizlight` documents.
//! - A `temp` *inside* that wire range but outside the model's own `cctRange`
//!   is accepted, **clamped into the reported range, and read back clamped**:
//!   a bulb reporting 2200–6500 answers `success` to 12000 and then reports
//!   6500. Acceptance is not the same as being honoured, which is why the
//!   usable range comes from `getModelConfig`.
//! - Colour, colour temperature and scene are mutually exclusive: setting one
//!   clears the others.
//! - A `syncPilot` push can be emitted *before* the reply to the request that
//!   caused it. Off by default; see [`MockBulb::push_before_ack`].
//! - A `setPilot` with **no `params` key** is `-32602`, while one with an
//!   **empty `params` object** is `-32600` — a different code for what looks
//!   like the same mistake.
//! - Garbage that is not JSON draws `-32700`, in a reply with no `method`.
//! - `getPower` is **not socket-only**: the RGB personality answers it, always
//!   with `0`, whatever the bulb is doing.
//!
//! Anything else — `ratio` handling, the lower `dimming` bound, and the config
//! responses of the five models we do not own — is taken from `pywizlight` or
//! from the documented parameter ranges and has **not** been confirmed against
//! hardware.
//!
//! `reboot` used to be the sharpest case of that: this harness answered it with
//! an invented `{"success": true}`. It has since been measured, and the truth is
//! neither an acknowledgement nor silence — the bulb **refuses it** with
//! `-32600 Invalid Request`, in every spelling of `params`, and carries on
//! answering without rebooting. Note the code: `-32601 Method not found` would
//! mean the firmware lacked the method, and it does not. `pywizlight` sends
//! `reboot` and ignores the reply, which is presumably why nobody noticed.
//!
//! `reset` is **not** measured and never will be here: it is a factory reset
//! that unpairs the bulb and clears its Wi-Fi credentials. It is assumed to
//! behave like `reboot`, and that assumption is the only guess left in this
//! file's write path.
//!
//! The bulb binds `0.0.0.0`, not `127.0.0.1`: a loopback-bound socket never
//! receives broadcast, which discovery tests depend on.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};
use tokio::net::UdpSocket;
use tokio::task::JoinHandle;

/// The port real bulbs push `syncPilot` updates to.
pub const PUSH_PORT: u16 = 38900;
/// The port real bulbs listen on.
pub const BULB_PORT: u16 = 38899;

/// A model of bulb, with the config responses that model actually returns.
///
/// The RGB personality is our own capture; the others are lifted from
/// `pywizlight`'s recorded device dumps, so the harness can play hardware we do
/// not own.
#[derive(Clone, Copy)]
pub struct Personality {
    /// `moduleName` as reported by `getSystemConfig`, or empty on firmware too
    /// old to report one.
    pub module_name: &'static str,
    /// Firmware version as reported by `getSystemConfig`.
    pub fw_version: &'static str,
    system_config: &'static str,
    model_config: &'static str,
    user_config: &'static str,
    power: Option<&'static str>,
}

const METHOD_NOT_FOUND: &str =
    r#"{"env":"pro","error":{"code":-32601,"message":"Method not found"}}"#;

impl Personality {
    /// `ESP25_SHRGB_01` on firmware 1.38.0 — the hardware everything else was
    /// measured against. Full colour, 2200–6500 K.
    pub fn rgb() -> Self {
        Self {
            module_name: "ESP25_SHRGB_01",
            fw_version: "1.38.0",
            system_config: r#"{"method":"getSystemConfig","env":"pro","result":{"mac":"9877d5230f0a","homeId":19328771,"roomId":32205219,"rgn":"eu","moduleName":"ESP25_SHRGB_01","fwVersion":"1.38.0","groupId":0,"ping":0,"accUdpPropRate":100,"rdIdUidHash":"5f92d6b617a4c3c9e32c744f3e1cd51cbaf5c82c40213fde387838d5014dcdc3"}}"#,
            model_config: r#"{"method":"getModelConfig","env":"pro","result":{"devTotal":1,"headTotal":1,"swHead":0,"ps":3,"hasGradient":1,"nightLightOff":0,"wifiMaxTxPower":18,"minDimLevel":10,"devices":1,"devType":0,"lightType":1,"pwmFreq":1000,"pwmRes":13,"pwmRange":[0,100],"pwmRanges":[0,1000,0,1000,0,1000,0,1000,0,1000],"wcr":80,"nowc":1,"cctRange":[2200,2700,6500,6500],"renderFactor":[255,110,140,255,0,0,40,110,140,240],"wizc1":{"mode":[0,0,0,0,0,0,0]},"wizc2":{"mode":[0,0,0,0,0,0,0]},"drvIface":4,"i2cDrv":[{"chip":"BP5768D","addr":255,"freq":200,"curr":[10,8,6,23,22],"output":[2,1,3,4,5]}],"hasCctTable":16}}"#,
            user_config: r#"{"method":"getUserConfig","env":"pro","result":{"fadeIn":500,"fadeOut":500,"dftDim":100,"opMode":0,"po":false,"minDimming":0,"tapSensor":1,"autoUpd":1,"devices":1,"dim2WarmPoints":[[1800,1],[1800,10],[2700,50],[4200,90],[4200,100]],"wizc1":{"mode":[11,0,0,0,0,0,0],"opts":{"dim":100}},"wizc2":{"mode":[0,255,0,0,0,0,0],"opts":{"dim":100}},"apStkEn":false,"confTs":2}}"#,
            // Measured: this model implements getPower and its meter does not.
            // Zero at full brightness, zero dimmed, zero switched off — so the
            // method answering says nothing about the number being usable.
            power: Some(r#"{"method":"getPower","env":"pro","result":{"power":0}}"#),
        }
    }

    /// `ESP01_SHRGB_03` on 1.25.0 — an older RGB bulb whose `getModelConfig`
    /// carries none of the fields the 1.38.0 firmware added.
    pub fn rgb_legacy() -> Self {
        Self {
            module_name: "ESP01_SHRGB_03",
            fw_version: "1.25.0",
            system_config: r#"{"method":"getSystemConfig","env":"pro","result":{"mac":"a8bb5006033d","homeId":653906,"roomId":989983,"moduleName":"ESP01_SHRGB_03","fwVersion":"1.25.0","groupId":0,"drvConf":[30,1],"ewf":[255,0,255,255,0,0,0],"ewfHex":"ff00ffff000000","ping":0}}"#,
            model_config: r#"{"method":"getModelConfig","env":"pro","result":{"ps":1,"pwmFreq":1000,"pwmRange":[3,100],"wcr":30,"nowc":1,"cctRange":[2200,2700,4800,6500],"renderFactor":[171,255,75,255,43,85,0,0,0,0]}}"#,
            user_config: METHOD_NOT_FOUND,
            power: None,
        }
    }

    /// `ESP14_SHTW1C_01` on 1.18.0 — tunable white. No `getModelConfig` at all,
    /// so the Kelvin range has to come from `getUserConfig`.
    pub fn tunable_white() -> Self {
        Self {
            module_name: "ESP14_SHTW1C_01",
            fw_version: "1.18.0",
            system_config: r#"{"method":"getSystemConfig","env":"pro","result":{"mac":"a8bb503ea5f4","homeId":5385975,"roomId":0,"homeLock":false,"pairingLock":false,"typeId":0,"moduleName":"ESP14_SHTW1C_01","fwVersion":"1.18.0","groupId":0,"drvConf":[20,1]}}"#,
            model_config: METHOD_NOT_FOUND,
            user_config: r#"{"method":"getUserConfig","env":"pro","result":{"fadeIn":450,"fadeOut":500,"fadeNight":false,"dftDim":100,"pwmRange":[0,100],"whiteRange":[2700,6500],"extRange":[2700,6500],"opMode":0,"po":false}}"#,
            power: None,
        }
    }

    /// `ESP06_SHDW9_01` on 1.11.7 — dimmable white only.
    pub fn dimmable_white() -> Self {
        Self {
            module_name: "ESP06_SHDW9_01",
            fw_version: "1.11.7",
            system_config: r#"{"method":"getSystemConfig","env":"pro","result":{"mac":"a8bb509f71d1","homeId":0,"homeLock":false,"pairingLock":false,"typeId":0,"moduleName":"ESP06_SHDW9_01","fwVersion":"1.11.7","groupId":0,"drvConf":[20,1]}}"#,
            model_config: METHOD_NOT_FOUND,
            user_config: r#"{"method":"getUserConfig","env":"pro","result":{"fadeIn":450,"fadeOut":500,"fadeNight":false,"dftDim":100,"pwmRange":[0,100],"whiteRange":[2700,6500],"extRange":[2700,6500]}}"#,
            power: None,
        }
    }

    /// `ESP10_SOCKET_06` on 1.25.0 — a smart plug: on/off, no colour.
    pub fn socket() -> Self {
        Self {
            module_name: "ESP10_SOCKET_06",
            fw_version: "1.25.0",
            system_config: r#"{"method":"getSystemConfig","env":"pro","result":{"mac":"a8bb5006033d","homeId":653906,"roomId":989983,"moduleName":"ESP10_SOCKET_06","fwVersion":"1.25.0","groupId":0,"drvConf":[20,2],"ewf":[255,0,255,255,0,0,0],"ewfHex":"ff00ffff000000","ping":0}}"#,
            model_config: r#"{"method":"getModelConfig","env":"pro","result":{"ps":1,"pwmFreq":200,"pwmRange":[1,100],"wcr":20,"nowc":2,"cctRange":[2700,2700,2700,2700],"renderFactor":[255,0,255,255,0,0,0,0,0,0]}}"#,
            user_config: METHOD_NOT_FOUND,
            power: Some(r#"{"method":"getPower","env":"pro","result":{"power":1065385}}"#),
        }
    }

    /// Replaces the `getSystemConfig` reply.
    ///
    /// For the answers no real model gives — a bulb that reports neither a
    /// `moduleName` nor a `typeId`, or a module name with no identifier in it.
    /// Both come from `pywizlight`'s synthetic fixtures rather than from any
    /// device, so they are supplied at the call site instead of pretending to
    /// be a model.
    #[must_use]
    pub fn with_system_config(mut self, system_config: &'static str) -> Self {
        self.system_config = system_config;
        self
    }

    /// Replaces the `getModelConfig` reply, for the same reason.
    #[must_use]
    pub fn with_model_config(mut self, model_config: &'static str) -> Self {
        self.model_config = model_config;
        self
    }

    /// `ESP03_FANDIMS_31` on 1.31.32 — a ceiling fan with a dimmable white
    /// light, and the only personality whose `getModelConfig` reports a
    /// `fanSpeed`.
    pub fn fan() -> Self {
        Self {
            module_name: "ESP03_FANDIMS_31",
            fw_version: "1.31.32",
            system_config: r#"{"method":"getSystemConfig","env":"pro","result":{"mac":"d8a0119906b7","homeId":5385975,"roomId":8016844,"rgn":"eu","moduleName":"ESP03_FANDIMS_31","fwVersion":"1.31.32","groupId":0,"ping":0}}"#,
            model_config: r#"{"method":"getModelConfig","env":"pro","result":{"ps":1,"pwmFreq":200,"pwmRange":[0,100],"wcr":20,"nowc":1,"cctRange":[2700,2700,2700,2700],"renderFactor":[255,0,255,255,0,0,0,0,0,0],"fanSpeed":6,"wizc1":{"mode":[0,0,0,0,0,0,0]},"wizc2":{"mode":[0,0,0,0,0,0,0]}}}"#,
            user_config: METHOD_NOT_FOUND,
            power: None,
        }
    }

    /// A dimmable white bulb on firmware 1.8.0, which is too old to report a
    /// `moduleName` at all: the class has to come from its `typeId`, and the
    /// white channel count from `drvConf`.
    pub fn firmware_1_8_0() -> Self {
        Self {
            module_name: "",
            fw_version: "1.8.0",
            system_config: r#"{"method":"getSystemConfig","env":"pro","result":{"mac":"a8bb502054e3","homeId":5385975,"homeLock":false,"pairingLock":false,"typeId":0,"fwVersion":"1.8.0","groupId":0,"drvConf":[20,1]}}"#,
            model_config: METHOD_NOT_FOUND,
            user_config: r#"{"method":"getUserConfig","env":"pro","result":{"fadeIn":500,"fadeOut":500,"fadeNight":false,"dftDim":100,"pwmRange":[5,100],"whiteRange":[2700,2700]}}"#,
            power: None,
        }
    }

    /// `ESP20_DHRGB_01` on 1.35.0 — dual head, so `devices` matters.
    pub fn dual_head() -> Self {
        Self {
            module_name: "ESP20_DHRGB_01",
            fw_version: "1.35.0",
            system_config: r#"{"method":"getSystemConfig","env":"pro","result":{"mac":"444f8ec47828","homeId":653906,"roomId":989983,"moduleName":"ESP20_DHRGB_01","fwVersion":"1.35.0","groupId":0,"drvConf":[30,1],"ewf":[255,0,255,255,0,0,0],"ewfHex":"ff00ffff000000","ping":0}}"#,
            model_config: r#"{"method":"getModelConfig","env":"pro","result":{"ps":1,"pwmFreq":2000,"pwmRange":[1,100],"wcr":20,"nowc":2,"cctRange":[2200,2700,6500,6500],"renderFactor":[255,255,170,255,0,0,42,200,255,255]}}"#,
            user_config: METHOD_NOT_FOUND,
            power: None,
        }
    }
}

/// Builds a [`MockBulb`].
pub struct MockBulbBuilder {
    personality: Personality,
    mac: String,
    port: u16,
    push_port: u16,
    pilot: Option<Value>,
}

impl MockBulbBuilder {
    /// Chooses the model this bulb pretends to be.
    pub fn personality(mut self, personality: Personality) -> Self {
        self.personality = personality;
        self
    }

    /// Overrides the MAC, which is how discovery de-duplicates bulbs.
    pub fn mac(mut self, mac: &str) -> Self {
        self.mac = mac.to_owned();
        self
    }

    /// Binds a fixed port instead of an ephemeral one. Only worth doing for
    /// discovery tests, which need the real 38899.
    pub fn port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// Sends push updates somewhere other than the real 38900, so push tests
    /// can bind an ephemeral port and run in parallel.
    pub fn push_port(mut self, port: u16) -> Self {
        self.push_port = port;
        self
    }

    /// Replaces the initial `getPilot` state.
    pub fn pilot(mut self, pilot: Value) -> Self {
        self.pilot = Some(pilot);
        self
    }

    /// Binds the socket and starts serving.
    pub async fn start(self) -> MockBulb {
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, self.port))
            .await
            .expect("bind mock bulb");
        let port = socket.local_addr().expect("local_addr").port();
        let socket = Arc::new(socket);

        let pilot = self.pilot.unwrap_or_else(|| {
            json!({
                "mac": self.mac,
                "rssi": -55,
                "state": true,
                "sceneId": 11,
                "temp": 2700,
                "dimming": 100,
            })
        });

        let model_config = config(self.personality.model_config, &self.mac);
        let user_config = config(self.personality.user_config, &self.mac);
        let shared = Arc::new(Shared {
            mac: self.mac.clone(),
            system_config: config(self.personality.system_config, &self.mac),
            kelvin_range: reported_kelvin_range(&model_config, &user_config),
            model_config,
            user_config,
            power: self.personality.power.map(|p| config(p, &self.mac)),
            state: Mutex::new(State {
                pilot: pilot.as_object().expect("pilot is an object").clone(),
                push_port: self.push_port,
                ..State::default()
            }),
        });

        let task = tokio::spawn(serve(Arc::clone(&socket), Arc::clone(&shared)));

        MockBulb {
            addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
            mac: self.mac,
            shared,
            task,
        }
    }
}

/// A fake bulb listening on UDP.
///
/// Dropping it stops the server.
pub struct MockBulb {
    addr: SocketAddr,
    mac: String,
    shared: Arc<Shared>,
    task: JoinHandle<()>,
}

impl MockBulb {
    /// Starts a full-colour bulb on an ephemeral port.
    pub async fn start() -> Self {
        Self::builder().start().await
    }

    /// Starts configuring a bulb.
    pub fn builder() -> MockBulbBuilder {
        MockBulbBuilder {
            personality: Personality::rgb(),
            mac: "9877d5230f0a".to_owned(),
            port: 0,
            push_port: PUSH_PORT,
            pilot: None,
        }
    }

    /// Where to send requests. Always loopback, whatever the socket is bound to.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The port the bulb is listening on.
    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// The bulb's MAC.
    pub fn mac(&self) -> &str {
        &self.mac
    }

    /// Every datagram received, in order, exactly as it arrived on the wire.
    pub fn requests(&self) -> Vec<String> {
        self.shared.state.lock().unwrap().requests.clone()
    }

    /// The most recent datagram, parsed.
    pub fn last_request(&self) -> Option<Value> {
        self.requests()
            .last()
            .and_then(|raw| serde_json::from_str(raw).ok())
    }

    /// The bulb's current state, as `getPilot` would report it.
    pub fn pilot(&self) -> Value {
        Value::Object(self.shared.state.lock().unwrap().pilot.clone())
    }

    /// Silently discards the next `n` datagrams, for exercising retries and
    /// timeouts.
    pub fn drop_next(&self, n: usize) {
        self.shared.state.lock().unwrap().drop_next = n;
    }

    /// Answers the next `n` requests with something that is not JSON.
    pub fn malformed_next(&self, n: usize) {
        self.shared.state.lock().unwrap().malformed_next = n;
    }

    /// Answers the next `n` requests with this error instead of a result.
    pub fn error_next(&self, n: usize, code: i64, message: &str) {
        let mut state = self.shared.state.lock().unwrap();
        state.error_next = n;
        state.error = (code, message.to_owned());
    }

    /// Delays every reply, for exercising timeouts and slow-bulb behaviour.
    pub fn set_latency(&self, latency: Option<Duration>) {
        self.shared.state.lock().unwrap().latency = latency;
    }

    /// Emits the `syncPilot` push *before* the reply to the request that caused
    /// it — an ordering real hardware does produce.
    pub fn push_before_ack(&self, yes: bool) {
        self.shared.state.lock().unwrap().push_first = yes;
    }

    /// Redirects pushes to another port after the bulb has started, for tests
    /// that only learn the client's port once it exists. Takes effect at the
    /// next registration.
    pub fn set_push_port(&self, port: u16) {
        self.shared.state.lock().unwrap().push_port = port;
    }

    /// Whether a client has registered for push updates, and where they go.
    pub fn push_target(&self) -> Option<SocketAddr> {
        self.shared.state.lock().unwrap().push_target
    }

    /// Sends an unsolicited heartbeat push, as a real bulb does periodically.
    /// Returns `false` if nobody has registered.
    pub async fn push_heartbeat(&self) -> bool {
        let Some(target) = self.push_target() else {
            return false;
        };
        let msg = self.shared.sync_pilot("hb");
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
            .await
            .expect("bind push socket");
        socket
            .send_to(msg.to_string().as_bytes(), target)
            .await
            .expect("send heartbeat");
        true
    }
}

impl Drop for MockBulb {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn config(raw: &str, mac: &str) -> Value {
    let mut value: Value = serde_json::from_str(raw).expect("fixture is valid JSON");
    if let Some(result) = value.get_mut("result").and_then(Value::as_object_mut) {
        if result.contains_key("mac") {
            result.insert("mac".into(), json!(mac));
        }
    }
    value
}

/// The Kelvin range a personality reports, as `setPilot` needs it: the outer
/// bounds of `cctRange`, or of the older `extRange` / `whiteRange`.
fn reported_kelvin_range(model_config: &Value, user_config: &Value) -> Option<(i64, i64)> {
    let bounds = |config: &Value, key: &str| -> Option<(i64, i64)> {
        let values: Vec<i64> = config
            .get("result")?
            .get(key)?
            .as_array()?
            .iter()
            .filter_map(Value::as_i64)
            .collect();
        Some((*values.iter().min()?, *values.iter().max()?))
    };
    bounds(model_config, "cctRange")
        .or_else(|| bounds(user_config, "extRange"))
        .or_else(|| bounds(user_config, "whiteRange"))
}

struct Shared {
    mac: String,
    system_config: Value,
    model_config: Value,
    user_config: Value,
    power: Option<Value>,
    /// What `temp` is clamped into; `None` for a personality that reports no
    /// range at all, which is then left to store whatever it was sent.
    kelvin_range: Option<(i64, i64)>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    pilot: Map<String, Value>,
    requests: Vec<String>,
    push_target: Option<SocketAddr>,
    push_port: u16,
    drop_next: usize,
    malformed_next: usize,
    error_next: usize,
    error: (i64, String),
    latency: Option<Duration>,
    push_first: bool,
}

/// What the bulb decided to do about one datagram.
#[derive(Default)]
struct Reaction {
    latency: Option<Duration>,
    reply: Option<Vec<u8>>,
    push: Option<(SocketAddr, Vec<u8>)>,
    push_first: bool,
}

async fn serve(socket: Arc<UdpSocket>, shared: Arc<Shared>) {
    let mut buf = vec![0u8; 4096];
    loop {
        let Ok((n, from)) = socket.recv_from(&mut buf).await else {
            continue;
        };
        let raw = String::from_utf8_lossy(&buf[..n]).into_owned();
        let reaction = shared.react(&raw, from);
        let socket = Arc::clone(&socket);
        tokio::spawn(async move {
            if let Some(latency) = reaction.latency {
                tokio::time::sleep(latency).await;
            }
            let ack = async {
                if let Some(reply) = &reaction.reply {
                    let _ = socket.send_to(reply, from).await;
                }
            };
            let push = async {
                if let Some((target, msg)) = &reaction.push {
                    let _ = socket.send_to(msg, *target).await;
                }
            };
            if reaction.push_first {
                push.await;
                ack.await;
            } else {
                ack.await;
                push.await;
            }
        });
    }
}

impl Shared {
    fn react(&self, raw: &str, from: SocketAddr) -> Reaction {
        let mut state = self.state.lock().unwrap();
        state.requests.push(raw.to_owned());

        if state.drop_next > 0 {
            state.drop_next -= 1;
            return Reaction::default();
        }
        let latency = state.latency;
        if state.malformed_next > 0 {
            state.malformed_next -= 1;
            return Reaction {
                latency,
                reply: Some(b"garbage".to_vec()),
                ..Reaction::default()
            };
        }

        let Ok(request) = serde_json::from_str::<Value>(raw) else {
            return reply(latency, parse_error());
        };
        let Some(method) = request.get("method").and_then(Value::as_str) else {
            return reply(latency, parse_error());
        };

        if state.error_next > 0 {
            state.error_next -= 1;
            let (code, message) = state.error.clone();
            return reply(latency, error(method, code, &message));
        }

        match method {
            "getPilot" => {
                let result = Value::Object(state.pilot.clone());
                reply(
                    latency,
                    json!({"method": "getPilot", "env": "pro", "result": result}),
                )
            }
            "getSystemConfig" => reply(latency, self.system_config.clone()),
            "getModelConfig" => reply(latency, self.model_config.clone()),
            "getUserConfig" => reply(latency, self.user_config.clone()),
            "getPower" => match &self.power {
                Some(power) => reply(latency, power.clone()),
                None => reply(latency, error(method, -32601, "Method not found")),
            },
            "registration" => {
                let params = request.get("params").cloned().unwrap_or(Value::Null);
                let register = params
                    .get("register")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if register {
                    let ip = params
                        .get("phoneIp")
                        .and_then(Value::as_str)
                        .and_then(|ip| ip.parse::<IpAddr>().ok())
                        .unwrap_or_else(|| from.ip());
                    let push_port = state.push_port;
                    state.push_target = Some(SocketAddr::new(ip, push_port));
                } else {
                    state.push_target = None;
                }
                let ack = json!({
                    "method": "registration",
                    "env": "pro",
                    "result": {"mac": self.mac, "success": true},
                });
                let push = register.then(|| self.push_from(&state, "wizc1"));
                Reaction {
                    latency,
                    reply: Some(ack.to_string().into_bytes()),
                    push: push.flatten(),
                    push_first: state.push_first,
                }
            }
            // Measured: `reboot` is refused with -32600 in every params shape,
            // and the bulb does not reboot. `reset` is untested and assumed to
            // match. See the fidelity notes at the top of the module.
            "reboot" | "reset" => reply(latency, error(method, -32600, "Invalid Request")),
            "setPilot" | "setState" => {
                match apply(&mut state.pilot, request.get("params"), self.kelvin_range) {
                    Err((code, message)) => reply(latency, error(method, code, &message)),
                    Ok(()) => {
                        let ack = json!({
                            "method": method,
                            "env": "pro",
                            "result": {"success": true},
                        });
                        Reaction {
                            latency,
                            reply: Some(ack.to_string().into_bytes()),
                            push: self.push_from(&state, "udp"),
                            push_first: state.push_first,
                        }
                    }
                }
            }
            other => reply(latency, error(other, -32601, "Method not found")),
        }
    }

    fn push_from(&self, state: &State, src: &str) -> Option<(SocketAddr, Vec<u8>)> {
        let target = state.push_target?;
        let mut params = state.pilot.clone();
        params.insert("devices".into(), json!(1));
        params.insert("src".into(), json!(src));
        if src == "hb" {
            params.insert("mqttCd".into(), json!(0));
            params.insert("ts".into(), json!(unix_now()));
        }
        let msg = json!({"method": "syncPilot", "env": "pro", "params": params});
        Some((target, msg.to_string().into_bytes()))
    }

    fn sync_pilot(&self, src: &str) -> Value {
        let state = self.state.lock().unwrap();
        let mut params = state.pilot.clone();
        params.insert("devices".into(), json!(1));
        params.insert("src".into(), json!(src));
        if src == "hb" {
            params.insert("mqttCd".into(), json!(0));
            params.insert("ts".into(), json!(unix_now()));
        }
        json!({"method": "syncPilot", "env": "pro", "params": params})
    }
}

/// Applies `setPilot` params to the current state, or explains why the bulb
/// refused.
///
/// `kelvin_range` is the model's own reported range, which is what a `temp`
/// gets clamped into — a different bound from the one the wire accepts.
fn apply(
    pilot: &mut Map<String, Value>,
    params: Option<&Value>,
    kelvin_range: Option<(i64, i64)>,
) -> Result<(), (i64, String)> {
    let invalid = || (-32602, "Invalid params".to_owned());
    let params = params.and_then(Value::as_object).ok_or_else(invalid)?;
    // Measured: an absent `params` key and an empty one are not the same
    // request. `{"method":"setPilot"}` is invalid *params*; adding `"params":{}`
    // makes it an invalid *request*.
    if params.is_empty() {
        return Err((-32600, "Invalid Request".to_owned()));
    }

    // Validate everything before touching the state: a rejected request must
    // leave the bulb exactly as it was.
    for (key, value) in params {
        match key.as_str() {
            "state" => {
                value.as_bool().ok_or_else(invalid)?;
            }
            "r" | "g" | "b" | "c" | "w" => {
                let v = value.as_i64().ok_or_else(invalid)?;
                if !(0..=255).contains(&v) {
                    return Err(invalid());
                }
            }
            // Measured: 200 comes back as success and is clamped to 100.
            "dimming" => {
                value.as_i64().ok_or_else(invalid)?;
            }
            // Measured: the wire bound is 1000-12000, and 999 or 15000 are
            // refused. Anything inside it is accepted whatever the model's own
            // range says, and clamped into it below.
            "temp" => {
                let v = value.as_i64().ok_or_else(invalid)?;
                if !(1000..=12_000).contains(&v) {
                    return Err(invalid());
                }
            }
            // Measured: a contiguous 1-248, scanned end to end. Note what that
            // is *not* — most of those ids name no scene, and the bulb takes
            // them anyway, so this is a range check and not a scene table. `0`
            // is refused on a write while being reported on a read, and both
            // `1000` (Rhythm) and the `256..=265` custom slots are refused,
            // which is where the ranges inherited from `pywizlight` were wrong.
            "sceneId" => {
                let v = value.as_i64().ok_or_else(invalid)?;
                if !(1..=248).contains(&v) {
                    return Err(invalid());
                }
            }
            // Measured: 9 and 201 are refused, 10 and 200 accepted. Unlike
            // `dimming`, the bulb enforces this one. Whether it also refuses a
            // `speed` with no animating scene behind it was not tested, so
            // nothing here pretends to know.
            "speed" => {
                let v = value.as_i64().ok_or_else(invalid)?;
                if !(10..=200).contains(&v) {
                    return Err(invalid());
                }
            }
            // Not measured; passed through unvalidated rather than guessed at.
            _ => {}
        }
    }

    let colour = ["r", "g", "b", "c", "w"];
    let lit = pilot.get("state").and_then(Value::as_bool).unwrap_or(false);
    let activating = colour.iter().any(|k| params.contains_key(*k))
        || params.contains_key("temp")
        || params.contains_key("sceneId");

    // Measured: a bulb that is off ignores anything that does not either name a
    // new state or imply one. `{"dimming":55}` sent to an off bulb reports
    // success and changes nothing at all — not even the stored brightness.
    if !lit && !activating && !params.contains_key("state") {
        return Ok(());
    }
    // Measured: colour, temperature and scene all switch the bulb on.
    if activating {
        pilot.insert("state".into(), json!(true));
    }

    if colour.iter().any(|k| params.contains_key(*k)) {
        pilot.remove("temp");
        pilot.insert("sceneId".into(), json!(0));
        for key in colour {
            pilot.entry(key.to_string()).or_insert(json!(0));
        }
    }
    if params.contains_key("temp") {
        for key in colour {
            pilot.remove(key);
        }
        pilot.insert("sceneId".into(), json!(0));
    }
    if let Some(scene) = params.get("sceneId").and_then(Value::as_i64) {
        if scene != 0 {
            for key in colour {
                pilot.remove(key);
            }
            pilot.remove("temp");
        }
    }

    for (key, value) in params {
        let value = match (key.as_str(), kelvin_range) {
            ("dimming", _) => json!(value.as_i64().unwrap_or(100).clamp(1, 100)),
            // Measured on ESP25_SHRGB_01 fw 1.38.0, whose reported cctRange is
            // 2200-6500: `temp` is clamped into that range in both directions
            // and reported back clamped — 1000 reads as 2200, 9000 and 12000
            // read as 6500. So a `success` says nothing about the bulb having
            // honoured the temperature, and the range worth knowing is the one
            // getModelConfig reports rather than the one the wire takes.
            ("temp", Some((min, max))) => json!(value.as_i64().unwrap_or(min).clamp(min, max)),
            _ => value.clone(),
        };
        pilot.insert(key.clone(), value);
    }
    Ok(())
}

fn reply(latency: Option<Duration>, body: Value) -> Reaction {
    Reaction {
        latency,
        reply: Some(body.to_string().into_bytes()),
        ..Reaction::default()
    }
}

fn error(method: &str, code: i64, message: &str) -> Value {
    json!({
        "method": method,
        "env": "pro",
        "error": {"code": code, "message": message},
    })
}

/// The bulb's answer to something that is not a request at all. Measured: the
/// reply carries no `method`, because it never got far enough to find one.
fn parse_error() -> Value {
    json!({"env": "pro", "error": {"code": -32700, "message": "Parse error"}})
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
