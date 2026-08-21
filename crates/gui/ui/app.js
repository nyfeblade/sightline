// The window. It holds no knowledge about sessions — every question and every
// action goes to the engine the terminal view uses, so the two cannot answer
// the same question differently.
const invoke = window.__TAURI__.core.invoke;
const el = (id) => document.getElementById(id);
const clear = (node) => {
  while (node.firstChild) node.removeChild(node.firstChild);
};
// Wiring that tolerates a button having moved. A single missing element used
// to throw at load and leave an empty window, which is a silly way to lose
// everything.
const on = (id, event, run) => {
  const node = el(id);
  if (node) node.addEventListener(event, run);
};
const text = (id, value) => {
  const node = el(id);
  if (node) node.textContent = value;
};
const make = (tag, cls, text) => {
  const node = document.createElement(tag);
  if (cls) node.className = cls;
  if (text !== undefined) node.textContent = text;
  return node;
};

let selected = null;
let pane = "feed";
let feedFilter = "all";
let focused = false;
let liveOnly = false;
let showCost = false;
let typing = false;
let sessions = [];
let dragging = null;

// ── how a number is written ────────────────────────────────────────────────
const tokens = (n) => {
  if (n >= 1e6) return `${(n / 1e6).toFixed(1)}M`;
  if (n >= 1e3) return `${Math.round(n / 1e3)}k`;
  return String(n);
};
const bytes = (n) => {
  if (n >= 1 << 30) return `${(n / (1 << 30)).toFixed(1)} GB`;
  if (n >= 1 << 20) return `${Math.round(n / (1 << 20))} MB`;
  return `${Math.round(n / 1024)} KB`;
};
const age = (s) => {
  // A session with nothing on its record has no age worth printing, and the
  // arithmetic on a missing timestamp produces a century.
  if (s < 0 || s > 3650 * 86400) return "new";
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h`;
  return `${Math.floor(s / 86400)}d`;
};
const clock = (iso) => (iso ? new Date(iso).toTimeString().slice(0, 8) : "");
const ms = (n) => (n >= 1000 ? `${(n / 1000).toFixed(1)}s` : `${n}ms`);

let home = "";
const shortPath = (p) => (home && p.startsWith(home) ? `~${p.slice(home.length)}` : p || "—");
// In a narrow rail the folder's name is the useful part: every session on this
// machine shares the first three segments of its path.
const folderName = (p) => {
  if (!p) return "—";
  if (home && p === home) return "~";
  return p.split("/").filter(Boolean).pop() || p;
};

// A session's state, in the words the interface uses for it.
function condition(s) {
  if (s.asking) return ["needs", "needs you"];
  if (s.state === "running") return ["working", `running ${s.tool || ""}`.trim()];
  if (s.state === "working") return ["working", "working"];
  if (s.state === "waiting") return ["idle", "idle"];
  return ["ended", "ended"];
}

const say = (text) => {
  el("note").textContent = text;
};

// A window that fails silently is a window that lies. Anything thrown lands in
// the status line rather than leaving half the interface undrawn with no
// explanation.
window.addEventListener("error", (e) => say(`${e.message} · ${e.filename}:${e.lineno}`));
window.addEventListener("unhandledrejection", (e) => say(String(e.reason)));
const current = () => sessions.find((s) => s.id === selected);

// ── the session list ───────────────────────────────────────────────────────
function drawAgents() {
  const shown = liveOnly ? sessions.filter((s) => s.live) : sessions;
  text("agent-count", `Sessions (${shown.length})`);
  el("filter").classList.toggle("is-on", liveOnly);
  const list = el("agents");
  clear(list);
  for (const s of shown) {
    const [kind, label] = condition(s);
    const li = make(
      "li",
      `agent${s.id === selected ? " is-on" : ""}${kind === "ended" ? " is-ended" : ""}`,
    );
    li.draggable = true;
    li.dataset.id = s.id;
    li.append(make("i", `dot ${kind}`));
    li.append(make("span", "name", s.name));
    li.append(make("span", "age", age(s.age_secs)));
    const where = make("span", "where");
    where.append(
      kind === "needs" ? make("span", "needs-text", "needs you") : make("span", null, label),
    );
    // The folder is worth a line only when it says something: every session on
    // this machine would otherwise read "~".
    const folder = folderName(s.cwd);
    if (folder !== "~") where.append(document.createTextNode(` · ${folder}`));
    else if (s.branch) where.append(document.createTextNode(` · ${s.branch}`));
    li.append(where);
    li.append(make("span", "ctx", s.window ? tokens(s.context) : ""));
    li.addEventListener("click", () => {
      selected = s.id;
      draw();
      soon();
    });
    list.append(li);
  }
}

// Dragging a session moves it, and the order is remembered between runs.
function wireDragging() {
  const list = el("agents");
  list.addEventListener("dragstart", (e) => {
    const row = e.target.closest(".agent");
    if (!row) return;
    dragging = row.dataset.id;
    row.classList.add("dragging");
  });
  list.addEventListener("dragover", (e) => {
    e.preventDefault();
    const row = e.target.closest(".agent");
    if (!row || row.dataset.id === dragging) return;
    for (const other of list.children) other.classList.remove("over");
    row.classList.add("over");
  });
  list.addEventListener("drop", async (e) => {
    e.preventDefault();
    const row = e.target.closest(".agent");
    if (!row || !dragging) return;
    const ids = sessions.map((s) => s.id);
    const from = ids.indexOf(dragging);
    const to = ids.indexOf(row.dataset.id);
    if (from >= 0 && to >= 0) {
      ids.splice(to, 0, ids.splice(from, 1)[0]);
      await invoke("reorder", { ids });
    }
    dragging = null;
    draw();
  });
  list.addEventListener("dragend", () => {
    dragging = null;
    for (const other of list.children) other.classList.remove("dragging", "over");
  });
}

// ── panes ──────────────────────────────────────────────────────────────────
function eventRow(e) {
  const line = make("div", "row hit");
  line.addEventListener("click", () => {
    el("detail-title").textContent = `${e.tool || e.kind} · ${clock(e.at)}`;
    el("detail-body").textContent = e.body || e.head;
    el("detail-text").showModal();
  });
  line.append(make("span", "at", clock(e.at)));
  const who = e.kind === "prompt" ? "you" : e.kind === "text" ? "claude" : e.kind;
  line.append(make("span", `who ${e.kind}`, who));
  line.append(make("span", "said", e.head));
  return line;
}

function toolCard(e, running) {
  const box = make("div", `card${running ? " pending" : ""}`);
  const head = make("div", "card-head");
  head.append(make("span", null, `${running ? "running · " : ""}${e.tool || "tool"}`));
  head.append(make("span", null, clock(e.at)));
  box.append(head, make("pre", "card-body", e.head));
  return box;
}

const empty = (text) => make("div", "empty", text);

// Follow the feed only if it was already being followed: yanking someone back
// to the bottom while they are reading is worse than not following.
const keepingUp = (out) => out.scrollHeight - out.scrollTop - out.clientHeight < 40;

// The same five the terminal view offers under `f`.
const FILTERS = {
  all: () => true,
  tools: (e) => e.kind === "tool" || e.kind === "result",
  bash: (e) => e.tool === "Bash",
  files: (e) => ["Edit", "Write", "Read", "NotebookEdit"].includes(e.tool),
  talk: (e) => e.kind === "prompt" || e.kind === "text",
};

function drawFilters() {
  const box = el("filters");
  box.hidden = pane !== "feed";
  if (box.hidden) return;
  clear(box);
  for (const name of Object.keys(FILTERS)) {
    const chip = make("span", `chip${name === feedFilter ? " is-on" : ""}`, name);
    chip.addEventListener("click", () => {
      feedFilter = name;
      drawFilters();
      soon(0);
    });
    box.append(chip);
  }
}

async function drawFeed(id) {
  const events = (await invoke("feed", { id, limit: 250 })).filter(FILTERS[feedFilter]);
  const out = el("pane");
  const follow = keepingUp(out);
  clear(out);
  if (!events.length) return out.append(empty("nothing on this session's record yet"));
  const kinds = events.map((e) => e.kind);
  const lastTool = kinds.lastIndexOf("tool");
  const running = lastTool >= 0 && kinds.lastIndexOf("result") < lastTool;
  events.forEach((e, i) => {
    out.append(e.kind === "tool" ? toolCard(e, running && i === lastTool) : eventRow(e));
  });
  out.append(make("div", "end", "END OF STREAM"));
  if (follow) out.scrollTop = out.scrollHeight;
}

// The conversation with the machinery taken out: what was said, by whom.
async function drawRead(id) {
  const events = await invoke("feed", { id, limit: 400 });
  const out = el("pane");
  const follow = keepingUp(out);
  clear(out);
  const talk = events.filter((e) => e.kind === "prompt" || e.kind === "text");
  if (!talk.length) return out.append(empty("nothing said yet"));
  for (const e of talk) {
    const block = make("div", "row");
    block.append(make("span", "at", clock(e.at)));
    block.append(make("span", `who ${e.kind}`, e.kind === "prompt" ? "you" : "claude"));
    block.append(make("span", "said", e.body || e.head));
    out.append(block);
  }
  if (follow) out.scrollTop = out.scrollHeight;
}

async function drawFiles(id) {
  const files = await invoke("files", { id });
  const out = el("pane");
  clear(out);
  if (!files.length) return out.append(empty("no files touched yet"));
  for (const f of files) {
    const line = make("div", "line");
    line.append(make("span", "path", shortPath(f.path)));
    const right = make("span", "num");
    right.append(make("span", null, `${f.reads}r ${f.edits + f.writes}w  `));
    right.append(make("span", "added", `+${f.added}`));
    right.append(make("span", "removed", ` −${f.removed}`));
    line.append(right);
    out.append(line);
  }
}

async function drawTree(id) {
  const tree = await invoke("tree", { id });
  const out = el("pane");
  clear(out);
  if (!tree) return out.append(empty("this session is not inside a git repository"));
  const head = make("div", "line");
  head.append(make("span", "path", `on ${tree.branch}`));
  const sum = make("span", "num");
  sum.append(make("span", "added", `+${tree.insertions}`));
  sum.append(make("span", "removed", ` −${tree.deletions}`));
  head.append(sum);
  out.append(head);
  if (tree.ahead !== null && tree.ahead !== undefined) {
    const iso = make("div", "line");
    iso.append(make("span", "path", `${tree.ahead} commits ahead of ${tree.base}`));
    iso.append(make("span", "num", "its own checkout"));
    out.append(iso);
  }
  if (!tree.entries.length) return out.append(empty("working tree clean"));
  for (const e of tree.entries) {
    const line = make("div", "line");
    line.append(make("span", "path", e.path));
    line.append(make("span", "num", e.state));
    out.append(line);
  }
}

async function drawPlan(id) {
  const todos = await invoke("plan", { id });
  const out = el("pane");
  clear(out);
  if (!todos.length) return out.append(empty("no plan written"));
  for (const t of todos) {
    const state = t.state === "completed" ? "done" : t.state === "in_progress" ? "doing" : "";
    const line = make("div", `line todo ${state}`);
    const mark = state === "done" ? "✓" : state === "doing" ? "▸" : "○";
    line.append(make("span", "what", `${mark}  ${t.text}`));
    out.append(line);
  }
}

async function drawSubagents(id) {
  const runs = await invoke("agents", { id });
  const out = el("pane");
  clear(out);
  if (!runs.length) return out.append(empty("no subagents launched"));
  for (const a of runs) {
    const line = make("div", "line");
    line.append(make("span", "path", `${a.kind || "agent"} · ${a.description}`));
    line.append(make("span", "num", `${a.model} · ${a.state}`));
    out.append(line);
  }
}

function statLine(out, key, value) {
  const line = make("div", "line");
  line.append(make("span", "path", key));
  line.append(make("span", "num", value));
  out.append(line);
}

function drawStats(s) {
  const out = el("pane");
  clear(out);
  statLine(out, "turns", String(s.turns));
  statLine(out, "requests", String(s.requests));
  statLine(out, "context", s.window ? `${tokens(s.context)} of ${tokens(s.window)}` : tokens(s.context));
  statLine(out, "output", tokens(s.output));
  statLine(out, "input", tokens(s.input));
  statLine(out, "cache", `${tokens(s.cache_read)} read · ${tokens(s.cache_write)} written`);
  statLine(out, "if run on the API", `~$${s.cost.toFixed(2)}`);
  statLine(out, "tool calls", `median ${ms(s.latency[0])} · slowest ${ms(s.latency[1])}`);
  statLine(out, "errors · refusals", `${s.errors} · ${s.denials}`);
  if (s.model) statLine(out, "model", s.model + (s.effort ? ` · ${s.effort}` : ""));
  if (s.version) statLine(out, "client", s.version);
}

async function drawErrors(id) {
  const errors = await invoke("errors", { id });
  const out = el("pane");
  clear(out);
  if (!errors.length) return out.append(empty("nothing has gone wrong"));
  for (const e of errors) out.append(eventRow(e));
}

// How wide a character actually is in this font at this size, measured rather
// than guessed: a column count off by one wraps every line in the wrong place.
let cell = null;
function cellSize() {
  if (cell) return cell;
  const ruler = make("div", "screen", "M".repeat(100));
  ruler.style.cssText = "position:absolute;visibility:hidden;white-space:pre";
  document.body.append(ruler);
  const box = ruler.getBoundingClientRect();
  cell = { w: box.width / 100, h: box.height };
  ruler.remove();
  return cell;
}

// The session's own screen, cell by cell, at the size of this panel — and it
// takes the keyboard, so this is the session itself rather than a picture of
// one.
let lastFrame = [];
async function drawMirror(id) {
  const out = el("pane");
  const { w, h } = cellSize();
  const cols = Math.max(40, Math.floor((out.clientWidth - 26) / w));
  const rows = Math.max(10, Math.floor((out.clientHeight - 20) / h));
  const frame = await invoke("frame", { id, cols, rows });
  if (!frame) {
    clear(out);
    lastFrame = [];
    return out.append(empty("Ironsight has no terminal for this session"));
  }
  let screen = out.querySelector(".screen");
  if (!screen || lastFrame.length !== frame.lines.length) {
    clear(out);
    screen = make("div", "screen");
    out.append(screen);
    lastFrame = [];
  }
  frame.lines.forEach((line, y) => {
    // A line that has not changed is left alone: the browser then has nothing
    // to lay out, and typing stops competing with the redraw.
    const shape = JSON.stringify(line);
    if (lastFrame[y] === shape && screen.children[y]) return;
    lastFrame[y] = shape;
    const div = make("div");
    for (const run of line) {
      const span = run.bold ? make("b") : make("span");
      span.textContent = run.text;
      if (run.fg) span.style.color = run.fg;
      if (run.bg) span.style.background = run.bg;
      if (run.dim) span.style.opacity = ".62";
      if (run.italic) span.style.fontStyle = "italic";
      if (run.underline) span.style.textDecoration = "underline";
      if (run.inverse) {
        span.style.background = run.fg || "var(--body)";
        span.style.color = run.bg || "var(--ground)";
      }
      div.append(span);
    }
    if (screen.children[y]) screen.replaceChild(div, screen.children[y]);
    else screen.append(div);
  });
  // The caret belongs to the session, so it is drawn where the session says.
  if (frame.cursor_visible) {
    const [cy, cx] = frame.cursor;
    const row = screen.children[cy];
    if (row) {
      const text = row.textContent.padEnd(cx + 1, " ");
      if (!row.querySelector(".caret")) {
        const before = make("span", null, text.slice(0, cx));
        const at = make("span", "caret", text[cx] === " " ? " " : text[cx]);
        const after = make("span", null, text.slice(cx + 1));
        const styled = row.cloneNode(true);
        clear(row);
        row.append(before, at, after);
        row.dataset.caret = "1";
        lastFrame[cy] = null;
        void styled;
      }
    }
  }
  while (screen.children.length > frame.lines.length) screen.lastChild.remove();
}

async function drawFleet() {
  const [path, text] = await invoke("fleet");
  const out = el("pane");
  clear(out);
  const head = make("div", "line");
  head.append(make("span", "path", shortPath(path)));
  const go = make("button", "ghost", "Launch all of it");
  go.addEventListener("click", async () => say(await invoke("launch_fleet")));
  head.append(go);
  out.append(head);
  if (!text.trim()) {
    return out.append(
      empty("no fleet file yet — a JSON array of sessions to start together"),
    );
  }
  let entries = [];
  try {
    entries = JSON.parse(text);
  } catch {
    return out.append(empty("that file is not a JSON array"));
  }
  for (const e of entries) {
    const line = make("div", "line");
    line.append(make("span", "path", `${e.cwd || "."}${e.worktree ? ` · ${e.worktree}` : ""}`));
    line.append(make("span", "num", [e.agent, e.model, e.effort, e.permission_mode].filter(Boolean).join(" · ")));
    out.append(line);
    if (e.prompt) out.append(make("div", "empty", `“${e.prompt}”`));
  }
}

const painters = {
  feed: (s) => drawFeed(s.id),
  read: (s) => drawRead(s.id),
  mirror: (s) => drawMirror(s.id),
  files: (s) => drawFiles(s.id),
  tree: (s) => drawTree(s.id),
  plan: (s) => drawPlan(s.id),
  agents: (s) => drawSubagents(s.id),
  stats: (s) => drawStats(s),
  errors: (s) => drawErrors(s.id),
  fleet: () => drawFleet(),
};

// ── the selected session, and what can be done to it ───────────────────────
function fact(parent, key, value) {
  const line = make("div", "fact");
  line.append(make("span", "k", key));
  line.append(make("span", "v", value));
  parent.append(line);
}

function action(parent, label, cls, run) {
  const button = make("button", cls, label);
  button.addEventListener("click", run);
  parent.append(button);
}

function drawDetail(s) {
  const box = el("detail");
  clear(box);
  if (!s) return;
  box.dataset.for = s.id;
  const [, label] = condition(s);
  box.append(make("h3", null, s.name));
  box.append(make("div", "sub", `${label} · ${shortPath(s.cwd)}`));

  fact(box, "model", s.model || "—");
  fact(box, "branch", s.branch || "—");
  fact(box, "held in", s.pane || "not steerable");
  // Two different questions, and reporting one of them as "age" made a busy
  // session look like it kept restarting.
  fact(box, "started", s.started_secs >= 0 ? `${age(s.started_secs)} ago` : "—");
  fact(box, "last active", `${age(s.age_secs)} ago`);

  box.append(make("div", "group", "CONTEXT"));
  const track = make("div", "bar-line");
  const fill = make("i");
  fill.style.width = `${s.window ? Math.min(100, (s.context / s.window) * 100) : 0}%`;
  track.append(fill);
  box.append(track);
  box.append(make("div", "sub", s.window ? `${tokens(s.context)} of ${tokens(s.window)}` : "—"));

  // Not the machine's numbers — what this one session is costing it. The
  // heading used to say MACHINE, which invited exactly the wrong reading.
  box.append(make("div", "group", "THIS SESSION IS USING"));
  const share = s.cpu === null || s.cpu === undefined ? null : s.cpu / (s.cores || 1);
  fact(box, "processor", share === null ? "—" : `${share.toFixed(1)}% of ${s.cores} cores`);
  fact(box, "memory", s.memory ? bytes(s.memory) : "—");

  if (s.tools.length) {
    box.append(make("div", "group", "REACHES FOR"));
    const chips = make("div", "chips");
    for (const t of s.tools) chips.append(make("span", "chip", t));
    box.append(chips);
  }

  invoke("queued", { id: s.id }).then((waiting) => {
    if (!waiting.length || el("detail").dataset.for !== s.id) return;
    const group = make("div", "group", "WAITING FOR IT TO GO IDLE");
    el("detail").insertBefore(group, el("detail").querySelector(".actions"));
    for (const line of waiting) {
      const item = make("div", "queued");
      item.append(make("b", null, "› "));
      item.append(document.createTextNode(line));
      el("detail").insertBefore(item, el("detail").querySelector(".actions"));
    }
  });

  const actions = make("div", "actions");
  if (s.steerable) {
    action(actions, "Window", "ghost", async () => {
      try {
        say(`opened in ${await invoke("window", { id: s.id })}`);
      } catch (e) {
        say(String(e));
      }
    });
    action(actions, "Rename", "ghost", async () => {
      const name = await ask("Call this session:", s.name);
      if (!name) return;
      try {
        await invoke("rename", { id: s.id, name });
        say(`renamed to ${name}`);
      } catch (e) {
        say(String(e));
      }
      draw();
    });
    action(actions, "Branch", "ghost", async () => {
      const branch = await ask("Start a session on its own branch, called:");
      if (!branch) return;
      say(await invoke("isolate", { id: s.id, branch }));
      draw();
    });
    if (s.branch && s.branch !== "master" && s.branch !== "main") {
      action(actions, "Merge back", "ghost", async () => {
        say(await invoke("merge", { id: s.id }));
        draw();
      });
      action(actions, "Discard checkout", "ghost danger", async () => {
        say(await invoke("discard", { id: s.id }));
        draw();
      });
    }
    action(actions, "Close", "ghost danger", async () => {
      try {
        await invoke("stop", { id: s.id });
        say("closed — bring it back from Resume");
      } catch (e) {
        say(String(e));
      }
      draw();
    });
  } else {
    action(actions, "Reopen here", "primary", async () => {
      try {
        await invoke("reopen", { id: s.id, cwd: s.cwd });
        say("reopening…");
      } catch (e) {
        say(String(e));
      }
      draw();
    });
  }
  box.append(actions);
}

// ── what is waiting on you ─────────────────────────────────────────────────
function drawAsk() {
  const waiting =
    sessions.find((s) => s.id === selected && s.asking) || sessions.find((s) => s.asking);
  const bar = el("ask");
  bar.hidden = !waiting;
  if (!waiting) return;
  text("ask-who", waiting.name);
  text("ask-what", waiting.asking.question);
  const answers = el("answers");
  clear(answers);
  waiting.asking.options.forEach((option, i) => {
    const button = make("button", `answer${i === 0 ? " first" : ""}`);
    button.append(make("span", "key", i === 0 ? "y" : i === 1 ? "n" : String(i + 1)));
    button.append(document.createTextNode(option.replace(/^\d+\.\s*/, "")));
    button.addEventListener("click", async () => {
      try {
        await invoke("answer", { id: waiting.id, option: i + 1 });
        say(`answered ${option}`);
      } catch (e) {
        say(String(e));
      }
      draw();
    });
    answers.append(button);
  });
}

// ── the whole window ───────────────────────────────────────────────────────
async function draw() {
  sessions = await invoke("sessions");
  if (!home) {
    home = sessions.find((s) => s.cwd.startsWith("/home/"))?.cwd.match(/^\/home\/[^/]+/)?.[0] || "";
  }
  if (!sessions.some((s) => s.id === selected)) {
    selected =
      (sessions.find((s) => s.asking) || sessions.find((s) => s.live) || sessions[0])?.id ?? null;
  }
  const s = current();

  const working = sessions.filter((x) => x.state === "working" || x.state === "running").length;
  const asking = sessions.filter((x) => x.asking).length;
  const counts = el("counts");
  clear(counts);
  counts.append(document.createTextNode(`${sessions.length} sessions · ${working} working · `));
  counts.append(asking ? make("b", null, `${asking} need you`) : document.createTextNode("none waiting"));

  const out = sessions.reduce((n, x) => n + x.output, 0);
  const spend = sessions.reduce((n, x) => n + x.cost, 0);
  const requests = sessions.reduce((n, x) => n + x.requests, 0);
  text("spend", showCost
    ? `${tokens(out)} out · ~$${spend.toFixed(2)} if API`
    : `${tokens(out)} out · ${requests} requests`);
  text("context", s ? s.name : "");

  const canType = !!s && s.steerable;
  el("message").disabled = !canType;
  el("message").placeholder = canType ? "Message this session…" : "not steerable from here";

  drawAgents();
  drawDetail(s);
  drawAsk();
  drawFilters();
  if (!s) {
    clear(el("pane"));
    el("pane").append(empty("no sessions yet — start one"));
  }
}

// ── the things you can press ───────────────────────────────────────────────
on("panes", "click", (e) => {
  const tab = e.target.closest(".tab");
  if (!tab) return;
  if (pane === "mirror" && tab.dataset.pane !== "mirror" && selected) {
    // Stop holding the session at this window's shape.
    invoke("release_frame", { id: selected });
    typing = false;
    el("pane").classList.remove("typing");
  }
  pane = tab.dataset.pane;
  for (const other of el("panes").querySelectorAll(".tab")) {
    other.classList.toggle("is-on", other === tab);
  }
  draw();
  soon(0);
});

on("filter", "click", () => {
  liveOnly = !liveOnly;
  draw();
});

on("cost", "click", () => {
  showCost = !showCost;
  draw();
});

on("composer", "submit", async (e) => {
  e.preventDefault();
  const box = el("message");
  const text = box.value.trim();
  if (!text || !selected) return;
  try {
    await invoke("send", { id: selected, text });
    box.value = "";
    say("sent");
    // Stay where you are, the way a prompt does: sending one message is
    // usually the start of a conversation rather than the end of one.
    box.focus();
    soon();
  } catch (err) {
    say(String(err));
  }
});

on("interrupt", "click", async () => {
  if (!selected) return;
  try {
    await invoke("interrupt", { id: selected });
    say("interrupted");
  } catch (e) {
    say(String(e));
  }
});

on("new", "click", () => {
  el("start-path").value = current()?.cwd || "";
  el("starter").showModal();
});

on("start-form", "submit", async (e) => {
  if (e.submitter && e.submitter.value === "cancel") return;
  const value = (id) => el(id).value.trim();
  const agent = el("start-agent").value;
  let line = value("start-path") || ".";
  if (agent && agent !== "claude") line += ` --agent ${agent}`;
  if (value("start-model")) line += ` --model ${value("start-model")}`;
  if (value("start-effort")) line += ` --effort ${value("start-effort")}`;
  if (value("start-mode")) line += ` --mode ${value("start-mode")}`;
  const branch = value("start-branch");
  const first = value("start-prompt");
  if (first) line += ` ${first}`;
  try {
    // A branch means its own checkout, which is a different way to start.
    if (branch) {
      const id = current()?.id;
      say(id ? await invoke("isolate", { id, branch }) : "select a session in that repository first");
    } else {
      say(`started ${await invoke("start", { line, name: value("start-name") || null })}`);
    }
  } catch (err) {
    say(String(err));
  }
  el("start-prompt").value = "";
  draw();
});

// Every conversation on the machine, to bring one back.
let history = [];
function drawPast() {
  const words = el("past-filter").value.toLowerCase().split(/\s+/).filter(Boolean);
  const list = el("past-list");
  clear(list);
  const hits = history.filter((p) => {
    const hay = `${p.title} ${p.cwd}`.toLowerCase();
    return words.every((word) => hay.includes(word));
  });
  for (const p of hits.slice(0, 200)) {
    const row = make("li", "past-row");
    row.append(make("span", "when", age(p.age_secs)));
    const title = make("span", "title");
    if (p.open) title.append(make("span", "open", "● "));
    title.append(document.createTextNode(p.title));
    row.append(title);
    row.append(make("span", "folder", shortPath(p.cwd)));
    row.addEventListener("click", async () => {
      el("past").close();
      try {
        await invoke("reopen", { id: p.id, cwd: p.cwd });
        say("reopening…");
      } catch (e) {
        say(String(e));
      }
      draw();
    });
    list.append(row);
  }
  if (!hits.length) list.append(empty("nothing matches that"));
}

on("resume", "click", async () => {
  el("past-filter").value = "";
  history = await invoke("past");
  drawPast();
  el("past").showModal();
  el("past-filter").focus();
});
on("past-filter", "input", drawPast);
on("past-close", "click", () => el("past").close());

// Typing into the session's own screen: click it, and the keyboard belongs to
// the session until you click away.
on("pane", "click", () => {
  if (pane !== "mirror") return;
  typing = true;
  el("pane").focus();
  el("pane").classList.add("typing");
  say("typing into the session · click away to stop");
});
on("pane", "blur", () => {
  typing = false;
  el("pane").classList.remove("typing");
});
on("pane", "keydown", (e) => {
  if (!typing || !selected) return;
  e.preventDefault();
  // Send and move on. Waiting for the round trip, and then for a redraw, made
  // every key cost the sum of both — which is what "delayed" was.
  invoke("key", { id: selected, key: e.key, ctrl: e.ctrlKey }).catch((err) => say(String(err)));
  // The session redraws a few milliseconds after the key lands, so look twice:
  // once almost immediately, once after it has had time to paint.
  soon(10);
  soon(45);
});

on("search", "keydown", async (e) => {
  if (e.key !== "Enter") return;
  const text = el("search").value.trim();
  if (!text) return;
  const hits = await invoke("search", { text });
  el("hits-title").textContent = `${hits.length} matches for “${text}”`;
  const list = el("hits-list");
  clear(list);
  for (const hit of hits) {
    const row = make("li", "past-row");
    row.append(make("span", "when", clock(hit.at)));
    row.append(make("span", "title", hit.head));
    row.append(make("span", "folder", hit.session));
    row.addEventListener("click", () => {
      el("hits").close();
      selected = hit.id;
      pane = "feed";
      for (const tab of el("panes").querySelectorAll(".tab")) {
        tab.classList.toggle("is-on", tab.dataset.pane === "feed");
      }
      draw();
      soon(0);
    });
    list.append(row);
  }
  if (!hits.length) list.append(empty("nothing said that anywhere"));
  el("hits").showModal();
});
on("hits-close", "click", () => el("hits").close());

// The one thing that cannot make progress without you, wherever it is.
on("waiting", "click", () => {
  const order = sessions.map((s) => s.id);
  const from = order.indexOf(selected);
  const next = sessions
    .slice(from + 1)
    .concat(sessions.slice(0, from + 1))
    .find((s) => s.asking);
  if (!next) return say("nothing is waiting on you");
  selected = next.id;
  draw();
  soon(0);
});

on("queue", "click", async () => {
  const box = el("message");
  const text = box.value.trim();
  if (!text || !selected) return;
  try {
    const n = await invoke("queue", { id: selected, text });
    box.value = "";
    say(`queued — ${n} waiting for the next idle moment`);
  } catch (e) {
    say(String(e));
  }
  draw();
});

// Fill the window with the session. A terminal cannot do this without losing
// everything else; here it is one key, and one key back.
function toggleFocus() {
  focused = !focused;
  document.body.classList.toggle("focused", focused);
  if (focused && pane !== "mirror") {
    pane = "mirror";
    for (const tab of el("panes").querySelectorAll(".tab")) {
      tab.classList.toggle("is-on", tab.dataset.pane === "mirror");
    }
  }
  soon(0);
}
on("focus", "click", toggleFocus);

// y and n answer whatever is waiting, as they do in the terminal view.
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape" && focused) return toggleFocus();
  if (typing || e.target.matches("input, select, textarea")) return;
  const answers = el("answers");
  if (e.key === "y" && answers.children[0]) return answers.children[0].click();
  if (e.key === "n" && answers.children[1]) return answers.children[1].click();
  if (e.key === "/") {
    e.preventDefault();
    return el("search").focus();
  }
  if (e.key === "f") return toggleFocus();
  if (e.key === "n") return el("new").click();
  if (e.key === "r") return el("resume").click();
  if (e.key === "Escape" && selected) return el("interrupt").click();
  // The panes, left to right, as the numbers do in the terminal view.
  const tabs = [...el("panes").querySelectorAll(".tab")];
  const n = Number(e.key);
  if (n >= 1 && n <= tabs.length) tabs[n - 1].click();
});

// A question with a text answer. `prompt()` is blocked in a webview, and a
// dialog can look like the rest of this instead of like 1998.
function ask(title, value = "") {
  return new Promise((resolve) => {
    el("asking-title").textContent = title;
    const box = el("asking-input");
    box.value = value;
    const done = (answer) => {
      el("asking").close();
      el("asking-ok").onclick = null;
      el("asking-cancel").onclick = null;
      box.onkeydown = null;
      resolve(answer);
    };
    el("asking-ok").onclick = () => done(box.value.trim());
    el("asking-cancel").onclick = () => done(null);
    box.onkeydown = (e) => {
      if (e.key === "Enter") done(box.value.trim());
      if (e.key === "Escape") done(null);
    };
    el("asking").showModal();
    box.focus();
    box.select();
  });
}

// ── the accent ────────────────────────────────────────────────────────────
// A session lets you pick its colours; so does this. The accent is the only
// colour that carries meaning here, so it is the only one worth choosing.
const ACCENTS = ["#d99a4e", "#c08542", "#6fbf73", "#6aa3c8", "#a98bc7", "#d3675a", "#c9cdd6"];

function dim(hex, amount) {
  const n = parseInt(hex.slice(1), 16);
  const mix = (c) => Math.round(c * amount);
  return `#${[(n >> 16) & 255, (n >> 8) & 255, n & 255].map((c) => mix(c).toString(16).padStart(2, "0")).join("")}`;
}

function useAccent(hex) {
  document.documentElement.style.setProperty("--gold", hex);
  document.documentElement.style.setProperty("--gold-dim", dim(hex, 0.6));
  localStorage.setItem("accent", hex);
  for (const s of el("swatches").children) s.classList.toggle("is-on", s.dataset.hex === hex);
}

function drawSwatches() {
  const box = el("swatches");
  clear(box);
  for (const hex of ACCENTS) {
    const swatch = make("button", "swatch");
    swatch.dataset.hex = hex;
    swatch.style.background = hex;
    swatch.addEventListener("click", () => useAccent(hex));
    box.append(swatch);
  }
}

let notifyOn = true;
function drawMenu() {
  const grid = el("menu-grid");
  clear(grid);
  const item = (label, note, run) => {
    const button = make("button");
    button.append(document.createTextNode(label));
    button.append(make("small", null, note));
    button.addEventListener("click", async () => {
      el("menu").close();
      await run();
      draw();
    });
    grid.append(button);
  };
  item("Broadcast…", "say one thing to every session", async () => {
    const text = await ask("Send to every session Ironsight can reach:");
    if (text) say(`sent to ${await invoke("broadcast", { text })} sessions`);
  });
  item("Launch the fleet", "start everything the fleet file describes", async () => {
    say(await invoke("launch_fleet"));
  });
  item("Tidy up", "close sessions whose process has exited", async () => {
    const closed = await invoke("prune");
    say(closed.length ? `closed ${closed.join(", ")}` : "nothing to tidy up");
  });
  item("Close everything", "every session Ironsight started", async () => {
    const closed = await invoke("close_all");
    say(`closed ${closed.length} — each reopens from Resume`);
  });
  item(notifyOn ? "Notifications on" : "Notifications off", "when a session needs you", async () => {
    notifyOn = await invoke("notifications", { on: !notifyOn });
    say(notifyOn ? "notifications on" : "notifications off");
  });
  item("Look again", "rescan now rather than at the next beat", async () => {
    await invoke("rescan");
    say("rescanned");
  });
  item("Terminal view", "drive all of this from a terminal", async () => {
    say(`opened in ${await invoke("open_tui")}`);
  });
  item("Keys", "what the keyboard does here", async () => {
    el("detail-title").textContent = "Keys";
    el("detail-body").textContent = [
      "1 … 9      the panes, left to right",
      "n          new session",
      "/          search every session",
      "r          resume a conversation",
      "f          fill the window with the session",
      "y / n      accept or decline what is being asked",
      "enter      send the message you have typed",
      "escape     interrupt the selected session",
      "click the session's screen to type straight into it",
      "drag a session in the list to move it",
    ].join("\n");
    el("detail-text").showModal();
  });
}

on("more", "click", () => {
  drawMenu();
  drawSwatches();
  useAccent(localStorage.getItem("accent") || ACCENTS[0]);
  el("menu").showModal();
});
on("custom-accent", "input", (e) => useAccent(e.target.value));
on("menu-close", "click", () => el("menu").close());
on("detail-close", "click", () => el("detail-text").close());
useAccent(localStorage.getItem("accent") || ACCENTS[0]);

el("hint").textContent = "1…9 panes · n new · / search · f fill · y accept · ⋯ for the rest";
wireDragging();

// Two rhythms. Everything around the edges — the list, the counts, the detail —
// changes slowly and is redrawn slowly. The pane in the middle is whatever you
// are actually watching, and when that is a session's own screen it has to keep
// up with the session.
const pending = new Set();
function soon(delay = 12) {
  if (pending.has(delay)) return;
  pending.add(delay);
  setTimeout(async () => {
    pending.delete(delay);
    const s = current();
    if (s) await painters[pane](s);
  }, delay);
}

async function paneTick() {
  const s = current();
  if (s) {
    try {
      await painters[pane](s);
    } catch (e) {
      say(String(e));
    }
  }
  setTimeout(paneTick, pane === "mirror" ? (typing ? 55 : 140) : 500);
}

draw();
paneTick();
setInterval(draw, 900);
