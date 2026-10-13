//! The web UI: a local HTTP server over the same [`Hub`] as the TUI. Serves
//! the page and a small JSON API the page polls.
//!
//! The API can type prompts into agents and run commands, so it only binds to
//! 127.0.0.1 and only answers requests addressed to localhost (against DNS
//! rebinding), or to this machine's tailnet name when `tailscale serve` says
//! the requester is the machine's owner. Every
//! API call must carry an `X-Agentrelay` header, which other sites can't add to
//! a cross-origin request without a CORS preflight this server never grants.
//! `/files/<id>` serves only stored uploads.

mod render;

use crate::hub::feed::{self, Body, Clamp};
use crate::hub::{self, Hub, Input};
use crate::{db, files, theme, uploads};
use anyhow::{anyhow, Result};
use ratatui::style::Color;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tiny_http::{Header, Method, Request, Response, Server};

pub const DEFAULT_PORT: u16 = 3000;
const API_HEADER: &str = "X-Agentrelay";
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
    /// This machine on the tailnet, looked up now and then.
    tailnet: Mutex<(Option<Instant>, Option<Tailnet>)>,
}

/// This machine's tailnet name and its owner's login.
#[derive(Clone)]
struct Tailnet {
    name: String,
    owner: String,
}

const TAILNET_REFRESH: Duration = Duration::from_secs(60);

impl Shared {
    fn tailnet(&self) -> Option<Tailnet> {
        let mut cached = self.tailnet.lock().unwrap();
        if cached.0.is_none_or(|at| at.elapsed() > TAILNET_REFRESH) {
            *cached = (Some(Instant::now()), tailnet());
        }
        cached.1.clone()
    }

    /// Localhost, or the tailnet name with `tailscale serve` vouching that
    /// the requester is the machine's owner.
    fn allowed(&self, req: &Request) -> bool {
        let host = header(req, "Host").unwrap_or("").to_ascii_lowercase();
        if self.hosts.contains(&host) {
            return true;
        }
        let login = header(req, "Tailscale-User-Login").unwrap_or("");
        self.tailnet().is_some_and(|t| host == t.name && login.eq_ignore_ascii_case(&t.owner))
    }
}

/// Runs the server in the foreground until `/api/stop`.
pub fn serve(port: u16) -> Result<()> {
    let server = Server::http(("127.0.0.1", port)).map_err(|e| anyhow!("listening on 127.0.0.1:{port}: {e}"))?;
    let hosts = [format!("127.0.0.1:{port}"), format!("localhost:{port}")];
    let shared = Arc::new(Shared { hub: Mutex::new(Hub::open()?), html: Mutex::default(), hosts, tailnet: Mutex::default() });
    std::fs::write(info_path(), json!({ "pid": std::process::id(), "port": port }).to_string())?;

    let poller = shared.clone();
    std::thread::spawn(move || loop {
        if let Err(e) = poller.hub.lock().unwrap().poll() {
            eprintln!("poll: {e:#}"); // goes to server.log; try again next tick
        }
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

/// Starts `agentrelay serve` detached, and waits until it answers.
pub fn start(port: u16) -> Result<u16> {
    if let Some(p) = running() {
        return Ok(p);
    }
    use std::os::unix::process::CommandExt;
    std::fs::create_dir_all(db::data_dir())?;
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
    // Wait for the port to free up, so an immediate `start` can bind it.
    for _ in 0..50 {
        if TcpStream::connect(("127.0.0.1", port)).is_err() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
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
        false => Err(anyhow!("unexpected response from agentrelay server")),
    }
}

/// This machine's tailnet name (e.g. `box.tailnet.ts.net`) and owner, from
/// the local tailscaled API.
fn tailnet() -> Option<Tailnet> {
    // Linux, then macOS's open-source tailscaled. (The macOS App Store app
    // doesn't expose this socket, so only localhost works with it.)
    let sockets = ["/var/run/tailscale/tailscaled.sock", "/var/run/tailscaled.socket"];
    let mut s = sockets.iter().find_map(|p| std::os::unix::net::UnixStream::connect(p).ok())?;
    s.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    s.write_all(b"GET /localapi/v0/status HTTP/1.0\r\nHost: local-tailscaled.sock\r\n\r\n").ok()?;
    let mut out = String::new();
    s.read_to_string(&mut out).ok()?;
    let status: Value = serde_json::from_str(out.split_once("\r\n\r\n")?.1).ok()?;
    let name = status.pointer("/Self/DNSName")?.as_str()?.trim_end_matches('.').to_ascii_lowercase();
    let user = status.pointer("/Self/UserID")?.as_u64()?.to_string();
    let owner = status.get("User")?.get(&user)?.get("LoginName")?.as_str()?.to_string();
    Some(Tailnet { name, owner })
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
    if !shared.allowed(&req) {
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
        (Method::Get, "/api/folders") => {
            let folders = shared.hub.lock().unwrap().folders();
            let list: Vec<Value> = folders.iter().map(|f| json!({ "path": f, "label": db::tilde(f) })).collect();
            respond_json(req, json!(list))
        }
        (Method::Get, "/api/tool") => {
            // A tool call's full input, rendered on demand when expanded.
            let hub = shared.hub.lock().unwrap();
            let full = hub.channel(&arg("channel").unwrap_or_default()).and_then(|c| hub.tool_full(c, &arg("key").unwrap_or_default()));
            drop(hub);
            respond_json(req, json!({ "html": full.map(|f| render::markdown(&f, &|_| false)) }))
        }
        (Method::Get, "/api/file") => {
            // A file a feed mentions, highlighted for the file panel, plus a
            // rendered preview for markdown.
            let mut hub = shared.hub.lock().unwrap();
            let channel = arg("channel").unwrap_or_default();
            hub.follow(&channel);
            let opened = match hub.channel(&channel) {
                Some(c) => hub.open_file(c, &arg("file").unwrap_or_default()),
                None => Err(anyhow!("no channel")),
            };
            drop(hub);
            let v = match opened {
                Ok((f, text)) => json!({
                    "path": db::tilde(&f.path.display().to_string()),
                    "full": f.path,
                    "line": f.line,
                    "html": render::file(&files::lang(&f.path), &text, f.line),
                    "preview": files::is_markdown(&f.path).then(|| render::preview(&text, &f.path)),
                }),
                Err(e) => json!({ "error": e.to_string() }),
            };
            respond_json(req, v)
        }
        (Method::Get, "/api/complete") => {
            let list = shared.hub.lock().unwrap().complete(&arg("channel").unwrap_or_default(), &arg("text").unwrap_or_default());
            respond_json(req, json!(list))
        }
        (Method::Get, "/api/feed") => {
            let v = feed(shared, &arg("channel").unwrap_or_default(), arg("v").as_deref());
            respond_json(req, v)
        }
        (Method::Post, "/api/seen") => {
            shared.hub.lock().unwrap().mark_seen(&arg("channel").unwrap_or_default());
            respond_json(req, json!({ "ok": true }))
        }
        (Method::Post, "/api/new") => {
            let kind = crate::agents::Kind::parse(&arg("kind").unwrap_or_default());
            let v = start_agent(shared, &arg("channel").unwrap_or_default(), kind, arg("folder").as_deref(), &arg("prompt").unwrap_or_default());
            respond_json(req, v)
        }
        (Method::Post, "/api/theme") => {
            let v = set_theme(shared, &arg("name").unwrap_or_default());
            respond_json(req, v)
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
                "headline": hub.headline(c),
                "placeholder": hub.placeholder(c, false),
                "placeholder_shell": hub.placeholder(c, true),
                "empty": hub.empty_hint(c),
                "folder": hub.default_folder(c),
            })
        })
        .collect();
    json!({
        "me": hub.me,
        "online": hub.online(),
        "ready": !hub.agents.is_empty(),
        "channels": channels,
        "statuses": hub.last_statuses(),
        "help": hub::help(),
        "theme": theme::t().name,
        "themes": theme::names(),

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
                    let key = format!("{}:{}", theme::t().name, p.key); // code colors follow the theme
                    let cwd = p.cwd.unwrap_or("");
                    let is_file = |c: &str| files::resolve(cwd, c).is_some();
                    let html = cache.entry(key).or_insert_with(|| render::markdown(text, &is_file)).clone();
                    let clamp = match clamp.hint() {
                        None => Value::Null,
                        Some(hint) => json!({ "to_dm": *clamp == Clamp::ToDm, "hint": hint }),
                    };
                    (Some(html), Value::Null, clamp)
                }
                Body::Tool { name, detail, full, path, error } => {
                    let file = path.filter(|f| files::resolve(p.cwd.unwrap_or(""), f).is_some());
                    let tool = json!({ "name": name, "detail": detail, "error": error, "expandable": !full.is_empty(), "file": file });
                    (None, tool, Value::Null)
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
        Some(Input::Help) => return json!({ "note": hub::help() }),
        Some(Input::Theme(None)) => return json!({ "note": hub::theme_list() }),
        Some(Input::New { kind, folder, prompt }) => return start_agent(shared, channel, kind, folder.as_deref(), &prompt),
        Some(Input::Open(query)) => {
            // Resolve a query (best fuzzy match) here; the page opens the path.
            let hub = shared.hub.lock().unwrap();
            let found = hub.channel(channel).map(|c| hub.open_query(c, &query));
            return match found {
                Some(Ok((f, _))) => json!({ "open": f.path }),
                Some(Err(e)) => json!({ "note": e.to_string() }),
                None => json!({ "note": "no channel" }),
            };
        }
        Some(Input::Rename(name)) => {
            let renamed = shared.hub.lock().unwrap().rename(channel, &name); // run outside the lock
            return match renamed {
                Ok((task, follow)) => json!({ "note": task.run(), "channel": follow }),
                Err(e) => json!({ "note": e.to_string() }),
            };
        }
        Some(Input::Shell(cmd)) => {
            let task = shared.hub.lock().unwrap().shell(channel, &cmd);
            return run_task(task);
        }
        Some(Input::Theme(Some(name))) => return set_theme(shared, &name),
        _ => return json!({}),
    };
    // Log under the lock; deliver without it, since sockets can be slow.
    let delivery = shared.hub.lock().unwrap().send(channel, &body, files);
    match delivery {
        Err(e) => json!({ "note": format!("failed: {e}") }),
        Ok(None) => json!({ "note": "nobody online here to deliver to" }),
        Ok(Some(d)) => {
            let report = d.run();
            json!({ "delivered": report.delivered, "note": report.note() })
        }
    }
}

/// Runs a hub task here, outside the hub's lock, for its note.
fn run_task(task: Result<hub::Task>) -> Value {
    json!({ "note": task.map_or_else(|e| e.to_string(), hub::Task::run) })
}

fn start_agent(shared: &Shared, channel: &str, kind: crate::agents::Kind, folder: Option<&str>, prompt: &str) -> Value {
    let task = shared.hub.lock().unwrap().start_agent(channel, kind, folder, prompt);
    run_task(task)
}

fn set_theme(shared: &Shared, name: &str) -> Value {
    match shared.hub.lock().unwrap().set_theme(name) {
        Ok(true) => json!({ "note": format!("theme: {name}"), "theme": name }),
        _ => json!({ "note": format!("no theme {name}; {}", hub::theme_list()) }),
    }
}

fn file_json(path: &Path) -> Value {
    json!({ "id": uploads::id_of(path), "name": uploads::display_name(path), "image": uploads::is_image(path) })
}

/// The active theme as CSS variables, so both UIs use the same colors.
fn theme_css() -> String {
    let th = theme::t();
    let hex = |c: Color| match c {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        _ => "inherit".into(),
    };
    let (r, g, b) = crate::highlight::background();
    let vars = [
        ("bg", th.bg),
        ("fg", th.fg),
        ("sidebar-bg", th.sidebar_bg),
        ("sidebar-fg", th.sidebar_fg),
        ("sidebar-dim", th.sidebar_dim),
        ("selected-bg", th.selected_bg),
        ("badge-bg", th.badge_bg),
        ("claude", th.claude),
        ("codex", th.codex),
        ("user", th.user),
        ("subagent", th.subagent),
        ("busy", th.busy),
        ("online", th.online),
        ("dim", th.dim),
        ("code", th.code),
        ("error", th.error),
        ("link", th.link),
    ];
    let body: Vec<String> = vars.iter().map(|(k, c)| format!("--{k}:{};", hex(*c))).collect();
    format!(":root{{{}--code-bg:#{r:02x}{g:02x}{b:02x};}}", body.join(""))
}
