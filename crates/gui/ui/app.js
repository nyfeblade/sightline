// The window. It holds no knowledge about sessions — every question and every
// action goes to the engine, which the terminal view uses too.
const invoke = window.__TAURI__.core.invoke;
const el = (id) => document.getElementById(id);
const clear = (node) => {
  while (node.firstChild) node.removeChild(node.firstChild);
};

let selected = null;
let pane = "feed";
let liveOnly = false;
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

// Home is written the way it is everywhere else.
let home = "";
const shortPath = (p) => (home && p.startsWith(home) ? `~${p.slice(home.length)}` : p || "—");

// A session's state, in the words the interface uses for it.
function condition(s) {
  if (s.asking) return ["needs", "Needs Permission"];
  if (s.state === "running") return ["working", `Running ${s.tool || ""}`.trim()];
  if (s.state === "working") return ["working", "Processing"];
  if (s.state === "waiting") return ["idle", "Idle"];
  return ["ended", "Ended"];
}

const say = (text) => {
  el("note").textContent = text;
};

// ── the list ───────────────────────────────────────────────────────────────
function drawAgents() {
  const shown = liveOnly ? sessions.filter((s) => s.live) : sessions;
  el("agent-count").textContent = `Active Agents (${shown.length})`;
  const list = el("agents");
  clear(list);
  for (const s of shown) {
    const [kind, label] = condition(s);
    const li = document.createElement("li");
    li.className = `agent${s.id === selected ? " is-on" : ""}`;
    li.draggable = true;
    li.dataset.id = s.id;

    const top = document.createElement("div");
    top.className = "agent-top";
    const name = document.createElement("span");
    name.className = "agent-name";
    name.textContent = s.name;
    const when = document.createElement("span");
    when.className = "agent-age";
    when.textContent = age(s.age_secs);
    top.append(name, when);

    const state = document.createElement("div");
    state.className = "agent-state";
    const dot = document.createElement("i");
    dot.className = `dot ${kind}`;
    const what = document.createElement("span");
    what.className = `state-text ${kind}`;
    what.textContent = label;
    state.append(dot, what);

    const foot = document.createElement("div");
    foot.className = "agent-foot";
    const where = document.createElement("span");
    where.className = "agent-where";
    where.textContent = shortPath(s.cwd);
    const ctx = document.createElement("span");
    ctx.className = "agent-ctx";
    ctx.textContent = s.window ? `${tokens(s.context)} / ${tokens(s.window)}` : "";
    foot.append(where, ctx);

    li.append(top, state, foot);
    li.addEventListener("click", () => {
      selected = s.id;
      draw();
    });
    list.append(li);
  }
}

// Dragging a session moves it, and the order is remembered.
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
    if (from < 0 || to < 0) return;
    ids.splice(to, 0, ids.splice(from, 1)[0]);
    await invoke("reorder", { ids });
    dragging = null;
    await draw();
  });
  list.addEventListener("dragend", () => {
    dragging = null;
    for (const other of list.children) other.classList.remove("dragging", "over");
  });
}

// ── the middle ─────────────────────────────────────────────────────────────
function row(at, kind, head) {
  const line = document.createElement("div");
  line.className = "row";
  const when = document.createElement("span");
  when.className = "at";
  when.textContent = clock(at);
  const tag = document.createElement("span");
  tag.className = `tag ${kind}`;
  tag.textContent = `[${kind.toUpperCase().slice(0, 6)}]`;
  const said = document.createElement("span");
  said.className = "said";
  said.textContent = head;
  line.append(when, tag, said);
  return line;
}

function card(e, pending) {
  const box = document.createElement("div");
  box.className = `card${pending ? " pending" : ""}`;
  const head = document.createElement("div");
  head.className = "card-head";
  const title = document.createElement("span");
  title.textContent = `${pending ? "Pending " : ""}Tool Call: ${e.tool || "tool"}`;
  const when = document.createElement("span");
  when.className = "card-id";
  when.textContent = clock(e.at);
  head.append(title, when);
  const body = document.createElement("pre");
  body.className = "card-body";
  body.textContent = e.head;
  box.append(head, body);
  return box;
}

async function drawFeed(id) {
  const events = await invoke("feed", { id, limit: 200 });
  const out = el("pane");
  // Follow the feed only if it was already being followed: yanking someone
  // back to the bottom while they are reading is worse than not following.
  const following = out.scrollHeight - out.scrollTop - out.clientHeight < 40;
  clear(out);
  if (!events.length) {
    out.append(note("nothing on this session's record yet"));
    return;
  }
  const lastTool = events.map((e) => e.kind).lastIndexOf("tool");
  const answered = events.map((e) => e.kind).lastIndexOf("result") > lastTool;
  events.forEach((e, i) => {
    if (e.kind === "tool") {
      out.append(card(e, i === lastTool && !answered));
    } else {
      out.append(row(e.at, e.kind, e.head));
    }
  });
  const end = document.createElement("div");
  end.className = "end";
  end.textContent = "END OF STREAM";
  out.append(end);
  if (following) out.scrollTop = out.scrollHeight;
}

function note(text) {
  const p = document.createElement("div");
  p.className = "empty";
  p.textContent = text;
  return p;
}

async function drawFiles(id) {
  const files = await invoke("files", { id });
  const out = el("pane");
  clear(out);
  if (!files.length) return out.append(note("no files touched yet"));
  for (const f of files) {
    const line = document.createElement("div");
    line.className = "line";
    const path = document.createElement("span");
    path.className = "path";
    path.textContent = f.path;
    const counts = document.createElement("span");
    counts.className = "num";
    counts.textContent = `${f.reads}r ${f.edits + f.writes}w`;
    const diff = document.createElement("span");
    diff.className = "num";
    diff.innerHTML = "";
    const plus = document.createElement("span");
    plus.className = "added";
    plus.textContent = `+${f.added}`;
    const minus = document.createElement("span");
    minus.className = "removed";
    minus.textContent = ` −${f.removed}`;
    diff.append(plus, minus);
    line.append(path, document.createElement("span"), counts, diff);
    line.style.justifyContent = "space-between";
    out.append(line);
  }
}

async function drawPlan(id) {
  const todos = await invoke("plan", { id });
  const out = el("pane");
  clear(out);
  if (!todos.length) return out.append(note("no plan written"));
  for (const t of todos) {
    const line = document.createElement("div");
    const state = t.state === "completed" ? "done" : t.state === "in_progress" ? "doing" : "";
    line.className = `line todo ${state}`;
    const mark = document.createElement("span");
    mark.className = "num";
    mark.textContent = state === "done" ? "✓" : state === "doing" ? "▸" : "○";
    const what = document.createElement("span");
    what.className = "what";
    what.textContent = t.text;
    line.append(mark, what);
    out.append(line);
  }
}

async function drawAgentRuns(id) {
  const runs = await invoke("agents", { id });
  const out = el("pane");
  clear(out);
  if (!runs.length) return out.append(note("no subagents launched"));
  for (const a of runs) {
    const line = document.createElement("div");
    line.className = "line";
    const kind = document.createElement("span");
    kind.className = "num";
    kind.textContent = a.kind || "agent";
    const what = document.createElement("span");
    what.className = "path";
    what.textContent = a.description;
    const state = document.createElement("span");
    state.className = "num";
    state.textContent = a.state;
    line.append(kind, what, state);
    line.style.justifyContent = "space-between";
    out.append(line);
  }
}

async function drawErrors(id) {
  const errors = await invoke("errors", { id });
  const out = el("pane");
  clear(out);
  if (!errors.length) return out.append(note("nothing has gone wrong"));
  for (const e of errors) out.append(row(e.at, "result", e.head));
}

// How wide a character actually is in this font at this size, measured once
// rather than guessed at: a column count that is off by one puts every wrapped
// line in the wrong place.
let cell = null;
function cellSize() {
  if (cell) return cell;
  const ruler = document.createElement("div");
  ruler.className = "screen";
  ruler.style.cssText = "position:absolute;visibility:hidden;white-space:pre";
  ruler.textContent = "M".repeat(100);
  document.body.append(ruler);
  cell = { w: ruler.getBoundingClientRect().width / 100, h: ruler.getBoundingClientRect().height };
  ruler.remove();
  return cell;
}

// The session's own screen, cell by cell, at the size of this panel.
async function drawMirror(id) {
  const out = el("pane");
  const { w, h } = cellSize();
  const cols = Math.max(40, Math.floor((out.clientWidth - 26) / w));
  const rows = Math.max(10, Math.floor((out.clientHeight - 20) / h));
  const frame = await invoke("frame", { id, cols, rows });
  clear(out);
  if (!frame) return out.append(note("scope has no terminal for this session"));
  const screen = document.createElement("div");
  screen.className = "screen";
  frame.lines.forEach((line, y) => {
    const div = document.createElement("div");
    for (const run of line) {
      const span = run.bold ? document.createElement("b") : document.createElement("span");
      span.textContent = run.text;
      if (run.fg) span.style.color = run.fg;
      if (run.bg) span.style.background = run.bg;
      if (run.italic) span.style.fontStyle = "italic";
      if (run.underline) span.style.textDecoration = "underline";
      if (run.inverse) {
        span.style.background = run.fg || "var(--body)";
        span.style.color = run.bg || "var(--ground)";
      }
      div.append(span);
    }
    if (frame.cursor_visible && frame.cursor[0] === y) div.classList.add("has-caret");
    screen.append(div);
  });
  out.append(screen);
}

const painters = {
  feed: drawFeed,
  files: drawFiles,
  plan: drawPlan,
  agents: drawAgentRuns,
  mirror: drawMirror,
  errors: drawErrors,
};

// ── the right-hand meters ──────────────────────────────────────────────────
function drawMeters(s) {
  const cpu = s && s.cpu !== null && s.cpu !== undefined ? s.cpu : null;
  el("cpu").textContent = cpu === null ? "—" : `${cpu.toFixed(1)}%`;
  el("cpu-bar").style.width = `${Math.min(100, cpu || 0)}%`;
  el("mem").textContent = s && s.memory ? bytes(s.memory) : "—";
  // Against eight gigabytes, which is enough to read the bar by.
  el("mem-bar").style.width = `${Math.min(100, ((s?.memory || 0) / (8 << 30)) * 100)}%`;

  const tools = el("tools");
  clear(tools);
  for (const t of s?.tools || []) {
    const chip = document.createElement("span");
    chip.className = "chip";
    chip.textContent = t;
    tools.append(chip);
  }
  const share = s && s.window ? Math.min(100, (s.context / s.window) * 100) : 0;
  el("ctx-bar").style.width = `${share}%`;
  el("ctx-text").textContent = s && s.window ? `${tokens(s.context)} of ${tokens(s.window)}` : "";
}

// ── what is waiting on you ─────────────────────────────────────────────────
function drawAsk() {
  const waiting = sessions.find((s) => s.id === selected && s.asking) || sessions.find((s) => s.asking);
  const bar = el("ask");
  if (!waiting) {
    bar.hidden = true;
    return;
  }
  bar.hidden = false;
  el("ask-who").textContent = `[${waiting.name}]`;
  el("ask-what").textContent = waiting.asking.question;
  const answers = el("answers");
  clear(answers);
  waiting.asking.options.forEach((option, i) => {
    const button = document.createElement("button");
    button.className = `answer${i === 0 ? " first" : ""}`;
    const key = document.createElement("span");
    key.className = "key";
    key.textContent = i === 0 ? "[Y]" : i === 1 ? "[N]" : `[${i + 1}]`;
    button.append(key, document.createTextNode(option.replace(/^\d+\.\s*/, "")));
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
  if (!home) home = sessions.find((s) => s.cwd.startsWith("/home/"))?.cwd.match(/^\/home\/[^/]+/)?.[0] || "";
  if (!sessions.some((s) => s.id === selected)) {
    selected = (sessions.find((s) => s.asking) || sessions.find((s) => s.live) || sessions[0])?.id ?? null;
  }
  const current = sessions.find((s) => s.id === selected);

  const working = sessions.filter((s) => s.state === "working" || s.state === "running").length;
  const asking = sessions.filter((s) => s.asking).length;
  el("summary").textContent = `${sessions.length} sessions · ${working} working · ${asking} awaiting input`;
  const out = sessions.reduce((n, s) => n + s.output, 0);
  const spend = sessions.reduce((n, s) => n + s.cost, 0);
  el("spend").textContent = `${tokens(out)} out · ~$${spend.toFixed(2)} if API`;
  el("context").textContent = current ? `Context: ${current.name}` : "";

  drawAgents();
  drawMeters(current);
  drawAsk();
  if (current) await painters[pane](current.id);
  else {
    clear(el("pane"));
    el("pane").append(note("no sessions yet — start one"));
  }
}

// ── the things you can press ───────────────────────────────────────────────
el("views").addEventListener("click", (e) => {
  const tab = e.target.closest(".tab");
  if (!tab) return;
  for (const other of el("views").children) other.classList.toggle("is-on", other === tab);
  say(tab.dataset.view === "fleet" ? "" : `${tab.dataset.view} is not built yet`);
});

el("panes").addEventListener("click", (e) => {
  const tab = e.target.closest(".tab");
  if (!tab) return;
  if (pane === "mirror" && tab.dataset.pane !== "mirror" && selected) {
    invoke("release_frame", { id: selected });
  }
  pane = tab.dataset.pane;
  for (const other of el("panes").querySelectorAll(".tab")) {
    other.classList.toggle("is-on", other === tab);
  }
  draw();
});

el("filter").addEventListener("click", () => {
  liveOnly = !liveOnly;
  say(liveOnly ? "showing only what is running" : "showing everything");
  draw();
});

el("tui").addEventListener("click", async () => {
  try {
    say(`terminal view opened in ${await invoke("open_tui")}`);
  } catch (e) {
    say(String(e));
  }
});

el("new").addEventListener("click", () => {
  el("start-path").value = sessions.find((s) => s.id === selected)?.cwd || "";
  el("starter").showModal();
});

el("start-form").addEventListener("submit", async (e) => {
  if (e.submitter && e.submitter.value === "cancel") return;
  const agent = el("start-agent").value;
  const model = el("start-model").value.trim();
  const prompt = el("start-prompt").value.trim();
  let line = el("start-path").value.trim() || ".";
  if (agent && agent !== "claude") line += ` --agent ${agent}`;
  if (model) line += ` --model ${model}`;
  if (prompt) line += ` ${prompt}`;
  try {
    const name = await invoke("start", { line, name: el("start-name").value.trim() || null });
    say(`started ${name}`);
  } catch (err) {
    say(String(err));
  }
  el("start-prompt").value = "";
  draw();
});

// y and n answer whatever is waiting, as they do in the terminal view.
document.addEventListener("keydown", (e) => {
  if (e.target.matches("input, select, textarea")) return;
  const first = el("answers").firstElementChild;
  if (e.key === "y" && first) first.click();
  if (e.key === "n" && el("answers").children[1]) el("answers").children[1].click();
});

el("hint").textContent = "y accept · n decline · drag to reorder · F12 in a session comes back";
wireDragging();
draw();
setInterval(draw, 1000);
