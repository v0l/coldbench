//! Blockstream Jade over USB serial.
//!
//! The protocol is CBOR maps, one request and one reply, written straight to the serial port. The
//! only complication is the PIN: `auth_user` makes the Jade show a PIN pad, and then answers with
//! `http_request`, asking the host to relay a few messages to Blockstream's blind pin server. The
//! relay is a plain HTTPS POST of whatever the Jade says, so there is nothing here to trust or to
//! break. While the person types the PIN the read blocks with no limit, which is why the reader
//! polls [`Prompt::cancelled`].

use crate::{Error, Found, Info, Kind, Notice, Prompt, Result, Script, Signer, Silent};
use bitcoin::bip32::{DerivationPath, Xpub};
use bitcoin::{Network, Psbt};
use ciborium::Value;
use serialport::{ClearBuffer, SerialPort, SerialPortType};
use std::io::{Read, Write};
use std::str::FromStr;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// USB ids of the bridge chips Jade boards (and a lot of other boards) use.
const IDS: [(u16, u16); 6] = [
    (0x10c4, 0xea60),
    (0x1a86, 0x55d4),
    (0x0403, 0x6001),
    (0x1a86, 0x7523),
    (0x303a, 0x4001),
    (0x303a, 0x1001),
];

const BAUD: u32 = 115_200;
const POLL: Duration = Duration::from_millis(150);
/// How long a call that needs no person takes to answer, on a healthy device.
const QUICK: Duration = Duration::from_secs(4);
/// How long to wait for a stranger on a shared serial bridge to say what it is.
const PROBE: Duration = Duration::from_millis(1200);
/// Oldest firmware with native PSBT signing.
const MIN_PSBT_FW: (u32, u32, u32) = (0, 1, 47);

const USER_CANCELLED: i64 = -32000;
const HW_LOCKED: i64 = -32002;
const NETWORK_MISMATCH: i64 = -32003;
const BAD_PARAMETERS: i64 = -32602;

/// Ports already asked, and whether a Jade answered. Each port is probed once while it stays
/// plugged in, so listing devices on a timer does not keep poking other boards that use the same
/// USB serial chips, or a Jade that is busy with a job.
static SEEN: Mutex<Vec<(String, bool)>> = Mutex::new(Vec::new());

pub fn enumerate() -> Vec<Found> {
    let Ok(ports) = serialport::available_ports() else { return Vec::new() };
    let present: Vec<String> = ports
        .iter()
        .filter(|p| matches!(&p.port_type, SerialPortType::UsbPort(u) if IDS.contains(&(u.vid, u.pid))))
        .map(|p| p.port_name.clone())
        .collect();
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    seen.retain(|(p, _)| present.contains(p));
    let mut found = Vec::new();
    for port in present {
        let known = seen.iter().find(|(p, _)| *p == port).map(|(_, j)| *j);
        let is_jade = match known {
            Some(j) => j,
            None => match probe(&port) {
                Some(j) => {
                    seen.push((port.clone(), j));
                    j
                }
                // Busy or unreadable: ask again next time.
                None => false,
            },
        };
        if is_jade {
            found.push(Found { kind: Kind::Jade, id: port.clone(), name: format!("Jade on {port}") });
        }
    }
    found
}

/// Is a Jade on `port`? `None` when the port could not be asked.
fn probe(port: &str) -> Option<bool> {
    let mut link = Link::connect(port).ok()?;
    match link.rpc(&Silent, "get_version_info", nonblocking(), Some(PROBE)) {
        Ok(v) => Some(text_of(get(&v, "BOARD_TYPE")).unwrap_or_default().to_uppercase().contains("JADE")),
        // Something answered, but not in CBOR: another kind of board.
        Err(Error::Protocol(_)) => Some(false),
        Err(Error::Io(m)) if m.contains("did not answer") => Some(false),
        Err(_) => None,
    }
}

fn nonblocking() -> Option<Value> {
    Some(map([("nonblocking", Value::Bool(true))]))
}

pub struct Jade {
    link: Link,
    info: Info,
    network: &'static str,
}

impl Jade {
    pub fn open(port: &str, network: Network, prompt: &dyn Prompt) -> Result<Jade> {
        let name = net_name(network)?;
        let mut link = Link::connect(port)?;
        let v = link.rpc(prompt, "get_version_info", nonblocking(), Some(QUICK))?;
        let state = text_of(get(&v, "JADE_STATE")).unwrap_or_default();
        let version = text_of(get(&v, "JADE_VERSION")).unwrap_or_default();
        let board = text_of(get(&v, "BOARD_TYPE")).unwrap_or_else(|| "JADE".into());
        let nets = text_of(get(&v, "JADE_NETWORKS")).unwrap_or_default();
        if state == "UNINIT" || state == "UNSAVED" {
            return Err(Error::Uninitialised);
        }
        let wants_main = name == "mainnet";
        if (nets == "MAIN" && !wants_main) || (nets == "TEST" && wants_main) {
            return Err(Error::Network(format!(
                "this Jade is set to {} only, change it in the Jade's settings",
                if nets == "MAIN" { "mainnet" } else { "testnet" }
            )));
        }
        if parse_version(&version) < MIN_PSBT_FW {
            return Err(Error::Unsupported(format!(
                "Jade firmware {version} is too old to sign PSBTs, update it to 0.1.47 or newer"
            )));
        }

        // `ping` is answered at once even when the Jade is in the middle of something. A busy Jade
        // is almost always sitting at a PIN pad that an earlier session left open.
        let busy = !matches!(
            link.rpc(prompt, "ping", None, Some(QUICK))?.as_integer().map(i128::from),
            Some(0)
        );
        if busy {
            prompt.notify(Notice::PinOnDevice);
        }
        let mut entropy = [0u8; 32];
        fill_random(&mut entropy);
        let params = map([("entropy", Value::Bytes(entropy.to_vec()))]);
        link.rpc(prompt, "add_entropy", Some(params), if busy { None } else { Some(QUICK) })?;

        // Unlock. A Jade that is already unlocked answers true at once.
        let mut wrong = false;
        loop {
            if prompt.cancelled() {
                return Err(Error::cancelled());
            }
            prompt.notify(match (wrong, state == "LOCKED") {
                (true, _) => Notice::Confirm(
                    "wrong PIN, try again on the Jade (it erases itself after too many)".into(),
                ),
                (false, true) => Notice::PinOnDevice,
                (false, false) => Notice::Working,
            });
            let epoch =
                SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            let params = map([("network", text(name)), ("epoch", Value::from(epoch))]);
            match link.rpc(prompt, "auth_user", Some(params), None)? {
                Value::Bool(true) => break,
                Value::Bool(false) => wrong = true,
                other => return Err(Error::Protocol(format!("auth_user answered {other:?}"))),
            }
        }
        prompt.notify(Notice::Working);

        let params = map([("network", text(name)), ("path", path_value(&[0x8000_0000]))]);
        let fingerprint = xpub_of(&link.rpc(prompt, "get_xpub", Some(params), Some(QUICK))?)?
            .parent_fingerprint;

        let info = Info {
            kind: Kind::Jade,
            model: board,
            firmware: version,
            fingerprint,
            label: None,
            network,
        };
        Ok(Jade { link, info, network: name })
    }
}

impl Signer for Jade {
    fn info(&self) -> &Info {
        &self.info
    }

    fn xpub(&mut self, path: &DerivationPath, prompt: &dyn Prompt) -> Result<Xpub> {
        prompt.notify(Notice::Working);
        let params = map([
            ("network", text(self.network)),
            ("path", path_value(&child_numbers(path))),
        ]);
        xpub_of(&self.link.rpc(prompt, "get_xpub", Some(params), Some(QUICK))?)
    }

    fn show_address(
        &mut self,
        script: Script,
        path: &DerivationPath,
        prompt: &dyn Prompt,
    ) -> Result<String> {
        prompt.notify(Notice::Confirm("compare the address on the Jade, then confirm".into()));
        let variant = match script {
            Script::Legacy => "pkh(k)",
            Script::Nested => "sh(wpkh(k))",
            Script::Native => "wpkh(k)",
        };
        let params = map([
            ("network", text(self.network)),
            ("path", path_value(&child_numbers(path))),
            ("variant", text(variant)),
        ]);
        let v = self.link.rpc(prompt, "get_receive_address", Some(params), None)?;
        prompt.notify(Notice::Working);
        text_of(Some(&v)).ok_or_else(|| Error::Protocol("no address in the reply".into()))
    }

    fn sign_psbt(&mut self, psbt: Psbt, prompt: &dyn Prompt) -> Result<Psbt> {
        prompt.notify(Notice::Confirm("review the outputs and fee on the Jade, then approve".into()));
        let out = self.link.sign_psbt(prompt, self.network, psbt.serialize())?;
        prompt.notify(Notice::Working);
        Psbt::deserialize(&out).map_err(Error::from)
    }

    fn sign_message(
        &mut self,
        _script: Script,
        path: &DerivationPath,
        message: &str,
        prompt: &dyn Prompt,
    ) -> Result<String> {
        prompt.notify(Notice::Confirm("check the message on the Jade, then approve".into()));
        let params = map([
            ("path", path_value(&child_numbers(path))),
            ("message", text(message)),
        ]);
        let v = self.link.rpc(prompt, "sign_message", Some(params), None)?;
        prompt.notify(Notice::Working);
        text_of(Some(&v)).ok_or_else(|| Error::Protocol("no signature in the reply".into()))
    }
}

fn net_name(n: Network) -> Result<&'static str> {
    Ok(match n {
        Network::Bitcoin => "mainnet",
        Network::Testnet | Network::Testnet4 | Network::Signet => "testnet",
        Network::Regtest => "localtest",
    })
}

fn parse_version(v: &str) -> (u32, u32, u32) {
    let core = v.split(['-', '+']).next().unwrap_or("");
    let mut it = core.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    (it.next().unwrap_or(0), it.next().unwrap_or(0), it.next().unwrap_or(0))
}

fn fill_random(buf: &mut [u8]) {
    use bitcoin::secp256k1::rand::{RngCore, thread_rng};
    thread_rng().fill_bytes(buf);
}

fn child_numbers(path: &DerivationPath) -> Vec<u32> {
    path.into_iter().map(|c| u32::from(*c)).collect()
}

fn path_value(p: &[u32]) -> Value {
    Value::Array(p.iter().map(|n| Value::from(*n)).collect())
}

fn xpub_of(v: &Value) -> Result<Xpub> {
    let s = text_of(Some(v)).ok_or_else(|| Error::Protocol("no xpub in the reply".into()))?;
    Xpub::from_str(&s).map_err(Error::from)
}

// ---- CBOR helpers ----

fn text(s: &str) -> Value {
    Value::Text(s.to_string())
}

fn map<const N: usize>(items: [(&str, Value); N]) -> Value {
    Value::Map(items.into_iter().map(|(k, v)| (text(k), v)).collect())
}

fn get<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    v.as_map()?.iter().find(|(k, _)| k.as_text() == Some(key)).map(|(_, v)| v)
}

fn text_of(v: Option<&Value>) -> Option<String> {
    v?.as_text().map(str::to_string)
}

fn to_json(v: &Value) -> serde_json::Value {
    use serde_json::Value as J;
    match v {
        Value::Null => J::Null,
        Value::Bool(b) => J::Bool(*b),
        Value::Integer(i) => {
            let n: i128 = (*i).into();
            if let Ok(n) = i64::try_from(n) {
                J::from(n)
            } else {
                J::from(n as f64)
            }
        }
        Value::Float(f) => J::from(*f),
        Value::Text(s) => J::String(s.clone()),
        Value::Bytes(b) => J::String(b.iter().map(|x| format!("{x:02x}")).collect()),
        Value::Array(a) => J::Array(a.iter().map(to_json).collect()),
        Value::Map(m) => J::Object(
            m.iter().filter_map(|(k, v)| Some((k.as_text()?.to_string(), to_json(v)))).collect(),
        ),
        _ => J::Null,
    }
}

fn from_json(v: &serde_json::Value) -> Value {
    use serde_json::Value as J;
    match v {
        J::Null => Value::Null,
        J::Bool(b) => Value::Bool(*b),
        J::Number(n) => match n.as_i64() {
            Some(i) => Value::from(i),
            None => Value::Float(n.as_f64().unwrap_or(0.0)),
        },
        J::String(s) => Value::Text(s.clone()),
        J::Array(a) => Value::Array(a.iter().map(from_json).collect()),
        J::Object(o) => Value::Map(o.iter().map(|(k, v)| (text(k), from_json(v))).collect()),
    }
}

// ---- the wire ----

enum End {
    Cancelled,
    Patience,
}

struct Link {
    port: Box<dyn SerialPort>,
    counter: u32,
    /// Ids of requests we sent on behalf of a pin exchange that an earlier session started and
    /// cancelled. The Jade keeps waiting for the PIN across a reconnect, so we finish the exchange.
    adopted: Vec<String>,
}

/// Reads CBOR off the port, giving up when the person cancels or the patience runs out.
struct Reader<'a> {
    port: &'a mut dyn SerialPort,
    prompt: &'a dyn Prompt,
    deadline: Option<Instant>,
    end: Option<End>,
}

impl Read for Reader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            match self.port.read(buf) {
                Ok(0) => {}
                Ok(n) => return Ok(n),
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(e) => return Err(e),
            }
            if self.prompt.cancelled() {
                self.end = Some(End::Cancelled);
                return Err(std::io::Error::other("cancelled"));
            }
            if self.deadline.is_some_and(|d| Instant::now() > d) {
                self.end = Some(End::Patience);
                return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "no reply"));
            }
        }
    }
}

impl Link {
    fn connect(path: &str) -> Result<Link> {
        let mut port = serialport::new(path, BAUD).timeout(POLL).open().map_err(|e| match e.kind {
            serialport::ErrorKind::NoDevice => Error::NotFound,
            serialport::ErrorKind::Io(std::io::ErrorKind::PermissionDenied) => {
                Error::from(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
            }
            _ => Error::Busy(format!("{path}: {}", e.description)),
        })?;
        // DTR and RTS wired to the ESP32's reset and boot pins reboot it, so leave both low.
        let _ = port.write_data_terminal_ready(false);
        let _ = port.write_request_to_send(false);
        let _ = port.clear(ClearBuffer::Input);
        Ok(Link { port, counter: seed(), adopted: Vec::new() })
    }

    fn next_id(&mut self) -> String {
        self.counter = self.counter.wrapping_add(1) % 900_000;
        (100_000 + self.counter).to_string()
    }

    fn send(&mut self, req: &Value) -> Result<()> {
        let mut bytes = Vec::new();
        ciborium::into_writer(req, &mut bytes).map_err(|e| Error::Protocol(e.to_string()))?;
        self.port.write_all(&bytes)?;
        self.port.flush()?;
        Ok(())
    }

    /// Send `method` and return its result, relaying any pin server traffic the Jade asks for.
    /// `patience` bounds the wait for each reply; `None` waits as long as the person takes.
    fn rpc(
        &mut self,
        prompt: &dyn Prompt,
        method: &str,
        params: Option<Value>,
        patience: Option<Duration>,
    ) -> Result<Value> {
        let mut method = method.to_string();
        let mut params = params;
        loop {
            let id = self.next_id();
            self.send(&request(&id, &method, params.take()))?;
            let reply = self.read_reply(prompt, &id, patience)?;
            let result = result_of(&reply)?;
            let Some(http) = get(result, "http_request") else { return Ok(result.clone()) };
            let call = get(http, "params")
                .ok_or_else(|| Error::Protocol("http_request without params".into()))?;
            let next = text_of(get(http, "on-reply"))
                .ok_or_else(|| Error::Protocol("http_request without on-reply".into()))?;
            let body = relay(call)?;
            method = next;
            params = Some(from_json(&body));
        }
    }

    /// Read one CBOR message, giving up when the person cancels or the patience runs out.
    fn read_one(&mut self, prompt: &dyn Prompt, patience: Option<Duration>) -> Result<Value> {
        let mut reader = Reader {
            port: &mut *self.port,
            prompt,
            deadline: patience.map(|p| Instant::now() + p),
            end: None,
        };
        match ciborium::from_reader(&mut reader) {
            Ok(v) => Ok(v),
            Err(e) => Err(match reader.end.take() {
                Some(End::Cancelled) => Error::cancelled(),
                Some(End::Patience) => Error::Io("the Jade did not answer".into()),
                None => match e {
                    ciborium::de::Error::Io(io) => Error::from(io),
                    other => {
                        let _ = reader.port.clear(ClearBuffer::Input);
                        Error::Protocol(other.to_string())
                    }
                },
            }),
        }
    }

    /// Read until the reply to `id` arrives, skipping device logs and finishing the pin exchange of
    /// any earlier session on the way.
    fn read_reply(
        &mut self,
        prompt: &dyn Prompt,
        id: &str,
        patience: Option<Duration>,
    ) -> Result<Value> {
        loop {
            let v = self.read_one(prompt, patience)?;
            let Some(got) = text_of(get(&v, "id")) else { continue };
            if got == id || (got == "00" && get(&v, "error").is_some()) {
                return Ok(v);
            }
            let stale = self.adopted.iter().position(|a| *a == got);
            if let Some(i) = stale {
                self.adopted.remove(i);
            }
            let Some(result) = get(&v, "result") else { continue };
            let Some(http) = get(result, "http_request") else { continue };
            let (Some(call), Some(next)) = (get(http, "params"), text_of(get(http, "on-reply")))
            else {
                continue;
            };
            let body = relay(call)?;
            let again = self.next_id();
            self.send(&request(&again, &next, Some(from_json(&body))))?;
            self.adopted.push(again);
        }
    }

    fn sign_psbt(&mut self, prompt: &dyn Prompt, network: &str, psbt: Vec<u8>) -> Result<Vec<u8>> {
        let id = self.next_id();
        let params = map([("network", text(network)), ("psbt", Value::Bytes(psbt))]);
        self.send(&request(&id, "sign_psbt", Some(params)))?;
        let mut out = Vec::new();
        let mut current = id;
        loop {
            let reply = self.read_reply(prompt, &current, None)?;
            match result_of(&reply)? {
                Value::Bytes(b) => out.extend_from_slice(b),
                other => return Err(Error::Protocol(format!("sign_psbt gave {other:?}"))),
            }
            let number = |k: &str| get(&reply, k).and_then(|v| v.as_integer()).map(i128::from);
            match (number("seqnum"), number("seqlen")) {
                (Some(n), Some(l)) if n != l => {
                    let next = self.next_id();
                    let more = map([
                        ("origid", text(&current)),
                        ("orig", text("sign_psbt")),
                        ("seqnum", Value::from((n + 1) as i64)),
                        ("seqlen", Value::from(l as i64)),
                    ]);
                    self.send(&request(&next, "get_extended_data", Some(more)))?;
                    current = next;
                }
                _ => return Ok(out),
            }
        }
    }
}

fn seed() -> u32 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(1) % 900_000
}

fn request(id: &str, method: &str, params: Option<Value>) -> Value {
    let mut m = vec![(text("method"), text(method)), (text("id"), text(id))];
    if let Some(p) = params {
        m.push((text("params"), p));
    }
    Value::Map(m)
}

fn result_of(reply: &Value) -> Result<&Value> {
    if let Some(e) = get(reply, "error") {
        let code = get(e, "code").and_then(|c| c.as_integer()).map(i128::from).unwrap_or(0) as i64;
        let message = text_of(get(e, "message")).unwrap_or_default();
        return Err(match code {
            USER_CANCELLED => Error::Declined(if message.is_empty() { "cancelled on the Jade".into() } else { message }),
            HW_LOCKED => Error::Locked("unlock the Jade first".into()),
            NETWORK_MISMATCH => Error::Network(message),
            BAD_PARAMETERS => Error::Psbt(message),
            _ => Error::Protocol(format!("{code}: {message}")),
        });
    }
    get(reply, "result").ok_or_else(|| Error::Protocol("reply has neither result nor error".into()))
}

/// Carry out the HTTP request the Jade describes and return the JSON body.
fn relay(params: &Value) -> Result<serde_json::Value> {
    let urls: Vec<String> = get(params, "urls")
        .and_then(|u| u.as_array())
        .map(|a| a.iter().filter_map(|u| u.as_text().map(str::to_string)).collect())
        .unwrap_or_default();
    let url = urls
        .iter()
        .find(|u| !u.contains(".onion"))
        .ok_or_else(|| Error::Io("the Jade only offered Tor pin servers".into()))?;
    let method = text_of(get(params, "method")).unwrap_or_else(|| "GET".into());
    let response = match method.as_str() {
        "GET" => minreq::get(url).with_timeout(30).send(),
        "POST" => {
            let body = get(params, "data").map(to_json).unwrap_or(serde_json::Value::Null);
            minreq::post(url).with_body(body.to_string()).with_timeout(30).send()
        }
        other => return Err(Error::Protocol(format!("pin server method {other}"))),
    }
    .map_err(|e| Error::Io(format!("pin server unreachable: {e}")))?;
    if response.status_code != 200 {
        return Err(Error::Io(format!("pin server answered {}", response.status_code)));
    }
    response.json::<serde_json::Value>().map_err(|e| Error::Io(format!("pin server reply: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_ignore_suffixes() {
        assert_eq!(parse_version("1.0.40"), (1, 0, 40));
        assert_eq!(parse_version("0.1.47-beta1"), (0, 1, 47));
        assert!(parse_version("0.1.46") < MIN_PSBT_FW);
        assert!(parse_version("1.0.0") >= MIN_PSBT_FW);
    }

    #[test]
    fn requests_carry_id_and_method() {
        let r = request("123456", "ping", None);
        assert_eq!(text_of(get(&r, "method")).as_deref(), Some("ping"));
        assert_eq!(text_of(get(&r, "id")).as_deref(), Some("123456"));
        assert!(get(&r, "params").is_none());
    }

    #[test]
    fn errors_map_to_words() {
        let reply = map([("id", text("1")), ("error", map([("code", Value::from(-32000)), ("message", text("User declined"))]))]);
        assert!(matches!(result_of(&reply), Err(Error::Declined(m)) if m == "User declined"));
        let reply = map([("id", text("1")), ("error", map([("code", Value::from(-32002)), ("message", text("locked"))]))]);
        assert!(matches!(result_of(&reply), Err(Error::Locked(_))));
    }

    #[test]
    fn json_and_cbor_round_trip() {
        let j = serde_json::json!({"a": [1, 2, {"b": "c"}], "d": true, "e": null});
        assert_eq!(to_json(&from_json(&j)), j);
    }

    #[test]
    fn cbor_request_is_what_the_jade_expects() {
        let r = request("1", "get_xpub", Some(map([("network", text("mainnet")), ("path", path_value(&[0x8000_0054, 0x8000_0000]))])));
        let mut bytes = Vec::new();
        ciborium::into_writer(&r, &mut bytes).unwrap();
        let back: Value = ciborium::from_reader(&bytes[..]).unwrap();
        assert_eq!(back, r);
    }
}
