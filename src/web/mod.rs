//! The web UI: a local HTTP server over the same [`Hub`] as the TUI. Serves
//! the page and a small JSON API the page polls.
//!
//! The API can type prompts into agents, so it only binds to 127.0.0.1, only
//! answers requests addressed to localhost or this machine's own tailnet name
//! (for `tailscale serve`; this check stops DNS rebinding), and every
//! API call must carry an `X-Agentchat` header, which other sites can't add to
//! a cross-origin request without a CORS preflight this server never grants.
//! `/files/<id>` serves only stored uploads.

mod render;

use crate::hub::feed::{self, Body, Clamp};
use crate::hub::{self, Hub, Input};
use crate::{db, theme, uploads};
use anyhow::{anyhow, Result};
use ratatui::style::Color;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tiny_http::{Header, Method, Request, Response, Server};

pub const DEFAULT_PORT: u16 = 3000;
const API_HEADER: &str = "X-Agentchat";
const POLL: Duration = Duration::from_millis(400);

const INDEX: &str = include_str!("assets/index.html");
const STYLE: &str = include_str!("assets/style.css");
const SCRIPT: &str = include_str!("assets/app.js");

struct Shared {
    hub: Mutex<Hub>,
    /// Rendered post HTML by key.
    html: Mutex<HashMap<String, String>>,
    /// Host names requests may be addressed to, besides the tailnet name.
    hosts: [String; 2],
}

/// Runs the server in the foreground until `/api/stop`.
pub fn serve(port: u16) -> Result<()> {
    let server = Server::http(("127.0.0.1", port)).map_err(|e| anyhow!("listening on 127.0.0.1:{port}: {e}"))?;
    let hosts = [format!("127.0.0.1:{port}"), format!("localhost:{port}")];
    let shared = Arc::new(Shared { hub: Mutex::new(Hub::open()?), html: Mutex::default(), hosts });
    std::fs::write(info_path(), json!({ "pid": std::process::id(), "port": port }).to_string())?;

    let poller = shared.clone();
    std::thread::spawn(move || loop {
        let _ = poller.hub.lock().unwrap().poll();
        std::thread::sleep(POLL);
    });
    for req in server.incoming_requests() {
        let shared = shared.clone();
        std::thread::spawn(move || handle(req, &shared));
    }
    Ok(())
}

// ---------------------------------------------------------------- background control

fn info_path() -> PathBuf {
    db::data_dir().join("server.json")
}

/// The port of a running server, if one answers.
pub fn running() -> Option<u16> {
    let info: Value = serde_json::from_str(&std::fs::read_to_string(info_path()).ok()?).ok()?;
    let port = info.get("port")?.as_u64()? as u16;
    call(port, "GET", "/api/ping").ok().map(|_| port)
}

/// Starts `agentchat serve` detached, and waits until it answers.
pub fn start(port: u16) -> Result<u16> {
    if let Some(p) = running() {
        return Ok(p);
    }
    use std::os::unix::process::CommandExt;
    let log = std::fs::File::create(db::data_dir().join("server.log"))?;
    std::process::Command::new(std::env::current_exe()?)
        .args(["serve", "--port", &port.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .process_group(0)
        .spawn()?;
    for _ in 0..50 {
        if let Some(p) = running() {
            return Ok(p);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(anyhow!("server didn't start; see {}", db::data_dir().join("server.log").display()))
}

/// Asks a running server to exit. False if none was running.
pub fn stop() -> Result<bool> {
    let Some(port) = running() else { return Ok(false) };
    call(port, "POST", "/api/stop")?;
    let _ = std::fs::remove_file(info_path());
    Ok(true)
}

/// A minimal local HTTP request, enough for ping and stop.
fn call(port: u16, method: &str, path: &str) -> Result<String> {
    let mut s = TcpStream::connect(("127.0.0.1", port))?;
    s.set_read_timeout(Some(Duration::from_secs(2)))?;
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{API_HEADER}: 1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )?;
    let mut out = String::new();
    s.read_to_string(&mut out)?;
    match out.starts_with("HTTP/1.1 200") {
        true => Ok(out),
        false => Err(anyhow!("unexpected response from agentchat server")),
    }
}

/// This machine's tailnet name (e.g. `box.tailnet.ts.net`), from the local
/// tailscaled API, so `tailscale serve` to this port works with no setup.
fn tailnet_name() -> Option<String> {
    let mut s = std::os::unix::net::UnixStream::connect("/var/run/tailscale/tailscaled.sock").ok()?;
    s.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    s.write_all(b"GET /localapi/v0/status HTTP/1.0\r\nHost: local-tailscaled.sock\r\n\r\n").ok()?;
    let mut out = String::new();
    s.read_to_string(&mut out).ok()?;
    let status: Value = serde_json::from_str(out.split_once("\r\n\r\n")?.1).ok()?;
    let name = status.pointer("/Self/DNSName")?.as_str()?;
    Some(name.trim_end_matches('.').to_ascii_lowercase())
}

// ---------------------------------------------------------------- routing

fn header<'a>(req: &'a Request, name: &str) -> Option<&'a str> {
    req.headers().iter().find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name)).map(|h| h.value.as_str())
}

fn respond(req: Request, status: u16, content_type: &str, body: impl Into<Vec<u8>>) {
    let ct = Header::from_bytes("Content-Type", content_type).expect("valid header");
    let nostore = Header::from_bytes("Cache-Control", "no-store").expect("valid header");
    let _ = req.respond(Response::from_data(body.into()).with_status_code(status).with_header(ct).with_header(nostore));
}

fn respond_json(req: Request, v: Value) {
    respond(req, 200, "application/json", v.to_string());
}

fn handle(mut req: Request, shared: &Shared) {
    let host = header(&req, "Host").unwrap_or("").to_ascii_lowercase();
    let host_ok = shared.hosts.contains(&host) || tailnet_name().is_some_and(|name| host == name);
    if !host_ok {
        return respond(req, 403, "text/plain", "forbidden");
    }
    let (path, query) = req.url().split_once('?').unwrap_or((req.url(), ""));
    let (path, query) = (path.to_string(), query.to_string());
    if path.starts_with("/api/") && header(&req, API_HEADER).is_none() {
        return respond(req, 403, "text/plain", "missing API header");
    }
    let mut raw = Vec::new();
    let _ = req.as_reader().take(uploads::MAX_BYTES as u64 + 1).read_to_end(&mut raw);
    let body: Value = serde_json::from_slice(&raw).unwrap_or(Value::Null);
    let arg = |k: &str| query_arg(&query, k).or_else(|| body.get(k).and_then(Value::as_str).map(str::to_string));

    match (req.method(), path.as_str()) {
        (Method::Get, "/") => respond(req, 200, "text/html; charset=utf-8", INDEX),
        (Method::Get, "/style.css") => respond(req, 200, "text/css", STYLE),
        (Method::Get, "/theme.css") => respond(req, 200, "text/css", theme_css()),
        (Method::Get, "/app.js") => respond(req, 200, "text/javascript", SCRIPT),
        (Method::Get, p) if p.starts_with("/files/") => match uploads::resolve(&percent_decode(&p[7..])) {
            Some(path) => {
                let nosniff = Header::from_bytes("X-Content-Type-Options", "nosniff").expect("valid header");
                let ct = Header::from_bytes("Content-Type", uploads::mime(&path)).expect("valid header");
                let data = std::fs::read(&path).unwrap_or_default();
                let _ = req.respond(Response::from_data(data).with_header(ct).with_header(nosniff));
            }
            None => respond(req, 404, "text/plain", "not found"),
        },
        (Method::Get, "/api/ping") => respond_json(req, json!({ "ok": true })),
        (Method::Get, "/api/state") => respond_json(req, state(shared)),
        (Method::Get, "/api/feed") => {
            let v = feed(shared, &arg("channel").unwrap_or_default(), arg("v").as_deref());
            respond_json(req, v)
        }
        (Method::Post, "/api/seen") => {
            shared.hub.lock().unwrap().mark_seen(&arg("channel").unwrap_or_default());
            respond_json(req, json!({ "ok": true }))
        }
        (Method::Post, "/api/upload") => {
            let name = arg("name").unwrap_or_else(|| "file".into());
            let v = match uploads::save(&name, &raw) {
                Ok(path) => file_json(&path),
                Err(e) => json!({ "error": e.to_string() }),
            };
            respond_json(req, v)
        }
        (Method::Post, "/api/send") => {
            let files: Vec<PathBuf> = body
                .get("files")
                .and_then(Value::as_array)
                .map(|ids| ids.iter().filter_map(Value::as_str).filter_map(uploads::resolve).collect())
                .unwrap_or_default();
            let v = send(shared, &arg("channel").unwrap_or_default(), &arg("text").unwrap_or_default(), &files);
            respond_json(req, v)
        }
        (Method::Post, "/api/stop") => {
            respond_json(req, json!({ "ok": true }));
            let _ = std::fs::remove_file(info_path());
            std::process::exit(0);
        }
        _ => respond(req, 404, "text/plain", "not found"),
    }
}

fn query_arg(query: &str, key: &str) -> Option<String> {
    query.split('&').filter_map(|kv| kv.split_once('=')).find(|(k, _)| *k == key).map(|(_, v)| percent_decode(v))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(b)) => {
                out.push(b);
                i += 3;
                continue;
            }
            (b'+', _) => out.push(b' '),
            (b, _) => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ---------------------------------------------------------------- API

fn state(shared: &Shared) -> Value {
    let hub = shared.hub.lock().unwrap();
    let channels: Vec<Value> = hub
        .channels
        .iter()
        .map(|c| {
            json!({
                "id": c.id,
                "name": c.name(),
                "title": c.title(),
                "section": c.section,
                "kind": c.kind,
                "presence": c.presence,
                "unread": hub.unread(c),
                "members": hub.members(c).len(),
                "detail": hub.detail(c),
            })
        })
        .collect();
    json!({
        "me": hub.me,
        "online": hub.online(),
        "ready": !hub.agents.is_empty(),
        "channels": channels,
        "statuses": hub.last_statuses(),
        "help": hub::HELP,
    })
}

/// The channel's posts with their HTML, or `same: true` if `v` is current.
fn feed(shared: &Shared, channel: &str, v: Option<&str>) -> Value {
    let mut hub = shared.hub.lock().unwrap();
    hub.follow(channel);
    let Some(chan) = hub.channel(channel).cloned() else { return json!({ "error": "no such channel" }) };
    let version = hub.version(&chan);
    if v == Some(version.as_str()) {
        return json!({ "v": version, "same": true });
    }
    let mut cache = shared.html.lock().unwrap();
    if cache.len() > 20_000 {
        cache.clear();
    }
    let posts: Vec<Value> = hub
        .posts(&chan)
        .iter()
        .map(|p| {
            let (html, tool, clamp) = match &p.body {
                Body::Markdown { text, clamp } => {
                    let html = cache.entry(p.key.clone()).or_insert_with(|| render::markdown(text)).clone();
                    let clamp = match clamp {
                        Clamp::Off => Value::Null,
                        Clamp::ToDm => json!("dm"),
                        Clamp::Expand => json!("expand"),
                    };
                    (Some(html), Value::Null, clamp)
                }
                Body::Tool { name, detail, error } => {
                    (None, json!({ "name": name, "detail": detail, "error": error }), Value::Null)
                }
            };
            json!({
                "key": p.key,
                "new_day": p.new_day,
                "day": feed::day_label(p.ts),
                "new_group": p.new_group,
                "author": p.author,
                "kind": p.author_kind,
                "subagent": p.is_subagent(),
                "tag": p.tag,
                "time": feed::clock(p.ts),
                "note": p.note,
                "html": html,
                "tool": tool,
                "clamp": clamp,
                "files": p.files.iter().map(|f| file_json(Path::new(*f))).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({ "v": version, "posts": posts })
}

fn send(shared: &Shared, channel: &str, text: &str, files: &[PathBuf]) -> Value {
    let body = match hub::parse_input(text) {
        Some(Input::Send(body)) => body,
        None if !files.is_empty() => String::new(),
        Some(Input::Help) => return json!({ "note": hub::HELP }),
        _ => return json!({}),
    };
    // Log under the lock; deliver without it, since sockets can be slow.
    let delivery = shared.hub.lock().unwrap().send(channel, &body, files);
    match delivery {
        Err(e) => json!({ "note": format!("failed: {e}") }),
        Ok(None) => json!({ "note": "nobody online here to deliver to" }),
        Ok(Some(d)) => {
            let (delivered, failed) = d.run();
            json!({ "delivered": delivered, "failed": failed })
        }
    }
}

fn file_json(path: &Path) -> Value {
    json!({ "id": uploads::id_of(path), "name": uploads::display_name(path), "image": uploads::is_image(path) })
}

/// The TUI's palette as CSS variables, so both UIs use the same colors.
fn theme_css() -> String {
    let hex = |c: Color| match c {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        _ => "inherit".into(),
    };
    let (r, g, b) = crate::highlight::background();
    let vars = [
        ("sidebar-bg", theme::SIDEBAR_BG),
        ("sidebar-fg", theme::SIDEBAR_FG),
        ("sidebar-dim", theme::SIDEBAR_DIM),
        ("selected-bg", theme::SELECTED_BG),
        ("badge-bg", theme::BADGE_BG),
        ("claude", theme::CLAUDE),
        ("codex", theme::CODEX),
        ("user", theme::USER),
        ("busy", theme::BUSY),
        ("online", theme::ONLINE),
        ("dim", theme::DIM),
        ("code", theme::CODE),
        ("error", theme::ERROR),
        ("link", theme::LINK),
        ("subagent", theme::SUBAGENT),
    ];
    let body: Vec<String> = vars.iter().map(|(k, c)| format!("--{k}:{};", hex(*c))).collect();
    format!(":root{{{}--code-bg:#{r:02x}{g:02x}{b:02x};}}", body.join(""))
}
