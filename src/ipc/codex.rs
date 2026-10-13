//! The Codex app-server daemon that codex TUIs connect to. It serves JSON-RPC
//! over a WebSocket on a Unix socket; `turn/start` on a loaded thread runs a
//! turn there as if typed.

use crate::db::home;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::os::unix::net::UnixStream;
use std::time::Duration;
use tungstenite::{Message, WebSocket};

pub struct AppServer {
    ws: WebSocket<UnixStream>,
    next_id: u64,
}

impl AppServer {
    pub fn connect() -> Result<AppServer> {
        let path = home().join(".codex/app-server-control/app-server-control.sock");
        let stream = UnixStream::connect(&path).with_context(|| format!("connecting to {}", path.display()))?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        let (ws, _) = tungstenite::client("ws://localhost/", stream).map_err(|e| anyhow!("websocket handshake: {e}"))?;
        let mut s = AppServer { ws, next_id: 1 };
        s.call("initialize", json!({ "clientInfo": { "name": "agentchat", "version": env!("CARGO_PKG_VERSION") } }))?;
        s.send(json!({ "jsonrpc": "2.0", "method": "initialized" }))?;
        Ok(s)
    }

    fn send(&mut self, v: Value) -> Result<()> {
        self.ws.send(Message::text(v.to_string()))?;
        Ok(())
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
        loop {
            let Message::Text(t) = self.ws.read()? else { continue };
            let v: Value = serde_json::from_str(&t)?;
            if v.get("id").and_then(Value::as_u64) != Some(id) {
                continue; // notifications and other traffic
            }
            if let Some(e) = v.get("error") {
                bail!("{method}: {}", e.get("message").and_then(Value::as_str).unwrap_or("error"));
            }
            return Ok(v.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    /// Threads currently loaded in the daemon, i.e. open in a codex session.
    pub fn loaded_threads(&mut self) -> Result<Vec<String>> {
        let r = self.call("thread/loaded/list", json!({}))?;
        Ok(r.get("data")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default())
    }

    pub fn start_turn(&mut self, thread: &str, text: &str) -> Result<()> {
        self.call("turn/start", json!({ "threadId": thread, "input": [{ "type": "text", "text": text }] }))?;
        Ok(())
    }
}
