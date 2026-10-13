mod agents;
mod db;
mod highlight;
mod hub;
mod ipc;
mod sound;
mod theme;
mod transcript;
mod tui;
mod uploads;
mod web;

use anyhow::{bail, Result};

const USAGE: &str = "agentchat — a Slack-style chat for your Claude Code and Codex agents

usage:
  agentchat                      open the TUI
  agentchat start [--port N]     run the web UI in the background (default port 3000);
                                 reachable from your tailnet via `tailscale serve`
  agentchat stop                 stop the background web UI
  agentchat serve [--port N]     run the web UI in the foreground
  agentchat who                  list discovered agents
  agentchat send <agent> <text>  deliver a message to one agent over its socket
  agentchat ping                 play the notification sound
  agentchat doctor               show which image protocol this terminal gets, and why";

fn main() -> Result<()> {
    sound::init_env();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => tui::run(),
        Some("start") => {
            let port = web::start(port_arg(&args)?)?;
            println!("agentchat is running at http://127.0.0.1:{port}");
            Ok(())
        }
        Some("stop") => {
            match web::stop()? {
                true => println!("agentchat stopped"),
                false => println!("agentchat isn't running"),
            }
            Ok(())
        }
        Some("serve") => web::serve(port_arg(&args)?),
        Some("who") => {
            for a in agents::discover() {
                println!("{:<28} {:<7} #{:<22} {:<7} {}", a.name, a.kind.as_str(), a.group, a.presence.label(), a.cwd);
            }
            Ok(())
        }
        Some("send") if args.len() >= 3 => {
            let Some(agent) = agents::discover().into_iter().find(|a| a.name == args[1]) else {
                bail!("no agent named {}", args[1]);
            };
            let report = ipc::deliver(std::slice::from_ref(&agent), &args[2..].join(" "), &[]);
            if let Some((_, why)) = report.failed.first() {
                bail!("delivery to {} failed: {why}", agent.name);
            }
            println!("delivered to {}", agent.name);
            Ok(())
        }
        Some("doctor") => {
            doctor();
            Ok(())
        }
        Some("ping") => {
            sound::ping();
            std::thread::sleep(std::time::Duration::from_millis(1500));
            Ok(())
        }
        _ => {
            println!("{USAGE}");
            Ok(())
        }
    }
}

/// `--port N`, or the default.
fn port_arg(args: &[String]) -> Result<u16> {
    match args.iter().position(|a| a == "--port") {
        None => Ok(web::DEFAULT_PORT),
        Some(i) => match args.get(i + 1).and_then(|p| p.parse().ok()) {
            Some(p) => Ok(p),
            None => bail!("--port needs a port number"),
        },
    }
}

/// Reports terminal image support, run in the terminal in question.
fn doctor() {
    ratatui::crossterm::terminal::enable_raw_mode().ok();
    let images = tui::images::Images::detect();
    ratatui::crossterm::terminal::disable_raw_mode().ok();
    match images {
        Some(im) => println!("{}", im.describe()),
        None => println!("images: off (AGENTCHAT_IMAGES)"),
    }
    for k in ["AGENTCHAT_IMAGES", "TERM", "TERM_PROGRAM", "TERM_PROGRAM_VERSION", "WEZTERM_PANE", "WEZTERM_EXECUTABLE", "TMUX", "WSL_DISTRO_NAME", "HERDR_ENV"] {
        if let Ok(v) = std::env::var(k) {
            println!("{k}={v}");
        }
    }
}
