//! Web UI and JSON API on port 80, on both the home network and the setup hotspot.
//!
//! A deliberately small HTTP/1.1 server: one request per connection, bodies up to
//! [`MAX_BODY`]. The UI is one gzipped HTML file (`web/`, built with SvelteKit) with hash
//! routing, so every page is `/`.
//!
//! API (JSON; everything but `/` needs `Authorization: Basic` with the device password,
//! any user name):
//! - `GET /api/status`: bridge, devices, network, system.
//! - `GET /api/settings`, `PUT /api/settings` (partial; saves and restarts).
//! - `GET /api/networks`: Wi-Fi scan.
//! - `GET /api/config`: the bridge config with all devices (large: the UI does not poll it).
//! - `GET /api/devices/<address>`: one device's stored settings; `DELETE`: forget it.
//! - `POST /api/bridge`: as MQTT `max/set`.
//! - `POST /api/devices/<address>`: as MQTT `max/<name>/set`, plus `"name"` to rename.
//! - `GET /api/backup`: settings (without the device password) and config in one file; `PUT /api/backup`:
//!   restore one, which restarts. See [`max2mqtt_bridge::backup`].
//! - `POST /api/restart`, `POST /api/factory-reset`.
//! - `POST /api/radio/calibrate` (`{"run": true|false}`), `GET /api/radio/calibration`:
//!   measure the CC1101's frequency error from received packets.
//! - `GET /api/logs?after=<n>`: the log kept in RAM, lines after sequence number `n`
//!   (written piece by piece, like the config).
//! - `GET /api/crashes`, `POST /api/crashes/clear`: what went wrong before restarts.
//! - `POST /api/firmware`: an app image (`fw-image`), streamed into the other app slot;
//!   restarts into it. See [`crate::ota`].

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::sync::atomic::Ordering;

use embassy_futures::select::{Either, select};
use embassy_net::Stack;
use embassy_net::tcp::TcpSocket;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use embedded_io_async::Write;
use max2mqtt_bridge::backup::Backup;
use max2mqtt_bridge::{Address, Bridge, Settings, Topics};
use serde_json::{Value, json};

use crate::shared::{
    HOTSPOT, MQTT_CONNECTED, SCAN_REQUEST, SCAN_RESULT, STATION, SYSTEM, SystemCmd, ctx, dispatch, hotspot_used,
    with_bridge,
};
use crate::storage::SharedStore;
use crate::{board, net, ota};

const MAX_HEAD: usize = 4096;
const MAX_BODY: usize = 16 * 1024;

static INDEX: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/index.html.gz"));
/// SvelteKit's build id: open pages fetch it to notice a new UI after a firmware update.
static VERSION: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/version.json"));

pub struct App {
    pub settings: &'static Settings,
    pub store: &'static SharedStore,
    pub hotspot_ssid: &'static str,
    pub station: Stack<'static>,
}

#[embassy_executor::task(pool_size = 6)]
pub async fn web_task(stack: Stack<'static>, on_hotspot: bool, app: &'static App) {
    loop {
        // The hotspot is off nearly always: its listeners take their buffers only while it
        // is on.
        while on_hotspot && !HOTSPOT.load(Ordering::Relaxed) {
            Timer::after_secs(1).await;
        }
        // Requests are small; a firmware upload is streamed through it.
        let mut rx = alloc::vec![0u8; 1024];
        let mut tx = alloc::vec![0u8; 2048];
        while serve(stack, on_hotspot, app, &mut rx, &mut tx).await {}
    }
}

/// The hotspot went off: checked once a second.
async fn hotspot_off() {
    while HOTSPOT.load(Ordering::Relaxed) {
        Timer::after_secs(1).await;
    }
}

/// Wait for one connection and answer its request. `false` when the hotspot this listens
/// on went off instead.
async fn serve(stack: Stack<'static>, on_hotspot: bool, app: &'static App, rx: &mut [u8], tx: &mut [u8]) -> bool {
    let mut socket = TcpSocket::new(stack, rx, tx);
    socket.set_timeout(Some(Duration::from_secs(10)));
    if on_hotspot {
        match select(socket.accept(80), hotspot_off()).await {
            Either::First(Ok(())) => hotspot_used(),
            Either::First(Err(_)) => return true,
            Either::Second(()) => return false,
        }
    } else if socket.accept(80).await.is_err() {
        return true;
    }
    let mut path = String::new();
    let (response, after) = match with_timeout(Duration::from_secs(10), read_request(&mut socket)).await {
        // Written piece by piece as they are produced: built whole first, they took up to
        // twice their size in memory.
        Ok(Ok(request))
            if request.method == "GET"
                && (request.path == "/api/config" || request.path == "/api/backup")
                && authorized(app, request.authorization.as_deref()) =>
        {
            path = request.path.clone();
            if let Err(e) = stream_config(&mut socket, app, request.path == "/api/backup").await {
                log::debug!("HTTP write failed: {e:?}");
            }
            (None, None)
        }
        Ok(Ok(request))
            if request.method == "GET"
                && request.path == "/api/logs"
                && authorized(app, request.authorization.as_deref()) =>
        {
            path = request.path.clone();
            let after = request.query.split('&').find_map(|kv| kv.strip_prefix("after=")).and_then(|v| v.parse().ok());
            if let Err(e) = stream_logs(&mut socket, after.unwrap_or(0)).await {
                log::debug!("HTTP write failed: {e:?}");
            }
            (None, None)
        }
        Ok(Ok(request)) if request.method == "POST" && request.path == "/api/firmware" => {
            if !authorized(app, request.authorization.as_deref()) {
                Timer::after_millis(500).await;
                (Some(Response::error(401, "wrong password")), None)
            } else {
                match ota::receive(&mut socket, &request.body, request.length, app.store).await {
                    Ok(()) => (Some(Response::ok(json!({ "ok": true, "restarting": true }))), Some(SystemCmd::Restart)),
                    Err(e) => {
                        log::warn!("Firmware upload failed: {e}");
                        (Some(Response::error(400, &e)), None)
                    }
                }
            }
        }
        Ok(Ok(request)) => {
            path = request.path.clone();
            let (r, after) = handle(app, on_hotspot, request).await;
            (Some(r), after)
        }
        Ok(Err(status)) => (Some(Response::error(status, "bad request")), None),
        Err(_) => (Some(Response::error(408, "timeout")), None),
    };
    // The request and its response are both in memory here.
    crate::heap::note(&path);
    if let Some(response) = response
        && let Err(e) = response.write(&mut socket).await
    {
        log::debug!("HTTP write failed: {e:?}");
    }
    let _ = socket.flush().await;
    socket.close();
    // Let the FIN go out before the socket is dropped (which would reset).
    let _ = with_timeout(Duration::from_secs(2), async {
        let mut b = [0u8; 64];
        while socket.read(&mut b).await.is_ok_and(|n| n > 0) {}
    })
    .await;
    if let Some(cmd) = after {
        let _ = SYSTEM.try_send(cmd);
    }
    true
}

struct Request {
    method: String,
    path: String,
    query: String,
    host: Option<String>,
    authorization: Option<String>,
    /// Content-Length.
    length: usize,
    /// The whole body; for a firmware upload only what came with the headers.
    body: Vec<u8>,
}

async fn read_request(socket: &mut TcpSocket<'_>) -> Result<Request, u16> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 512];
    let head_end = loop {
        let n = socket.read(&mut chunk).await.map_err(|_| 400u16)?;
        if n == 0 {
            return Err(400);
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        if buf.len() > MAX_HEAD {
            return Err(431);
        }
    };
    let head = core::str::from_utf8(&buf[..head_end]).map_err(|_| 400u16)?;
    let mut lines = head.split("\r\n");
    let mut first = lines.next().ok_or(400u16)?.split(' ');
    let method = first.next().ok_or(400u16)?.to_string();
    let target = first.next().ok_or(400u16)?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let (path, query) = (path.to_string(), query.to_string());
    let (mut host, mut authorization, mut length) = (None, None, 0usize);
    for line in lines {
        let Some((name, value)) = line.split_once(':') else { continue };
        let value = value.trim();
        if name.eq_ignore_ascii_case("host") {
            host = Some(value.to_string());
        } else if name.eq_ignore_ascii_case("authorization") {
            authorization = Some(value.to_string());
        } else if name.eq_ignore_ascii_case("content-length") {
            length = value.parse().map_err(|_| 400u16)?;
        }
    }
    let mut body = buf.split_off(head_end + 4);
    if method == "POST" && path == "/api/firmware" {
        // Streamed by ota::receive.
        return Ok(Request { method, path, query, host, authorization, length, body });
    }
    if length > MAX_BODY {
        return Err(413);
    }
    while body.len() < length {
        let n = socket.read(&mut chunk).await.map_err(|_| 400u16)?;
        if n == 0 {
            return Err(400);
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(length);
    Ok(Request { method, path, query, host, authorization, length, body })
}

enum Body {
    Static(&'static [u8]),
    Owned(Vec<u8>),
}

struct Response {
    status: u16,
    content_type: &'static str,
    gzip: bool,
    location: Option<String>,
    body: Body,
}

impl Response {
    fn json(status: u16, v: &Value) -> Self {
        Response {
            status,
            content_type: "application/json",
            gzip: false,
            location: None,
            body: Body::Owned(v.to_string().into_bytes()),
        }
    }

    fn ok(v: Value) -> Self {
        Self::json(200, &v)
    }

    fn error(status: u16, message: &str) -> Self {
        Self::json(status, &json!({ "error": message }))
    }

    fn redirect(to: String) -> Self {
        Response { status: 302, content_type: "text/plain", gzip: false, location: Some(to), body: Body::Static(b"") }
    }

    async fn write(&self, socket: &mut TcpSocket<'_>) -> Result<(), embassy_net::tcp::Error> {
        let reason = match self.status {
            200 => "OK",
            302 => "Found",
            400 => "Bad Request",
            401 => "Unauthorized",
            404 => "Not Found",
            405 => "Method Not Allowed",
            408 => "Request Timeout",
            413 => "Content Too Large",
            431 => "Request Header Fields Too Large",
            503 => "Service Unavailable",
            _ => "Error",
        };
        let body: &[u8] = match &self.body {
            Body::Static(b) => b,
            Body::Owned(b) => b,
        };
        let mut head = format!(
            "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n",
            self.status,
            self.content_type,
            body.len()
        );
        if self.gzip {
            head.push_str("Content-Encoding: gzip\r\n");
        }
        if let Some(l) = &self.location {
            head.push_str(&format!("Location: {l}\r\n"));
        }
        head.push_str("\r\n");
        socket.write_all(head.as_bytes()).await?;
        socket.write_all(body).await
    }
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => continue,
            _ => return None,
        } as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn authorized(app: &App, header: Option<&str>) -> bool {
    let Some(decoded) = header.and_then(|h| h.strip_prefix("Basic ")).and_then(|b| base64_decode(b.trim())) else {
        return false;
    };
    let password = match decoded.iter().position(|&c| c == b':') {
        Some(i) => &decoded[i + 1..],
        None => &decoded[..],
    };
    constant_time_eq(password, app.settings.password.as_bytes())
}

fn parse_json(body: &[u8]) -> Result<Value, Response> {
    serde_json::from_slice(body).map_err(|e| Response::error(400, &format!("invalid JSON: {e}")))
}

async fn handle(app: &App, on_hotspot: bool, req: Request) -> (Response, Option<SystemCmd>) {
    let get = req.method == "GET" || req.method == "HEAD";
    if !req.path.starts_with("/api/") {
        if !get {
            return (Response::error(405, "method not allowed"), None);
        }
        // Captive portal: phones probe some well-known URL; send them to us.
        let ours = req.host.as_deref().is_some_and(|h| {
            h == "192.168.4.1" || h.split(':').next().is_some_and(|h| h.eq_ignore_ascii_case(&app.settings.hostname))
        });
        if on_hotspot && !ours {
            return (Response::redirect(format!("http://{}/", net::HOTSPOT_IP)), None);
        }
        if req.path == "/_app/version.json" {
            let r = Response {
                status: 200,
                content_type: "application/json",
                gzip: false,
                location: None,
                body: Body::Static(VERSION),
            };
            return (r, None);
        }
        if req.path != "/" && !on_hotspot {
            return (Response::error(404, "not found"), None);
        }
        let index = Response {
            status: 200,
            content_type: "text/html; charset=utf-8",
            gzip: true,
            location: None,
            body: Body::Static(INDEX),
        };
        return (index, None);
    }

    if !authorized(app, req.authorization.as_deref()) {
        // Slow down guessing.
        Timer::after_millis(500).await;
        return (Response::error(401, "wrong password"), None);
    }

    let path = &req.path["/api".len()..];
    let result = match (req.method.as_str(), path) {
        ("GET", "/status") => Ok((status(app).await, None)),
        ("GET", "/settings") => Ok((Response::ok(app.settings.redacted()), None)),
        ("PUT", "/settings") => save_settings(app, &req.body).await,
        ("GET", "/networks") => Ok((scan().await, None)),
        ("GET", "/crashes") => {
            let crashes = app.store.lock().await.as_ref().map(|s| s.crashes()).unwrap_or_default();
            Ok((Response::ok(serde_json::to_value(crashes).unwrap_or_default()), None))
        }
        ("POST", "/crashes/clear") => {
            if let Some(s) = app.store.lock().await.as_mut() {
                let _ = s.clear_crashes().await;
            }
            Ok((Response::ok(json!({ "ok": true })), None))
        }
        ("POST", "/radio/calibrate") => parse_json(&req.body).map(|v| {
            let run = v.get("run").and_then(Value::as_bool).unwrap_or(true);
            crate::shared::CALIBRATE.signal(run);
            (Response::ok(json!({ "ok": true })), None)
        }),
        ("GET", "/radio/calibration") => {
            let c = crate::shared::CALIBRATION.lock(|c| c.borrow().clone());
            let packets: Vec<Value> = c
                .packets
                .iter()
                .map(|(from, rssi, off)| json!({ "from": from, "rssi": rssi, "offset": off }))
                .collect();
            Ok((
                Response::ok(json!({
                    "running": c.running,
                    "listening_at": c.listening_at,
                    "seconds": Instant::now().as_secs().saturating_sub(c.started_at),
                    "packets": packets,
                    "suggested": c.suggested(),
                    "current": app.settings.cc1101_freq_offset,
                })),
                None,
            ))
        }
        ("PUT", "/backup") => restore(app, &req.body).await,
        ("POST", "/bridge") => bridge(&req.body).await,
        ("POST", "/restart") => Ok((Response::ok(json!({ "ok": true })), Some(SystemCmd::Restart))),
        ("POST", "/factory-reset") => {
            log::warn!("Factory reset from the web UI");
            Ok((Response::ok(json!({ "ok": true })), Some(SystemCmd::FactoryReset)))
        }
        ("POST", p) if p.starts_with("/devices/") => device(&p["/devices/".len()..], &req.body).await,
        ("DELETE", p) if p.starts_with("/devices/") => forget(&p["/devices/".len()..]).await,
        ("GET", p) if p.starts_with("/devices/") => {
            let address: Option<Address> = p["/devices/".len()..].parse().ok();
            let json = with_bridge(|b| address.and_then(|a| b.device_config(a)).map(serde_json::to_vec)).await;
            match json {
                Some(Ok(body)) => Ok((
                    Response {
                        status: 200,
                        content_type: "application/json",
                        gzip: false,
                        location: None,
                        body: Body::Owned(body),
                    },
                    None,
                )),
                _ => Err(Response::error(404, "no such device")),
            }
        }
        _ => Err(Response::error(404, "not found")),
    };
    result.unwrap_or_else(|r| (r, None))
}

type Handled = Result<(Response, Option<SystemCmd>), Response>;

async fn bridge(body: &[u8]) -> Handled {
    let v = parse_json(body)?;
    let effects = with_bridge(|b| b.configure_bridge(&v, ctx())).await;
    dispatch(effects);
    Ok((Response::ok(json!({ "ok": true })), None))
}

async fn forget(address: &str) -> Handled {
    let address: Address = address.parse().map_err(|_| Response::error(404, "no such device"))?;
    let effects = with_bridge(|b| b.forget_device(address, ctx())).await.map_err(|e| Response::error(404, &e))?;
    dispatch(effects);
    Ok((Response::ok(json!({ "ok": true })), None))
}

async fn device(address: &str, body: &[u8]) -> Handled {
    let address: Address = address.parse().map_err(|_| Response::error(404, "no such device"))?;
    let v = parse_json(body)?;
    let effects =
        with_bridge(|b| b.configure_device(address, &v, ctx())).await.map_err(|e| Response::error(400, &e))?;
    dispatch(effects);
    Ok((Response::ok(json!({ "ok": true })), None))
}

async fn save_settings(app: &App, body: &[u8]) -> Handled {
    let patch = parse_json(body)?;
    let settings = app.settings.patched(&patch).map_err(|e| Response::error(400, &e))?;
    let mut store = app.store.lock().await;
    let store = store.as_mut().ok_or_else(|| Response::error(503, "no config partition"))?;
    store.save_settings(&settings).await.map_err(|e| Response::error(503, &format!("saving failed: {e:?}")))?;
    log::warn!("Settings changed in the web UI, restarting");
    Ok((Response::ok(json!({ "ok": true, "restarting": true })), Some(SystemCmd::Restart)))
}

/// Write a backup's settings and config, and put its config into the running bridge too:
/// the restart saves the bridge's config, which would otherwise be the old one. Done while
/// holding the store, so no config save from before can land in between.
async fn restore(app: &App, body: &[u8]) -> Handled {
    let backup = Backup::restore(body, &app.settings.password).map_err(|e| Response::error(400, &e))?;
    let mut store = app.store.lock().await;
    let store = store.as_mut().ok_or_else(|| Response::error(503, "no config partition"))?;
    let failed = |e| Response::error(503, &format!("saving failed: {e:?}"));
    store.save_settings(&backup.settings).await.map_err(failed)?;
    store.save_config(backup.config.clone()).await.map_err(failed)?;
    let devices = backup.config.devices.len();
    let topics = Topics { prefix: app.settings.mqtt.prefix.clone(), discovery: None };
    with_bridge(|b| *b = Bridge::new(backup.config, topics, Instant::now().as_millis())).await;
    log::warn!("Restored a backup ({devices} devices, from firmware {}), restarting", backup.firmware);
    Ok((Response::ok(json!({ "ok": true, "restarting": true })), Some(SystemCmd::Restart)))
}

/// `GET /api/config`, or with `backup` `GET /api/backup` (the same as [`Backup`] writes it),
/// one device at a time. No Content-Length: the connection closes at the end.
async fn stream_config(socket: &mut TcpSocket<'_>, app: &App, backup: bool) -> Result<(), embassy_net::tcp::Error> {
    socket
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n")
        .await?;
    if backup {
        let mut settings = app.settings.clone();
        // Not in backups: a restore keeps the board's own.
        settings.password.clear();
        let start = format!(
            "{{\"format\":\"{}\",\"version\":{},\"firmware\":\"{}\",\"settings\":{},\"config\":",
            max2mqtt_bridge::backup::FORMAT,
            max2mqtt_bridge::backup::VERSION,
            env!("CARGO_PKG_VERSION"),
            settings.to_json()
        );
        socket.write_all(start.as_bytes()).await?;
    }
    let (head, tail) = with_bridge(|b| b.config_json_around_devices()).await;
    socket.write_all(head.as_bytes()).await?;
    drop(head);
    let mut k = 0;
    while let Some(device) = with_bridge(|b| b.device_json(k)).await {
        if k > 0 {
            socket.write_all(b",").await?;
        }
        socket.write_all(device.as_bytes()).await?;
        k += 1;
    }
    socket.write_all(tail.as_bytes()).await?;
    if backup {
        socket.write_all(b"}").await?;
    }
    Ok(())
}

/// `GET /api/logs`: `{"lines":[[n,"text"],...],"last":n}`, a kilobyte of log at a time.
/// Built whole, the answer took three times the log's size next to the log itself: the
/// highest heap use of all requests. No Content-Length: the connection closes at the end.
async fn stream_logs(socket: &mut TcpSocket<'_>, mut after: u32) -> Result<(), embassy_net::tcp::Error> {
    const PIECE: usize = 1024;
    socket
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n{\"lines\":[")
        .await?;
    let mut text = Vec::with_capacity(PIECE);
    let mut json = Vec::with_capacity(PIECE + PIECE / 4);
    let mut comma = false;
    let last = loop {
        let copied = crate::logger::copy_after(after, &mut text, PIECE);
        if copied.lines == 0 {
            break copied.last;
        }
        json.clear();
        for (n, line) in (copied.first..).zip(text.split(|&c| c == b'\n')).take(copied.lines as usize) {
            if comma {
                json.push(b',');
            }
            comma = true;
            json.extend(serde_json::to_vec(&(n, String::from_utf8_lossy(line))).unwrap_or_default());
        }
        socket.write_all(&json).await?;
        after = copied.first + copied.lines - 1;
    };
    socket.write_all(format!("],\"last\":{last}}}").as_bytes()).await
}

async fn scan() -> Response {
    SCAN_RESULT.reset();
    SCAN_REQUEST.signal(());
    match with_timeout(Duration::from_secs(15), SCAN_RESULT.wait()).await {
        Ok(Ok(networks)) => Response::ok(Value::Array(
            networks.iter().map(|n| json!({ "ssid": n.ssid, "rssi": n.rssi, "secure": n.secure })).collect(),
        )),
        Ok(Err(())) => Response::error(503, "scan failed"),
        Err(_) => Response::error(503, "scan timed out"),
    }
}

#[derive(serde::Serialize)]
struct Status<'a> {
    #[serde(flatten)]
    core: max2mqtt_bridge::bridge::WebStatus<'a>,
    network: Network<'a>,
    system: System,
}

#[derive(serde::Serialize)]
struct Network<'a> {
    station: bool,
    ssid: &'a str,
    ip: Option<String>,
    hostname: &'a str,
    hotspot: bool,
    hotspot_ssid: &'a str,
    mqtt: bool,
    mqtt_host: &'a str,
}

#[derive(serde::Serialize)]
struct System {
    version: &'static str,
    board: &'static str,
    uptime_seconds: u64,
    heap_used: usize,
    heap_max: usize,
    heap_size: usize,
    firmware_slot: String,
    firmware_state: String,
}

/// Written straight into the response while the bridge is locked: no JSON tree, which for
/// ten devices took tens of kilobytes per request.
async fn status(app: &App) -> Response {
    let (firmware_slot, firmware_state) = app.store.lock().await.as_mut().map(ota::describe).unwrap_or_default();
    let heap = esp_alloc::HEAP.stats();
    let s = app.settings;
    let network = Network {
        station: STATION.load(Ordering::Relaxed),
        ssid: &s.wifi.ssid,
        ip: app.station.config_v4().map(|c| c.address.address().to_string()),
        hostname: &s.hostname,
        hotspot: HOTSPOT.load(Ordering::Relaxed),
        hotspot_ssid: app.hotspot_ssid,
        mqtt: MQTT_CONNECTED.load(Ordering::Relaxed),
        mqtt_host: &s.mqtt.host,
    };
    let system = System {
        version: env!("CARGO_PKG_VERSION"),
        board: board::NAME,
        uptime_seconds: Instant::now().as_secs(),
        heap_used: heap.current_usage,
        heap_max: heap.max_usage,
        heap_size: heap.size,
        firmware_slot,
        firmware_state,
    };
    let body = with_bridge(|b| serde_json::to_vec(&Status { core: b.status(ctx()), network, system })).await;
    Response {
        status: 200,
        content_type: "application/json",
        gzip: false,
        location: None,
        body: Body::Owned(body.unwrap_or_default()),
    }
}
