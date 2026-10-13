# agentchat

A Slack-style chat for the Claude Code and Codex CLI sessions running on your
machine. Every live agent shows up in one place. You can read what each one is
doing, message one agent or a whole channel, start new agents, and run shell
commands for them, from a terminal UI or a web page that stay in sync.

![The web UI: a DM with a Claude agent, a pasted image, subagent activity and file links](docs/screenshots/web-dm.png)

agentchat doesn't install hooks, wrap your sessions or touch your settings. It
finds the agents that are already running and talks to them over their own
local sockets.

## Features

- **Channels.** `#all`, `#claude` and `#codex`, plus a channel for each project.
  Agents are grouped by name prefix, so `webapp-3f` and `webapp-a91c` share
  `#webapp`. Channels hold each agent's latest reply, and `/status` asks everyone
  in a channel for an update.
- **DMs.** One per agent, with its full transcript. You see your prompts,
  replies, tool calls (expand them for the input and output), images, and
  subagent activity, which is marked with a colored bar.
- **Markdown and syntax highlighting** in both UIs, from one shared renderer.
- **Clickable files.** Paths in tool calls and inline code open in a side panel
  with highlighting, at the right line. `/open` fuzzy-finds any file in the
  agent's folder.
- **Markdown preview.** Markdown files open rendered, with their images and
  links to other files. A Preview / Source switch at the top of the panel
  (`Alt+m` in the TUI) shows the highlighted source.
- **Attachments.** Drag, drop or paste images and files into either UI. The
  agent gets their paths.
- **Start agents** with `/new claude|codex [folder] [prompt]` or the **+**
  button. Rename the conversation with `/rename`.
- **Shell mode.** Type `!` to run a command in the agent's folder. Codex runs
  it in the agent's thread. For Claude, agentchat runs it and sends the output to
  the agent.
- **Presence**: busy, online and away, with unread badges and a chime when an
  agent replies.
- **Eight themes**, including terminal favorites. Switching in either UI
  switches both.
- **Phone-friendly web UI**: a swipeable drawer for channels and DMs.
- **Images in the terminal** through
  [ratatui-image](https://github.com/benjajaja/ratatui-image): Kitty, iTerm2 or
  Sixel, with halfblocks as the fallback.

## Install

agentchat is a single Rust binary.

```sh
git clone <this repo> && cd agentchat
cargo build --release
cp target/release/agentchat ~/.local/bin/   # or anywhere on your PATH
```

It needs Rust 1.85 or newer (edition 2024). On Linux, the reply chime uses
PulseAudio when it's available, which includes WSLg. Elsewhere it uses the
default audio output.

## Usage

```
agentchat                      open the TUI
agentchat start [--port N]     run the web UI in the background (default port 3000)
agentchat stop                 stop the background web UI
agentchat serve [--port N]     run the web UI in the foreground
agentchat who                  list discovered agents
agentchat send <agent> <text>  deliver a message to one agent over its socket
agentchat ping                 play the notification sound
agentchat doctor               show which image protocol this terminal gets, and why
```

### Web UI

```sh
agentchat start            # → http://127.0.0.1:3000
agentchat start --port 8080
agentchat stop
```

The server listens on **127.0.0.1 only**, on port **3000** by default. It runs
in the background and keeps its state in `~/.agentchat`. The TUI and any number
of browser tabs can be open at once. They share one database, so channels, read
state, themes and messages stay in sync.

To use it from your phone or another machine, put it on your tailnet:

```sh
tailscale serve --bg 3000
```

The server accepts tailnet requests only from your own Tailscale login (checked
against `tailscaled`), so other people on a shared tailnet can't use it.

![Phone UI](docs/screenshots/phone-dm.png) ![Phone drawer](docs/screenshots/phone-drawer.png)

### Terminal UI

![The TUI with an image, subagent activity and a file open in the side pane](docs/screenshots/tui.png)

| Keys | |
|---|---|
| `Alt+↑` / `Alt+↓` (or `Ctrl+P` / `Ctrl+N`) | previous / next channel or DM |
| `Alt+h` / `Alt+l` | move focus between sidebar, feed and file pane |
| `↑` / `↓` (empty composer) | select a message; `Enter` expands it, `o` opens its files, `y` copies it, `Esc` leaves |
| `PgUp` / `PgDn`, `Esc` | scroll, jump to the latest |
| `Ctrl+O` | pick a file mentioned in this conversation |
| `Ctrl+E` | expand or collapse all tool calls |
| `Ctrl+T` | next theme |
| `Alt+m` | switch a markdown file between preview and source |
| `Alt+←` / `Alt+→` | resize the sidebar (`Alt+Shift` resizes the file pane) |
| `Ctrl+C` / `Ctrl+Q` | quit |

You can also drag the borders with the mouse. Paste or drop a file path to
attach it. Copying uses OSC 52, so it works over SSH too.

### Commands

These work in both UIs and autocomplete as you type.

| | |
|---|---|
| `/status` | ask everyone here for a status update |
| `/new claude\|codex [folder] [prompt]` | start an agent |
| `/rename <name>` | rename this DM's conversation |
| `/open <file>` | open a file from this agent's folder (fuzzy) |
| `/theme [name]` | switch the theme for every UI |
| `/sound` | turn the reply chime on or off |
| `!<command>` | run a shell command for this DM's agent |
| `/quit` | leave agentchat |

## Themes

`slack` (default), `slack-light`, `dracula`, `nord`, `gruvbox`, `tokyo-night`,
`catppuccin`, `solarized`. The syntax highlighting follows the theme.

| | |
|---|---|
| ![slack-light](docs/screenshots/theme-light.png) | ![dracula](docs/screenshots/theme-dracula.png) |
| ![gruvbox, with a file open](docs/screenshots/theme-gruvbox.png) | ![slack, the #all channel](docs/screenshots/web-all.png) |

![A file opened from a link, at the mentioned line](docs/screenshots/web-file.png)

![A markdown file previewed in the file panel](docs/screenshots/web-preview.png)

## How it works

**Claude Code.** Each interactive session writes `~/.claude/sessions/<pid>.json`
with the path of its peer messaging socket, plus a token file. agentchat
connects to the socket, authenticates with the token and sends a
`cross-session-message`, the same envelope Claude sessions use to message each
other. The envelope carries `from-mode="bypass"`, so sessions running with
bypassed permissions accept it without an approval prompt. Transcripts and
subagent logs come from `~/.claude/projects/`.

**Codex.** The Codex app-server listens on
`~/.codex/app-server-control/app-server-control.sock`. agentchat speaks its
JSON-RPC over a WebSocket on that socket to list loaded threads, start turns
(with images), start threads, run shell commands and rename threads. Thread
metadata comes from `~/.codex/state_*.sqlite` and transcripts from the rollout
files.

**One model, two UIs.** A shared hub turns agents, transcripts and the
message log into channels and posts. The TUI draws them with ratatui. The web
server sends the same posts as HTML, rendered by the same markdown and syntect
code. State lives in SQLite (WAL) at `~/.agentchat/chat.db`, so each process
sees what the others do.

**New agents** run with `claude --bg --permission-mode bypassPermissions`, or as
Codex threads with `approvalPolicy: never`, the same mode as sessions you'd
start by hand to work unattended. The folder has to be trusted by Claude first.

## Security

- The web server binds to 127.0.0.1. It accepts only a `localhost` Host header,
  or your tailnet name with your own Tailscale login.
- API calls require a custom header, so other sites can't post to it from your
  browser.
- Agent output is escaped, and unsafe links are dropped.
- The file panel opens only files inside an agent's folder or mentioned in the
  conversation. `/files/` serves only uploads.

Anyone who can use agentchat can run commands as you through your agents, so
treat it like a shell.

## Configuration

| Variable | |
|---|---|
| `AGENTCHAT_USER` | your display name (default: `$USER`) |
| `AGENTCHAT_DB` | database path (default: `~/.agentchat/chat.db`) |
| `AGENTCHAT_SOUND=off` | silence the chime |
| `AGENTCHAT_IMAGES` | force `kitty`, `sixel`, `iterm2`, `halfblocks` or `off` in the TUI |
| `AGENTCHAT_CODEX_HOURS` | how far back to list idle Codex threads (default 24) |
| `AGENTCHAT_AGENTS` | read agents from a JSON file instead of discovering them |

## Demo

The screenshots use made-up agents from [`demo/`](demo). To try it without
touching your real sessions:

```sh
HOME=$PWD/demo/home AGENTCHAT_AGENTS=$PWD/demo/agents.json \
AGENTCHAT_DB=/tmp/agentchat-demo/chat.db AGENTCHAT_USER=alex \
  target/release/agentchat serve --port 3901
```

## Platforms

agentchat is developed on Linux (including WSL). It should build on macOS,
though that isn't tested regularly. Gemini CLI isn't supported because it has
no IPC for a running session.

## License

[MIT](LICENSE)
