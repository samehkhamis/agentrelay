// The agentchat web client: polls the local server's JSON API and renders the
// same channels, feeds and highlighting as the terminal UI.

const $ = (id) => document.getElementById(id);
const STATE_EVERY = 1500;
const FEED_EVERY = 1000;
const TOAST_FOR = 4000;

let state = { channels: [], statuses: {} };
let current = localStorage.getItem("agentchat.channel") || "all";
let feedVersion = null;
let awaiting = new Set();
let sound = localStorage.getItem("agentchat.sound") !== "off";
let toastTimer = null;
let pending = []; // uploaded files waiting to be sent: {id, name, image}
let shell = false; // shell mode: "!" typed into an empty composer

function api(path, body) {
  const init = { headers: { "X-Agentchat": "1", "Content-Type": "application/json" } };
  if (body !== undefined) Object.assign(init, { method: "POST", body: JSON.stringify(body) });
  return fetch(path, init).then((r) => r.json());
}

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

function toast(text) {
  const t = $("toast");
  t.textContent = text;
  t.style.opacity = 1;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (t.style.opacity = 0), TOAST_FOR);
}

// The same two-note chime as the terminal UI.
function ping() {
  if (!sound) return;
  const ctx = new (window.AudioContext || window.webkitAudioContext)();
  [[880, 0, 0.16], [1318.5, 0.09, 0.24]].forEach(([freq, delay, len]) => {
    const osc = ctx.createOscillator();
    const gain = ctx.createGain();
    osc.frequency.value = freq;
    gain.gain.setValueAtTime(0.18, ctx.currentTime + delay);
    gain.gain.linearRampToValueAtTime(0, ctx.currentTime + delay + len);
    osc.connect(gain).connect(ctx.destination);
    osc.start(ctx.currentTime + delay);
    osc.stop(ctx.currentTime + delay + len);
  });
}

// ---------------------------------------------------------------- state

function channel(id) {
  return state.channels.find((c) => c.id === id);
}

// Follows theme changes made in either UI.
function applyTheme() {
  const picker = $("theme");
  if (picker.options.length !== state.themes.length) {
    picker.replaceChildren(...state.themes.map((n) => Object.assign(el("option", "", n), { value: n })));
  }
  picker.value = state.theme;
  const href = `/theme.css?${encodeURIComponent(state.theme)}`;
  if ($("theme-css").getAttribute("href") !== href) {
    $("theme-css").setAttribute("href", href);
    feedVersion = null; // code highlighting changes with the theme
  }
}

async function refreshState() {
  const prev = state.statuses;
  state = await api("/api/state");
  applyTheme();
  for (const [agent, id] of Object.entries(state.statuses)) {
    if (awaiting.has(agent) && prev[agent] !== undefined && id > prev[agent]) {
      awaiting.delete(agent);
      ping();
      toast(`${agent} replied`);
    }
  }
  if (!channel(current)) current = "all";
  renderSidebar();
  renderHeader();
}

function select(id) {
  document.body.classList.remove("drawer");
  if (id === current) return;
  current = id;
  localStorage.setItem("agentchat.channel", id);
  feedVersion = null;
  $("feed").replaceChildren();
  renderSidebar();
  renderHeader();
  refreshFeed();
  $("input").focus();
}

// ---------------------------------------------------------------- sidebar and header

const SECTIONS = { top: "Channels", projects: "Projects", direct: "Direct messages" };

function presenceDot(presence) {
  return el("span", `dot ${presence || "away"}`);
}

function renderSidebar() {
  $("me").textContent = `${state.me} · ${state.online} online`;
  const unread = state.channels.filter((c) => c.id !== current).reduce((n, c) => n + c.unread, 0);
  $("menu-badge").hidden = !unread;
  $("menu-badge").textContent = unread > 99 ? "99+" : unread;
  const nav = $("channels");
  nav.replaceChildren();
  let section = null;
  for (const c of state.channels) {
    if (c.section !== section) {
      section = c.section;
      const head = el("div", "section", `▾ ${SECTIONS[section]}`);
      if (section === "direct") {
        const add = el("button", "add", "＋");
        Object.assign(add, { type: "button", title: "Start an agent: /new claude|codex [folder] <prompt>" });
        add.onclick = () => {
          document.body.classList.remove("drawer");
          openNewAgent();
        };
        head.append(add);
      }
      nav.append(head);
    }
    const selected = c.id === current;
    const unread = selected ? 0 : c.unread;
    const row = el("div", `row${selected ? " selected" : ""}${unread ? " unread" : ""}`);
    row.append(c.section === "direct" ? presenceDot(c.presence) : el("span", "glyph", "#"));
    row.append(el("span", "name", c.name));
    if (unread) row.append(el("span", "badge", unread));
    else if (c.section === "direct") row.append(el("span", `side k-${c.kind || "claude"}`, c.kind || ""));
    else if (c.section === "projects") row.append(el("span", "side", c.members));
    row.onclick = () => select(c.id);
    nav.append(row);
  }
}

function renderHeader() {
  const c = channel(current);
  if (!c) return;
  $("title-dot").replaceChildren(...(c.section === "direct" ? [presenceDot(c.presence)] : []));
  $("title").textContent = c.title;
  $("detail").textContent = c.detail;
  setPlaceholder();
  document.title = `${c.title} · agentchat`;
}

// ---------------------------------------------------------------- feed

function atBottom() {
  const f = $("feed");
  return f.scrollHeight - f.scrollTop - f.clientHeight < 40;
}

async function refreshFeed() {
  const id = current;
  const q = new URLSearchParams({ channel: id, ...(feedVersion ? { v: feedVersion } : {}) });
  const data = await api(`/api/feed?${q}`);
  if (id !== current || data.error) return;
  if (!data.same) {
    feedVersion = data.v;
    renderFeed(data.posts);
  }
  if (atBottom() && (channel(id)?.unread ?? 0) > 0) api("/api/seen", { channel: id });
}

function renderFeed(posts) {
  const feed = $("feed");
  const stick = atBottom() || !feed.childElementCount;
  const nodes = posts.map(renderPost).flat();
  if (!posts.length) {
    const c = channel(current);
    const hint = c?.section === "direct"
      ? `Nothing from ${c.name} yet.`
      : `This is the start of ${c?.title ?? "this channel"}. Type /status to ask everyone here for an update.`;
    nodes.push(el("div", "empty", hint));
  }
  feed.replaceChildren(...nodes);
  for (const body of feed.querySelectorAll(".body.clamp")) {
    body.classList.toggle("overflowing", body.scrollHeight > body.clientHeight + 2);
  }
  // Pin to the bottom now and again after layout settles (clamps, fonts).
  const settle = () => {
    if (stick) feed.scrollTop = feed.scrollHeight;
    $("jump").hidden = atBottom();
  };
  settle();
  requestAnimationFrame(settle);
}

function renderPost(p) {
  const out = [];
  if (p.new_day) out.push(el("div", "day", p.day));
  const post = el("div", `post${p.new_group ? " first" : ""}${p.subagent ? " sub" : ""}`);
  if (p.new_group) {
    const head = el("div", "post-head");
    head.append(el("span", `author k-${p.kind}`, p.subagent ? `↳ ${p.author}` : p.author));
    if (p.tag) head.append(el("span", "meta", p.tag));
    head.append(el("span", "meta", p.time));
    if (p.note) head.append(el("span", "note", p.note));
    post.append(head);
  }
  if (p.tool) {
    const t = el("div", `tool${p.tool.error ? " error" : ""}`);
    t.append(el("span", "mark", p.tool.error ? "✗" : "▸"), el("span", "tool-name", p.tool.name),
      el("span", "tool-detail", p.tool.detail));
    t.title = p.tool.detail;
    post.append(t);
  } else {
    const body = el("div", `body${p.clamp ? " clamp" : ""}`);
    body.innerHTML = p.html; // rendered and sanitized by the server
    for (const a of body.querySelectorAll("a")) Object.assign(a, { target: "_blank", rel: "noopener noreferrer" });
    post.append(body);
    if (p.clamp === "dm") {
      const more = el("span", "more", "… open the DM for the full reply");
      more.onclick = () => select(`dm:${p.author}`);
      post.append(more);
    } else if (p.clamp === "expand") {
      const more = el("span", "more", "Show more");
      more.onclick = () => body.classList.remove("clamp", "overflowing");
      post.append(more);
    }
  }
  if (p.files?.length) post.append(renderFiles(p.files));
  out.push(post);
  return out;
}

function fileUrl(f) {
  return `/files/${encodeURIComponent(f.id)}`;
}

function renderFiles(files) {
  const box = el("div", "files");
  for (const f of files) {
    const a = el("a", f.image ? "" : "chip");
    Object.assign(a, { href: fileUrl(f), target: "_blank", rel: "noopener" });
    if (f.image) {
      const img = el("img");
      Object.assign(img, { src: fileUrl(f), alt: f.name, loading: "lazy" });
      a.append(img);
    } else {
      a.append("📎 ", el("span", "", f.name));
    }
    box.append(a);
  }
  return box;
}

// ---------------------------------------------------------------- composer

async function submit() {
  const input = $("input");
  const typed = input.value.trim();
  if (!typed && !pending.length) return;
  const text = shell ? `!${typed}` : typed;
  input.value = "";
  setShell(false);
  autosize();
  if (text === "/sound") {
    sound = !sound;
    localStorage.setItem("agentchat.sound", sound ? "on" : "off");
    toast(`sound ${sound ? "on" : "off"}`);
    ping();
    return;
  }
  if (text === "/quit" || text === "/q") return;
  toast(text.startsWith("/new") ? "starting…" : text.startsWith("!") ? "running…" : "sending…");
  const files = pending.map((f) => f.id);
  pending = [];
  renderPending();
  const r = await api("/api/send", { channel: current, text, files });
  if (r.delivered) {
    r.delivered.forEach((a) => awaiting.add(a));
    toast(`delivered to ${r.delivered.length}${r.failed.length ? ` · failed: ${r.failed.join(", ")}` : ""}`);
  } else if (r.note) {
    toast(r.note);
    if (r.theme) await refreshState();
  }
  feedVersion = null;
  refreshFeed();
}

// ---------------------------------------------------------------- attachments

async function upload(fileList) {
  for (const file of fileList) {
    toast(`uploading ${file.name}…`);
    const r = await fetch(`/api/upload?name=${encodeURIComponent(file.name || "pasted.png")}`, {
      method: "POST",
      headers: { "X-Agentchat": "1" },
      body: file,
    }).then((res) => res.json());
    if (r.error) toast(r.error);
    else pending.push(r);
  }
  renderPending();
  if (pending.length) toast(`${pending.length} file(s) ready to send`);
  $("input").focus();
}

function renderPending() {
  // The composer grows or shrinks; keep the feed pinned if it was.
  const stick = atBottom();
  requestAnimationFrame(() => {
    if (stick) $("feed").scrollTop = $("feed").scrollHeight;
    $("jump").hidden = atBottom();
  });
  $("attachments").replaceChildren(...pending.map((f, i) => {
    const chip = el("div", "chip");
    if (f.image) {
      const img = el("img", "thumb");
      img.src = fileUrl(f);
      chip.append(img);
    } else {
      chip.append("📎");
    }
    const remove = el("button", "", "×");
    Object.assign(remove, { type: "button", title: "Remove" });
    remove.onclick = () => {
      pending.splice(i, 1);
      renderPending();
    };
    chip.append(el("span", "", f.name), remove);
    return chip;
  }));
}

const hasFiles = (e) => [...(e.dataTransfer?.types || [])].includes("Files");
let dragDepth = 0;
document.addEventListener("dragenter", (e) => {
  if (!hasFiles(e)) return;
  dragDepth++;
  $("drop").hidden = false;
});
document.addEventListener("dragleave", () => {
  if (--dragDepth <= 0) {
    dragDepth = 0;
    $("drop").hidden = true;
  }
});
document.addEventListener("dragover", (e) => hasFiles(e) && e.preventDefault());
document.addEventListener("drop", (e) => {
  if (!hasFiles(e)) return;
  e.preventDefault();
  dragDepth = 0;
  $("drop").hidden = true;
  upload(e.dataTransfer.files);
});
document.addEventListener("paste", (e) => {
  const files = [...(e.clipboardData?.files || [])];
  if (!files.length) return;
  e.preventDefault();
  upload(files);
});
$("theme").onchange = async () => {
  const r = await api("/api/theme", { name: $("theme").value });
  if (r.note) toast(r.note);
  await refreshState();
  refreshFeed();
};
$("attach").onclick = () => $("file").click();
$("file").onchange = () => {
  upload($("file").files);
  $("file").value = "";
};

// ---------------------------------------------------------------- shell mode

function setShell(on) {
  shell = on;
  $("composer").classList.toggle("shell", on);
  $("shell-mark").hidden = !on;
  setPlaceholder();
}

function setPlaceholder() {
  const c = channel(current);
  if (!c) return;
  $("input").placeholder = !shell
    ? `Message ${c.title} · ! for shell`
    : c.section === "direct"
      ? `Shell command for ${c.name} · Backspace to leave`
      : "Shell commands run in a DM · Backspace to leave";
}

// ---------------------------------------------------------------- new agent

function tilde(path) {
  return state.home && path.startsWith(state.home) ? `~${path.slice(state.home.length)}` : path;
}

function openNewAgent() {
  const c = channel(current);
  // Default to the open DM's or project's folder.
  const here = c?.section === "direct" || c?.section === "projects"
    ? state.folders.find((f) => f.split("/").pop() === (c.section === "projects" ? c.name : c.name.replace(/-[0-9a-f]{2,6}$/, "")))
    : null;
  $("new-folder").replaceChildren(...state.folders.map((f) => Object.assign(el("option", "", tilde(f)), { value: f })));
  if (here) $("new-folder").value = here;
  $("new-prompt").value = "";
  $("new-agent").showModal();
}

$("new-cancel").onclick = () => $("new-agent").close();
$("new-form").onsubmit = async (e) => {
  e.preventDefault();
  const kind = new FormData($("new-form")).get("kind");
  $("new-agent").close();
  toast(`starting ${kind}…`);
  const r = await api("/api/new", { channel: current, kind, folder: $("new-folder").value, prompt: $("new-prompt").value });
  toast(r.note);
};

// ---------------------------------------------------------------- drawer (phones)

$("menu").onclick = () => document.body.classList.toggle("drawer");
$("backdrop").onclick = () => document.body.classList.remove("drawer");

function autosize() {
  const input = $("input");
  input.style.height = "auto";
  input.style.height = `${input.scrollHeight + 2}px`;
}

$("composer").onsubmit = (e) => {
  e.preventDefault();
  submit();
};
$("input").addEventListener("keydown", (e) => {
  const empty = !$("input").value;
  if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
    e.preventDefault();
    submit();
  } else if (e.key === "!" && empty && !shell) {
    // Like Claude Code: "!" on an empty line enters shell mode...
    e.preventDefault();
    setShell(true);
  } else if (e.key === "Backspace" && empty && shell) {
    // ...and Backspace on an empty line leaves it.
    e.preventDefault();
    setShell(false);
  } else if (e.key === "Backspace" && empty && pending.length) {
    pending.pop();
    renderPending();
  }
});
$("input").addEventListener("input", autosize);
$("feed").addEventListener("scroll", () => ($("jump").hidden = atBottom()));
$("jump").onclick = () => {
  const f = $("feed");
  f.scrollTop = f.scrollHeight;
};
document.addEventListener("keydown", (e) => {
  if (!e.altKey || (e.key !== "ArrowUp" && e.key !== "ArrowDown")) return;
  e.preventDefault();
  const i = state.channels.findIndex((c) => c.id === current);
  const n = state.channels.length;
  select(state.channels[(i + (e.key === "ArrowUp" ? -1 : 1) + n) % n].id);
});

// ---------------------------------------------------------------- start

refreshState().then(refreshFeed);
setInterval(() => refreshState().catch(() => toast("server unreachable")), STATE_EVERY);
setInterval(() => refreshFeed().catch(() => {}), FEED_EVERY);
if (matchMedia("(min-width: 761px)").matches) $("input").focus(); // don't pop the phone keyboard
