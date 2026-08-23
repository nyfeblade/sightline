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
let pane = "talk";
let feedFilter = "all";
let focused = false;
let liveOnly = false;
let showCost = false;
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
// ── reading code ───────────────────────────────────────────────────────────
//
// A hand-written tokeniser, because the window is served under a policy that
// allows nothing from anywhere else — no CDN, no external script. That turns
// out to be a reasonable constraint rather than a burden: what is wanted here
// is not a general highlighter but a fast one, over the handful of languages
// agents actually write, that never throws on a half-written file.
//
// It is deliberately a lexer and not a parser. It knows about comments,
// strings, numbers, keywords and names, and nothing about grammar. Code being
// looked at while it is being written is usually not valid, and anything that
// needed it to parse would spend most of its time being wrong.

const WORDS = {
  rust: "as async await break const continue crate dyn else enum extern false fn for if impl in let loop match mod move mut pub ref return static struct super trait true type unsafe use where while yield",
  js: "as async await break case catch class const continue debugger default delete do else export extends false finally for from function get if import in instanceof let new null of return set static super switch this throw true try typeof undefined var void while with yield",
  ts: "abstract any as asserts async await boolean break case catch class const constructor continue declare default delete do else enum export extends false finally for from function get if implements import in infer instanceof interface is keyof let namespace never new null number of private protected public readonly return set static string super switch symbol this throw true try type typeof undefined unknown var void while yield",
  python: "and as assert async await break class continue def del elif else except False finally for from global if import in is lambda None nonlocal not or pass raise return True try while with yield match case self",
  go: "break case chan const continue default defer else fallthrough for func go goto if import interface map package range return select struct switch type var nil true false",
  c: "auto break case char const continue default do double else enum extern float for goto if inline int long register restrict return short signed sizeof static struct switch typedef union unsigned void volatile while true false NULL",
  java: "abstract assert boolean break byte case catch char class const continue default do double else enum extends final finally float for if implements import instanceof int interface long native new package private protected public return short static super switch synchronized this throw throws transient true false null try void volatile while",
  sh: "if then else elif fi for while until do done case esac function return in select time break continue local export readonly declare source alias unset shift trap exit set echo cd",
  sql: "select from where insert into values update set delete create table drop alter add index join left right inner outer on group by order having limit offset union all distinct as and or not null primary key foreign references default",
  toml: "true false",
  yaml: "true false null yes no on off",
  css: "",
  json: "true false null",
};

// Each language is described by how it writes the four things every language
// writes differently: comments, strings, names and numbers.
const SPECS = {
  rust:   { line: ["//"], block: ["/*", "*/"], quotes: `"'`, raw: true, words: WORDS.rust, attr: "#[" },
  js:     { line: ["//"], block: ["/*", "*/"], quotes: "\"'`", words: WORDS.js },
  ts:     { line: ["//"], block: ["/*", "*/"], quotes: "\"'`", words: WORDS.ts },
  python: { line: ["#"], triple: true, quotes: `"'`, words: WORDS.python, deco: "@" },
  go:     { line: ["//"], block: ["/*", "*/"], quotes: "\"'`", words: WORDS.go },
  c:      { line: ["//"], block: ["/*", "*/"], quotes: `"'`, words: WORDS.c, pre: "#" },
  java:   { line: ["//"], block: ["/*", "*/"], quotes: `"'`, words: WORDS.java, attr: "@" },
  sh:     { line: ["#"], quotes: "\"'", words: WORDS.sh, dollar: true },
  sql:    { line: ["--"], block: ["/*", "*/"], quotes: `"'`, words: WORDS.sql, fold: true },
  json:   { quotes: `"`, words: WORDS.json },
  toml:   { line: ["#"], quotes: `"'`, words: WORDS.toml },
  yaml:   { line: ["#"], quotes: `"'`, words: WORDS.yaml },
  css:    { block: ["/*", "*/"], quotes: `"'`, words: "" },
  plain:  { quotes: "", words: "" },
};

const KEYSETS = {};
for (const [name, spec] of Object.entries(SPECS)) {
  KEYSETS[name] = new Set((spec.words || "").split(/\s+/).filter(Boolean));
}

const BY_EXTENSION = {
  rs: "rust",
  js: "js", mjs: "js", cjs: "js", jsx: "js",
  ts: "ts", tsx: "ts",
  py: "python", pyi: "python",
  go: "go",
  c: "c", h: "c", cc: "c", cpp: "c", hpp: "c", cxx: "c",
  java: "java", kt: "java", swift: "java", cs: "java",
  sh: "sh", bash: "sh", zsh: "sh", fish: "sh",
  sql: "sql",
  json: "json", jsonl: "json", lock: "toml",
  toml: "toml",
  yaml: "yaml", yml: "yaml",
  css: "css", scss: "css",
  html: "html", htm: "html", xml: "html", svg: "html",
  md: "markdown", markdown: "markdown",
  patch: "diff", diff: "diff",
};

function langOf(path) {
  if (!path) return "plain";
  const name = path.split(/[/\\]/).pop() || "";
  if (/^(dockerfile|makefile|justfile)$/i.test(name)) return "sh";
  const ext = name.includes(".") ? name.split(".").pop().toLowerCase() : "";
  return BY_EXTENSION[ext] || "plain";
}

const WORD_START = /[A-Za-z_$]/;
const WORD_REST = /[A-Za-z0-9_$]/;
const DIGIT = /[0-9]/;

// Beyond this the window spends longer colouring than anyone spends reading, so
// the text is shown plainly instead. Being slow is worse than being grey.
const PAINTABLE = 400_000;

/// Text in, [class, text] pairs out. Never throws: unterminated anything runs
/// to the end of the text, which is exactly what a half-written file contains.
function tokenize(text, lang) {
  const spec = SPECS[lang] || SPECS.plain;
  const keywords = KEYSETS[lang] || KEYSETS.plain;
  const out = [];
  const n = text.length;
  let i = 0;
  let plain = 0;
  // Runs of ordinary text are pushed as one token rather than one per
  // character, which is the difference between this being instant and not.
  const flush = (to) => {
    if (to > plain) out.push(["", text.slice(plain, to)]);
    plain = to;
  };
  const take = (cls, to) => {
    flush(i);
    out.push([cls, text.slice(i, to)]);
    i = to;
    plain = to;
  };
  const at = (what, pos) => text.startsWith(what, pos);

  while (i < n) {
    const ch = text[i];

    // Comments.
    let matched = false;
    for (const marker of spec.line || []) {
      if (at(marker, i)) {
        let end = text.indexOf("\n", i);
        if (end < 0) end = n;
        take("com", end);
        matched = true;
        break;
      }
    }
    if (matched) continue;
    if (spec.block && at(spec.block[0], i)) {
      const close = text.indexOf(spec.block[1], i + spec.block[0].length);
      take("com", close < 0 ? n : close + spec.block[1].length);
      continue;
    }

    // Strings, including Python's triple quotes and Rust's raw literals.
    if (spec.quotes && spec.quotes.includes(ch)) {
      if (spec.triple && at(ch.repeat(3), i)) {
        const close = text.indexOf(ch.repeat(3), i + 3);
        take("str", close < 0 ? n : close + 3);
        continue;
      }
      let j = i + 1;
      while (j < n) {
        if (text[j] === "\\") j += 2;
        else if (text[j] === ch) { j += 1; break; }
        else if (text[j] === "\n" && ch !== "`") { break; }
        else j += 1;
      }
      take("str", Math.min(j, n));
      continue;
    }
    if (spec.raw && ch === "r" && (text[i + 1] === '"' || text[i + 1] === "#")) {
      let hashes = 0;
      let j = i + 1;
      while (text[j] === "#") { hashes += 1; j += 1; }
      if (text[j] === '"') {
        const close = text.indexOf('"' + "#".repeat(hashes), j + 1);
        take("str", close < 0 ? n : close + 1 + hashes);
        continue;
      }
    }

    // Numbers.
    if (DIGIT.test(ch) || (ch === "." && DIGIT.test(text[i + 1] || ""))) {
      let j = i;
      while (j < n && /[0-9a-fA-FxXoObB_.eE]/.test(text[j])) {
        // 1..10 is a range, not a number with two points in it.
        if (text[j] === "." && text[j + 1] === ".") break;
        j += 1;
      }
      while (j < n && WORD_REST.test(text[j])) j += 1;
      take("num", j);
      continue;
    }

    // A shell variable, an attribute, a decorator, a preprocessor line.
    if (spec.dollar && ch === "$") {
      let j = i + 1;
      if (text[j] === "{") { j = text.indexOf("}", j); j = j < 0 ? n : j + 1; }
      else while (j < n && WORD_REST.test(text[j])) j += 1;
      take("var", j);
      continue;
    }
    if (spec.deco && ch === spec.deco && WORD_START.test(text[i + 1] || "")) {
      let j = i + 1;
      while (j < n && (WORD_REST.test(text[j]) || text[j] === ".")) j += 1;
      take("attr", j);
      continue;
    }
    if (spec.attr && at(spec.attr, i)) {
      let end = text.indexOf("\n", i);
      if (end < 0) end = n;
      take("attr", end);
      continue;
    }
    if (spec.pre && ch === "#" && (i === 0 || text[i - 1] === "\n")) {
      let end = text.indexOf("\n", i);
      if (end < 0) end = n;
      take("attr", end);
      continue;
    }

    // Names: a keyword, a type, something being called, or a plain identifier.
    if (WORD_START.test(ch)) {
      let j = i;
      while (j < n && WORD_REST.test(text[j])) j += 1;
      const word = text.slice(i, j);
      const known = keywords.has(word) || (spec.fold && keywords.has(word.toLowerCase()));
      let after = j;
      while (after < n && (text[after] === " " || text[after] === "\t")) after += 1;
      let cls = "";
      if (known) cls = "kw";
      else if (/^[A-Z]/.test(word) && word.length > 1) cls = "typ";
      else if (text[after] === "(") cls = "fnc";
      else if (lang === "json" || lang === "yaml" || lang === "toml") {
        if (text[after] === ":" || text[after] === "=") cls = "key";
      }
      if (cls) take(cls, j);
      else i = j;
      continue;
    }

    i += 1;
  }
  flush(n);
  return out;
}

/// Lines that begin with a mark, which is what a diff is. Handled separately
/// because a diff is not a language and colouring it as one loses the only
/// thing about it that matters.
function tokenizeDiff(text) {
  const out = [];
  for (const line of text.split("\n")) {
    let cls = "";
    if (/^(\+\+\+|---)/.test(line)) cls = "dhead";
    else if (line.startsWith("@@")) cls = "dhunk";
    else if (line.startsWith("+")) cls = "dadd";
    else if (line.startsWith("-")) cls = "ddel";
    out.push([cls, line], ["", "\n"]);
  }
  out.pop();
  return out;
}

/// Tokens in, lines of tokens out, so a gutter can be drawn beside them.
function intoLines(tokens) {
  const lines = [[]];
  for (const [cls, text] of tokens) {
    const parts = text.split("\n");
    parts.forEach((part, k) => {
      if (k) lines.push([]);
      if (part) lines[lines.length - 1].push([cls, part]);
    });
  }
  return lines;
}

/// A block of code, coloured, optionally numbered, always selectable.
function codeBlock(text, lang, { numbers = false, start = 1 } = {}) {
  const wrap = make("div", `code${numbers ? " numbered" : ""}`);
  const body = String(text ?? "");
  if (body.length > PAINTABLE) {
    const pre = make("pre", "plainly", body);
    wrap.append(pre);
    return wrap;
  }
  const tokens = lang === "diff" ? tokenizeDiff(body) : tokenize(body, lang);
  const lines = intoLines(tokens);
  const code = make("pre", "lines");
  // The number belongs to its line rather than to a column beside it. A
  // separate gutter has to be kept the same height as the code by agreement,
  // and it stops agreeing the moment anything around it scrolls — which is how
  // the first version of this ended up numbering lines it was not showing.
  lines.forEach((line, index) => {
    const row = make("div", "cline");
    if (numbers) row.append(make("span", "ln", String(start + index)));
    const src = make("span", "src");
    for (const [cls, part] of line) {
      if (cls) src.append(make("span", cls, part));
      else src.append(document.createTextNode(part));
    }
    // A newline the row does not draw but a copy still takes with it.
    src.append(document.createTextNode("\n"));
    row.append(src);
    code.append(row);
  });
  wrap.append(code);
  return wrap;
}

/// Prose with fenced code in it, which is what an agent's replies are made of.
///
/// Only fences are treated specially. The rest is left as text: this is a
/// window onto a conversation, not a markdown renderer, and turning asterisks
/// into italics in a message about a glob pattern helps nobody.
function prose(body) {
  const out = document.createDocumentFragment();
  const parts = String(body ?? "").split(/```/);
  parts.forEach((part, i) => {
    if (i % 2 === 0) {
      if (part) out.append(make("span", null, part));
      return;
    }
    // The word after the opening fence is the language, when there is one.
    const nl = part.indexOf("\n");
    const first = (nl < 0 ? part : part.slice(0, nl)).trim();
    const named = /^[A-Za-z0-9+#_-]{1,16}$/.test(first);
    const lang = named ? (BY_EXTENSION[first.toLowerCase()] || first.toLowerCase()) : "";
    const code = named && nl >= 0 ? part.slice(nl + 1) : part;
    const block = codeBlock(code.replace(/\n$/, ""), SPECS[lang] ? lang : "plain");
    block.classList.add("fenced");
    out.append(block);
  });
  return out;
}

/// Show a body of code in the large view: coloured, numbered, and scrollable
/// without taking the window with it.
function showCode(title, note, body, lang, numbers = true) {
  el("code-title").textContent = title;
  text("code-note", note);
  const out = el("code-body");
  clear(out);
  out.append(codeBlock(body, lang, { numbers }));
  out.scrollTop = 0;
  el("code").showModal();
}

/// The first thing in a summary that looks like a path. Tool summaries lead
/// with one — `src/main.rs · +12/-4`, `~/notes.md @40` — so this is how a feed
/// row knows what it is about.
function pathIn(head) {
  if (!head) return null;
  const first = head.split(/[\s·]+/)[0];
  if (!first) return null;
  return /[/\\]/.test(first) || /\.[A-Za-z0-9]{1,6}$/.test(first) ? first : null;
}

/// Open a file in the viewer. Relative paths are resolved against wherever the
/// session is working, which is the only place they mean anything.
async function openFile(path, title) {
  const base = selected ? await invoke("session_cwd", { id: selected }) : null;
  try {
    const file = await invoke("open_file", { path, base });
    const note = [
      `${file.lines} lines`,
      bytes(file.bytes),
      file.truncated ? "showing the first 20,000" : null,
    ]
      .filter(Boolean)
      .join(" · ");
    showCode(title || shortPath(file.path), note, file.text, langOf(file.path));
  } catch (e) {
    say(String(e));
  }
}

/// A path that opens what it names when clicked.
function pathLink(path, label) {
  const link = make("span", "openable", label ?? shortPath(path));
  link.title = "Open this file";
  link.addEventListener("click", (e) => {
    e.stopPropagation();
    openFile(path);
  });
  return link;
}

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
  text("agent-count", liveOnly ? `Live (${shown.length})` : "Sessions");
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
    // A session started by another sits under it. Indenting the row rather
    // than reordering the list keeps the order you chose.
    if (s.depth) li.style.paddingLeft = `${8 + s.depth * 14}px`;
    if (s.parent) li.classList.add("is-child");
    li.append(make("i", `dot ${kind}`));
    li.append(make("span", "name", s.name));
    li.append(make("span", "age", age(s.age_secs)));
    const where = make("span", "where");
    // The state as a pill rather than a bare word: it is the one thing on this
    // line you scan for, and it was competing with the folder next to it.
    const tone = kind === "needs" ? "needs" : kind === "working" ? "working" : kind === "ended" ? "ended" : "";
    where.append(make("span", `pill ${tone}`, kind === "needs" ? "needs you" : label));
    // The folder is worth a line only when it says something: every session on
    // this machine would otherwise read "~".
    const folder = folderName(s.cwd);
    if (folder !== "~") where.append(document.createTextNode(` · ${folder}`));
    else if (s.branch) where.append(document.createTextNode(` · ${s.branch}`));
    li.append(where);
    li.append(make("span", "ctx", s.window ? tokens(s.context) : ""));
    // What it was asked to do, when it was asked something. This is the line
    // that turns a list of processes into a list of work.
    if (s.task) {
      const job = make("span", "job");
      job.append(make("span", `state ${s.task.state}`, s.task.state));
      job.append(make("span", null, s.task.assignment));
      li.append(job);
    }
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
// What the reader has opened.
//
// Deliberately not in the DOM. The feed redraws itself several times a second,
// and anything remembered only by a node is forgotten the next time that node
// is replaced — which is exactly what happened: expand a block, and half a
// second later it closed by itself.
const expanded = new Set();

/// Enough of an event to recognise it again after the feed is rebuilt.
const keyOf = (e) => `${e.at}|${e.tool || e.kind}|${(e.head || "").slice(0, 80)}`;

/// The full text of an event, when it says more than its summary does.
function bodyOf(e) {
    const body = e.body && e.body.trim();
    return body && body !== (e.head || "").trim() ? e.body : null;
}

/// Which language a tool's output is written in. A command is shell, a patch
/// is a diff, and anything about a file is whatever that file is.
function langOfEvent(e) {
  if (e.tool === "Bash") return "sh";
  if (/^@@ |^--- /m.test(e.body || "")) return "diff";
  const path = pathIn(e.head);
  if (path) return langOf(path);
  const body = (e.body || "").trimStart();
  if (body.startsWith("{") || body.startsWith("[")) return "json";
  return "plain";
}

function eventRow(e) {
  // A result is where the output actually is — a command's stdout, a file's
  // contents, a stack trace — so it opens where it sits as well as in the
  // large view. One click to see it, one to put it away.
  const body = bodyOf(e);
  const opens = body && (body.includes("\n") || body.length > 160);
  const holder = opens ? make("div", "rowset") : null;
  const key = keyOf(e);
  const block = () => {
    const out = codeBlock(body, langOfEvent(e), { numbers: body.split("\n").length > 2 });
    out.classList.add("inline-code");
    return out;
  };

  const line = make("div", "row hit");
  line.addEventListener("click", () => {
    if (!opens) {
      return showCode(
        `${e.tool || e.kind} · ${clock(e.at)}`,
        e.tool || "",
        e.body || e.head,
        langOfEvent(e),
        false,
      );
    }
    const shown = holder.querySelector(".inline-code");
    if (shown) {
      expanded.delete(key);
      shown.remove();
    } else {
      expanded.add(key);
      holder.append(block());
    }
  });
  line.append(make("span", "at", clock(e.at)));
  const who = e.kind === "prompt" ? "you" : e.kind === "text" ? "claude" : e.kind;
  line.append(make("span", `who ${e.kind}`, who));
  const said = make("span", "said");
  const path = pathIn(e.head);
  if (path && e.kind !== "prompt" && e.kind !== "text") {
    said.append(pathLink(path));
    said.append(document.createTextNode(e.head.slice(path.length)));
  } else {
    said.textContent = e.head;
  }
  line.append(said);
  if (!opens) return line;
  line.append(make("span", "more-mark", "⌄"));
  line.title = "Show what came back";
  holder.append(line);
  if (expanded.has(key)) holder.append(block());
  return holder;
}

function toolCard(e, running) {
  const box = make("div", `card${running ? " pending" : ""}`);
  const head = make("div", "card-head");
  head.append(make("span", null, `${running ? "running · " : ""}${e.tool || "tool"}`));
  const path = pathIn(e.head);
  if (path) {
    head.append(make("span", "grow"));
    head.append(pathLink(path));
  }
  head.append(make("span", "grow"));
  head.append(make("span", null, clock(e.at)));
  box.append(head);

  const lang = langOfEvent(e);
  // The summary is one line; the body is everything. Show the first and offer
  // the second, because a feed of fully expanded tool calls is unreadable and a
  // feed you cannot expand is useless.
  const body = bodyOf(e);
  // When the summary is nothing but the path, the header has already said it.
  if (!path || path !== e.head.trim()) {
    const preview = codeBlock(e.head, lang);
    preview.classList.add("card-body");
    box.append(preview);
  }

  if (body) {
    const key = keyOf(e);
    const lines = body.split("\n").length;
    const full = codeBlock(body, lang, { numbers: lines > 2 });
    full.classList.add("card-body", "more");
    const toggle = make("button", "expand");
    const show = (open) => {
      full.hidden = !open;
      toggle.textContent = open ? "collapse" : `expand · ${lines} lines`;
      toggle.classList.toggle("is-on", open);
    };
    show(expanded.has(key));
    toggle.addEventListener("click", () => {
      const open = !expanded.has(key);
      if (open) expanded.add(key);
      else expanded.delete(key);
      show(open);
    });
    box.append(toggle, full);
  }
  return box;
}

const empty = (text) => make("div", "empty", text);

// Painters run on a timer. Rebuilding a pane that has not changed costs a full
// re-colour of everything in it and throws away whatever the reader had open,
// so each painter says what it is about to draw and is told if that is already
// on screen.
const drawn = new Map();
function alreadyDrawn(name, signature) {
  // Another painter may have cleared the pane since; an empty one is never
  // already drawn.
  if (drawn.get(name) === signature && el("pane").firstChild) return true;
  drawn.set(name, signature);
  return false;
}

/// What a list of events amounts to, cheaply. Two lists with the same
/// signature look the same on screen.
const feedShape = (id, events, extra = "") =>
  `${id}|${extra}|${events.length}|${events.at(-1)?.at ?? ""}|${(events.at(-1)?.head ?? "").slice(0, 60)}`;

// ── the stream ─────────────────────────────────────────────────────────────
//
// Everything happening on the machine, in one place, pushed rather than
// polled. The terminal view shows one session at a time and asks; this is the
// whole fleet, arriving as it happens. It is the view that only exists because
// there is an event stream underneath.

// A ring, so a window left open for a day cannot grow without limit.
const STREAM_KEEP = 800;
let streamEvents = [];
let streamSeq = 0;
let streamFilter = "all";
let streamPinned = null;

const STREAM_FILTERS = {
  all: () => true,
  attention: (e) =>
    ["permissionAsked", "toolFailed", "checksFailed", "sessionStalled"].includes(e.kind.type),
  tools: (e) => ["toolCalled", "toolFailed"].includes(e.kind.type),
  changes: (e) => ["fileChanged", "commitCreated"].includes(e.kind.type),
  flow: (e) =>
    ["sessionStarted", "sessionWorking", "sessionWaiting", "sessionEnded"].includes(e.kind.type),
  spend: (e) => e.kind.type === "costSpent",
};

// How each kind reads, and how it is coloured. Deliberately the same wording
// the terminal view uses, so one is not a different account of the same event.
const STREAM_GLYPH = {
  sessionStarted: ["▸", "flow"],
  sessionWorking: ["●", "work"],
  sessionWaiting: ["◦", "wait"],
  sessionEnded: ["■", "flow"],
  permissionAsked: ["◆", "ask"],
  permissionAnswered: ["✓", "ok"],
  toolCalled: ["›", "tool"],
  toolFailed: ["✗", "bad"],
  fileChanged: ["±", "file"],
  commitCreated: ["⎇", "commit"],
  checksPassed: ["✓", "ok"],
  checksFailed: ["✗", "bad"],
  sessionStalled: ["⏸", "bad"],
  costSpent: ["$", "spend"],
};

function streamText(k) {
  switch (k.type) {
    case "sessionStarted": return `started in ${shortPath(k.cwd)}${k.branch ? ` on ${k.branch}` : ""}`;
    case "sessionWorking": return k.tool ? `working · ${k.tool}` : "working";
    case "sessionWaiting": return "waiting on you";
    case "sessionEnded": return `ended (${k.reason})`;
    case "permissionAsked": return `asks: ${k.question}`;
    case "permissionAnswered": return `answered ${k.option}${k.by === "policy" ? ` by policy ${k.name}` : ""}`;
    case "toolCalled": return `${k.tool} ${k.summary}`;
    case "toolFailed": return `${k.tool} failed · ${k.summary}`;
    case "fileChanged": return `${shortPath(k.path)} +${k.added}/-${k.removed}`;
    case "commitCreated": return `commit ${k.sha.slice(0, 7)} on ${k.branch} · ${k.message}`;
    case "checksPassed": return `${k.suite} passed in ${ms(k.ms)}`;
    case "checksFailed": return `${k.suite} failed · ${k.first}`;
    case "sessionStalled": return `stalled · nothing for ${age(k.quiet_for)}`;
    case "costSpent": return `${tokens(k.output)} out · $${k.estimate.toFixed(4)}`;
    default: return k.type;
  }
}

const nameOf = (id) => {
  const s = sessions.find((x) => x.id === id);
  return s ? s.name : id.slice(0, 8);
};

/// `live` is false while the recent past is being read in at startup. Those
/// events belong in the Stream view — that is what it is for — but they must
/// not touch the session list: replaying an hour of history would rewind every
/// status to whatever it was an hour ago and count the same tokens twice.
function pushEvent(ev, live = true) {
  streamEvents.push(ev);
  streamSeq = Math.max(streamSeq, ev.seq);
  if (streamEvents.length > STREAM_KEEP) streamEvents.splice(0, streamEvents.length - STREAM_KEEP);
  if (live) applyEvent(ev);
  if (pane === "stream") soon(0);
}

/// Move the interface to where the event says the session now is.
///
/// This is what it means for the window to take its status from the stream: a
/// session that starts working says so, and the row changes then — not up to a
/// second later when the window next thinks to ask.
function applyEvent(ev) {
  const s = sessions.find((x) => x.id === ev.session);
  const k = ev.kind;
  if (!s) {
    // Something Ironsight has not told us about yet. Only a new session is
    // worth a round trip; anything else will be right at the next correction.
    if (k.type === "sessionStarted") draw();
    return;
  }
  switch (k.type) {
    case "sessionWorking":
      s.state = k.tool ? "running" : "working";
      s.tool = k.tool ?? null;
      s.live = true;
      break;
    case "sessionWaiting":
      s.state = "waiting";
      s.tool = null;
      break;
    case "sessionEnded":
      s.state = "ended";
      s.tool = null;
      s.live = false;
      s.asking = null;
      break;
    case "permissionAsked":
      s.asking = { question: k.question, options: k.options };
      break;
    case "permissionAnswered":
      s.asking = null;
      break;
    case "toolCalled":
      s.tool = k.tool;
      if (s.state === "waiting") s.state = "running";
      break;
    case "costSpent": {
      s.output += k.output;
      s.cost += k.estimate;
      // And on to whoever is waiting on this session's work, so a supervisor's
      // figure moves when its worker spends rather than a poll later.
      let at = s;
      const seen = new Set([s.id]);
      while (at) {
        at.rolled_output += k.output;
        at.rolled_cost += k.estimate;
        const parent = at.parent && !seen.has(at.parent) ? sessions.find((x) => x.id === at.parent) : null;
        if (parent) seen.add(parent.id);
        at = parent;
      }
      break;
    }
    default:
      return;
  }
  s.age_secs = 0;
  repaintSoon();
}

/// Several events usually arrive together — a call, a change, a spend — and
/// they are one change to look at, not three.
function repaintSoon() {
  if (repainting) return;
  repainting = true;
  requestAnimationFrame(() => {
    repainting = false;
    repaint();
  });
}

async function startStream() {
  try {
    for (const ev of await invoke("stream", { since: 0 })) pushEvent(ev, false);
  } catch (e) {
    say(String(e));
  }
  // Pushed from the engine the moment it happens. Nothing here polls.
  window.__TAURI__.event.listen("ironsight://event", (msg) => pushEvent(msg.payload));
}

function drawStream() {
  const out = el("pane");
  const follow = keepingUp(out);
  const shown = streamEvents.filter(STREAM_FILTERS[streamFilter]).filter(
    (e) => !streamPinned || e.session === streamPinned,
  );
  clear(out);
  out.classList.add("stream");
  if (!shown.length) {
    return out.append(
      empty(
        streamEvents.length
          ? "nothing matches this filter"
          : "the stream is quiet — it fills as sessions do things",
      ),
    );
  }
  for (const ev of shown) {
    const [glyph, tone] = STREAM_GLYPH[ev.kind.type] || ["·", "flow"];
    const row = make("div", `srow ${tone}`);
    row.append(make("span", "at", clock(ev.at)));
    row.append(make("span", `sglyph ${tone}`, glyph));
    const who = make("span", "swho", nameOf(ev.session));
    // Indented by how deep the session sits under whoever started it, so a
    // supervisor's workers read as its workers.
    const depth = sessions.find((x) => x.id === ev.session)?.depth || 0;
    if (depth) who.style.marginLeft = `${depth * 10}px`;
    who.title = "Show only this session";
    who.addEventListener("click", () => {
      streamPinned = streamPinned === ev.session ? null : ev.session;
      drawFilters();
      soon(0);
    });
    row.append(who);
    const said = make("span", "said");
    // A path in an event is a thing you can open, so it is one you can click.
    const touched =
      ev.kind.type === "fileChanged"
        ? ev.kind.path
        : ev.kind.type === "toolCalled"
          ? pathIn(ev.kind.summary)
          : null;
    const line = streamText(ev.kind);
    const shown = touched ? shortPath(touched) : "";
    const cut = touched ? line.indexOf(shown) : -1;
    if (cut >= 0) {
      if (cut) said.append(document.createTextNode(line.slice(0, cut)));
      said.append(pathLink(touched, shown));
      said.append(document.createTextNode(line.slice(cut + shown.length)));
    } else {
      said.textContent = line;
    }
    row.append(said);
    if (ev.task) row.append(make("span", "tag", ev.task));
    out.append(row);
  }
  if (follow) out.scrollTop = out.scrollHeight;
}

// ── what each session was asked to do ──────────────────────────────────────
async function drawWork() {
  const tasks = await invoke("tasks");
  const out = el("pane");
  clear(out);
  out.classList.remove("stream");
  if (!tasks.length) {
    return out.append(empty("nothing has been assigned yet — assign a session from its panel"));
  }
  for (const t of tasks) {
    const card = make("div", `task ${t.state}`);
    card.style.marginLeft = `${t.depth * 18}px`;
    const head = make("div", "task-head");
    head.append(make("span", `state ${t.state}`, t.state));
    head.append(make("span", "task-who", nameOf(t.session)));
    head.append(make("span", "grow"));
    head.append(make("span", "at", t.id));
    card.append(head);
    card.append(make("div", "task-what", t.assignment));
    if (t.why) card.append(make("div", "task-why", t.why));
    for (const n of t.notes) card.append(make("div", "task-note", `· ${n}`));
    out.append(card);
  }
}


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
  box.hidden = pane !== "feed" && pane !== "stream";
  if (box.hidden) return;
  clear(box);
  if (pane === "stream") {
    for (const name of Object.keys(STREAM_FILTERS)) {
      const chip = make("span", `chip${name === streamFilter ? " is-on" : ""}`, name);
      chip.addEventListener("click", () => {
        streamFilter = name;
        drawFilters();
        soon(0);
      });
      box.append(chip);
    }
    if (streamPinned) {
      const pin = make("span", "chip is-pin", `only ${nameOf(streamPinned)} ✕`);
      pin.addEventListener("click", () => {
        streamPinned = null;
        drawFilters();
        soon(0);
      });
      box.append(pin);
    }
    return;
  }
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
  if (alreadyDrawn("feed", feedShape(id, events, feedFilter))) return;
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
// ── talking to a session ───────────────────────────────────────────────────
//
// Not a picture of a terminal. Mirroring one meant resizing the session to the
// shape of a panel, which fights whoever is sitting in that session in their
// own terminal, and it meant the window could only ever show what a terminal
// can draw. This is the conversation itself: what was said, what was run, what
// came back, and the question it is stuck on — each rendered as the thing it
// is, and each openable.

/// The result belonging to a call, when it has come back.
///
/// A transcript interleaves — call, result, call, result — so the first result
/// after a call is its own. Another call arriving first means this one is still
/// running.
function resultOf(events, i) {
  for (let j = i + 1; j < events.length; j += 1) {
    if (events[j].kind === "result") return events[j];
    if (events[j].kind === "tool") return null;
  }
  return null;
}

/// Something a person or an agent said.
function bubble(who, e) {
  const box = make("div", `turn ${who}`);
  const head = make("div", "turn-head");
  head.append(make("span", "turn-who", who === "you" ? "you" : "claude"));
  head.append(make("span", "at", clock(e.at)));
  box.append(head);
  const said = make("div", "turn-said");
  said.append(prose(e.body || e.head));
  box.append(said);
  return box;
}

/// Reasoning, which is worth having and not worth reading every time.
function thought(e) {
  const key = keyOf(e);
  const box = make("div", "thought");
  const toggle = make("button", "expand", "thinking");
  const body = make("div", "turn-said");
  body.append(prose(e.body || e.head));
  const show = (open) => {
    body.hidden = !open;
    toggle.textContent = open ? "thinking · hide" : "thinking";
    toggle.classList.toggle("is-on", open);
  };
  show(expanded.has(key));
  toggle.addEventListener("click", () => {
    const open = !expanded.has(key);
    if (open) expanded.add(key);
    else expanded.delete(key);
    show(open);
  });
  box.append(toggle, body);
  return box;
}

/// Something the session did, with what came back folded underneath it.
function activity(call, result) {
  const key = keyOf(call);
  const failed = result && result.ok === false;
  const box = make("div", `act${failed ? " bad" : ""}${result ? "" : " running"}`);

  const row = make("div", "act-head");
  row.append(make("span", "act-mark", failed ? "✗" : result ? "›" : "◍"));
  row.append(make("span", "act-tool", call.tool || "tool"));
  const summary = make("span", "act-what");
  const path = pathIn(call.head);
  if (path) {
    summary.append(pathLink(path));
    summary.append(document.createTextNode(call.head.slice(path.length)));
  } else {
    summary.textContent = call.head;
  }
  row.append(summary);
  row.append(make("span", "grow"));
  row.append(make("span", "at", clock(call.at)));
  box.append(row);

  // What came back, if anything has. The call's own body is the fallback: a
  // call still running has arguments worth seeing and no result yet.
  const shown = result ? result.body || result.head : bodyOf(call);
  if (!shown || !shown.trim()) return box;
  const lang = result ? langOfEvent(result) : langOfEvent(call);
  const body = codeBlock(shown, lang, { numbers: shown.split("\n").length > 2 });
  body.classList.add("act-body");
  const lines = shown.split("\n").length;
  const toggle = make("button", "expand");
  const show = (open) => {
    body.hidden = !open;
    toggle.textContent = open ? "collapse" : `${result ? "what came back" : "arguments"} · ${lines} lines`;
    toggle.classList.toggle("is-on", open);
  };
  show(expanded.has(key));
  toggle.addEventListener("click", () => {
    const open = !expanded.has(key);
    if (open) expanded.add(key);
    else expanded.delete(key);
    show(open);
  });
  box.append(toggle, body);
  return box;
}

/// The question it is stuck on, where the conversation is, so answering it is
/// part of the conversation rather than a bar somewhere else.
function askCard(s) {
  const box = make("div", "asking-card");
  box.append(make("div", "asking-q", s.asking.question));
  const row = make("div", "asking-options");
  s.asking.options.forEach((option, i) => {
    const button = make("button", `answer${i === 0 ? " first" : ""}`);
    button.append(make("span", "key", String(i + 1)));
    button.append(document.createTextNode(option.replace(/^\d+\.\s*/, "")));
    button.addEventListener("click", async () => {
      try {
        await invoke("answer", { id: s.id, option: i + 1 });
        say(`answered ${option}`);
      } catch (e) {
        say(String(e));
      }
      draw();
    });
    row.append(button);
  });
  box.append(row);
  return box;
}

// What is already on screen, so a redraw can add to it instead of building it
// again. Rebuilding four times a second means re-colouring every code block in
// the conversation while someone is trying to type into the box below it, and
// that is exactly what it feels like.
let talkOn = { id: null, lastKey: null, mark: "", asking: "", lastCall: null };

/// One rendered event, appended to the conversation.
function talkNode(e, events, i) {
  switch (e.kind) {
    case "prompt":
      return bubble("you", e);
    case "text":
      return bubble("claude", e);
    case "thinking":
      return thought(e);
    case "tool":
      return activity(e, resultOf(events, i));
    default:
      // Results are shown under the call they belong to; the machinery talking
      // about itself is not part of the conversation.
      return null;
  }
}

async function drawTalk(id) {
  const events = await invoke("feed", { id, limit: 400 });
  const s = current();
  const out = el("pane");
  const asking = s?.asking ? s.asking.question : "";
  const mark = `${events.length}|${events.at(-1)?.at ?? ""}`;

  // Nothing has changed, and nothing was scrolled: leave it alone entirely.
  if (talkOn.id === id && talkOn.mark === mark && talkOn.asking === asking && out.firstChild) {
    return;
  }

  // Find where we left off by the last-rendered event's identity, not by an
  // index. `feed` returns a sliding window of the newest 400 events, so once a
  // session passes 400 the indices shift under us and an index-based diff
  // appends nothing — the view froze. As long as the last event we drew is
  // still in the window we append only what follows it; if it has scrolled off
  // the back, we rebuild.
  let startIdx = -1;
  if (talkOn.id === id && talkOn.lastKey) {
    for (let i = events.length - 1; i >= 0; i -= 1) {
      if (keyOf(events[i]) === talkOn.lastKey) {
        startIdx = i;
        break;
      }
    }
  }
  const grew = talkOn.id === id && out.firstChild && startIdx >= 0;

  const follow = keepingUp(out);
  const existing = out.querySelector(".asking-card");
  if (existing) existing.remove();

  if (!grew) {
    clear(out);
    const whose = make("div", "whose");
    whose.append(make("span", "whose-name", s ? s.name : "—"));
    if (s) whose.append(make("span", "whose-where", `${s.state} · ${shortPath(s.cwd)}`));
    out.append(whose);
    if (!events.length) {
      talkOn = { id, lastKey: null, mark, asking, lastCall: null };
      return out.append(empty("nothing said yet — type below to start"));
    }
    talkOn.lastCall = null;
    events.forEach((e, i) => {
      const node = talkNode(e, events, i);
      if (node) out.append(node);
      if (e.kind === "tool") talkOn.lastCall = { node, at: e.at, head: e.head };
    });
  } else {
    // Only what is new. A result completes the call above it rather than
    // arriving as a row of its own, so the call is rebuilt in place.
    for (let i = startIdx + 1; i < events.length; i += 1) {
      const e = events[i];
      if (e.kind === "result" && talkOn.lastCall) {
        const call = { kind: "tool", at: talkOn.lastCall.at, head: talkOn.lastCall.head, tool: e.tool, body: "" };
        const fresh = activity(call, e);
        talkOn.lastCall.node.replaceWith(fresh);
        talkOn.lastCall.node = fresh;
        continue;
      }
      const node = talkNode(e, events, i);
      if (node) out.append(node);
      if (e.kind === "tool") talkOn.lastCall = { node, at: e.at, head: e.head };
    }
  }

  if (s?.asking) out.append(askCard(s));
  talkOn = {
    id,
    lastKey: events.length ? keyOf(events.at(-1)) : null,
    mark,
    asking,
    lastCall: talkOn.lastCall,
  };
  if (follow) out.scrollTop = out.scrollHeight;
}

async function drawFiles(id) {
  const files = await invoke("files", { id });
  const out = el("pane");
  clear(out);
  if (!files.length) return out.append(empty("no files touched yet"));
  for (const f of files) {
    const line = make("div", "line hit");
    line.title = "Open this file";
    line.addEventListener("click", () => openFile(f.path));
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
    const line = make("div", "line hit");
    line.title = "Show what changed in this file";
    line.addEventListener("click", async () => {
      const patch = await invoke("file_diff", { id, path: e.path });
      if (!patch) return say(`nothing to show for ${e.path}`);
      // An untracked file has no diff, only contents — so it is shown as what
      // it is rather than as a patch that does not exist.
      const isPatch = /^(@@|--- |\+\+\+ |diff )/m.test(patch);
      showCode(
        e.path,
        isPatch ? e.state : `${e.state} · not yet tracked`,
        patch,
        isPatch ? "diff" : langOf(e.path),
        !isPatch,
      );
    });
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
  talk: (s) => drawTalk(s.id),
  files: (s) => drawFiles(s.id),
  tree: (s) => drawTree(s.id),
  plan: (s) => drawPlan(s.id),
  agents: (s) => drawSubagents(s.id),
  stats: (s) => drawStats(s),
  errors: (s) => drawErrors(s.id),
  stream: () => drawStream(),
  work: () => drawWork(),
  fleet: () => drawFleet(),
};

// Two of these are about the machine rather than about one session, so they are
// drawn whether or not anything is selected.
const FLEETWIDE = ["stream", "work", "fleet"];

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

  if (s.rolled_output > s.output) {
    fact(box, "with workers", `${tokens(s.rolled_output)} out · $${s.rolled_cost.toFixed(2)}`);
  }
  if (s.task) {
    fact(box, "assignment", s.task.assignment);
    fact(box, "task", `${s.task.id} · ${s.task.state}`);
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
  // Assigning does not need a terminal: a task is a record about a session,
  // and it outlives the session it is about.
  action(actions, s.task ? "Reassign" : "Assign…", "ghost", async () => {
    const what = await ask("What is this session for?", s.task ? s.task.assignment : "");
    if (!what) return;
    try {
      const id = await invoke("assign", { id: s.id, text: what });
      say(`${id} assigned`);
      draw();
    } catch (e) {
      say(String(e));
    }
  });
  if (s.task) {
    action(actions, "Note…", "ghost", async () => {
      const text = await ask("What was learned?");
      if (!text) return;
      try {
        await invoke("note", { task: s.task.id, text });
        say("noted");
        draw();
      } catch (e) {
        say(String(e));
      }
    });
  }
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
/// The whole picture, asked for outright.
///
/// This is now a correction rather than the heartbeat. Status arrives on the
/// stream, event by event, the moment it changes; what this fetches is
/// everything an event does not carry — processor share, resident memory, how
/// full the context window is, how slow the tools have been — none of which is
/// a transition and none of which anything would publish.
async function draw() {
  sessions = await invoke("sessions");
  if (!home) {
    home = sessions.find((s) => s.cwd.startsWith("/home/"))?.cwd.match(/^\/home\/[^/]+/)?.[0] || "";
  }
  try {
    readers = (await invoke("consumers")) || 0;
  } catch {
    readers = 0;
  }
  repaint();
}

let readers = 0;
let repainting = false;

/// Draw what is already known, asking the engine nothing.
function repaint() {
  if (!sessions.some((s) => s.id === selected)) {
    selected =
      (sessions.find((s) => s.asking) || sessions.find((s) => s.live) || sessions[0])?.id ?? null;
  }
  const s = current();

  const working = sessions.filter((x) => x.state === "working" || x.state === "running").length;
  const asking = sessions.filter((x) => x.asking).length;
  const counts = el("counts");
  clear(counts);
  counts.append(document.createTextNode(`${sessions.length} sessions · ${working} working`));
  if (asking) {
    counts.append(document.createTextNode(" · "));
    counts.append(make("b", null, `${asking} need you`));
  }

  const out = sessions.reduce((n, x) => n + x.output, 0);
  const spend = sessions.reduce((n, x) => n + x.cost, 0);
  const requests = sessions.reduce((n, x) => n + x.requests, 0);
  text("spend", showCost
    ? `${tokens(out)} out · ~$${spend.toFixed(2)} if API`
    : `${tokens(out)} out · ${requests} requests`);
  text("context", s ? s.name : "");

  // Name the session you are about to talk to.
  //
  // "Message this session…" is true and useless: the window picks a session on
  // launch, and there is nothing next to the box you type in that says which
  // one — so a message meant for one agent goes to another and the conversation
  // you are reading is not the one you think it is.
  const canType = !!s && s.steerable;
  el("message").disabled = !canType;
  el("message").placeholder = canType
    ? `Message ${s.name}…`
    : s
      ? `${s.name} cannot be typed into from here`
      : "no session selected";

  drawAgents();
  drawDetail(s);
  drawAsk();
  drawFilters();
  // How busy the machine is, and who else is reading the stream. Counted from
  // the events already in hand rather than asked for.
  const minute = Date.now() - 60_000;
  const recent = streamEvents.filter((e) => new Date(e.at).getTime() > minute).length;
  text(
    "pulse",
    recent
      ? `${recent}/min${readers ? ` · ${readers} reading` : ""}`
      : readers
        ? `${readers} reading`
        : "",
  );

  if (!s && !FLEETWIDE.includes(pane)) {
    clear(el("pane"));
    el("pane").append(empty("no sessions yet — start one"));
  }
}

// ── the things you can press ───────────────────────────────────────────────
on("panes", "click", (e) => {
  const tab = e.target.closest(".tab");
  if (!tab) return;
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

// Fill the window with the conversation, and put everything else away.
function toggleFocus() {
  focused = !focused;
  document.body.classList.toggle("focused", focused);
  if (focused && pane !== "talk") {
    pane = "talk";
    for (const tab of el("panes").querySelectorAll(".tab")) {
      tab.classList.toggle("is-on", tab.dataset.pane === "talk");
    }
  }
  soon(0);
}
on("focus", "click", toggleFocus);

// y and n answer whatever is waiting, as they do in the terminal view.
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape" && focused) return toggleFocus();
  if (e.target.matches("input, select, textarea")) return;
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
// Deliberately none of the meaning colours: amber (--warn), green (--good) and
// red (--bad) each say something, and a colour that means something cannot also
// be a preference — picking one would collapse "needs you" onto "selected".
const ACCENTS = ["#539bf5", "#986ee2", "#39c5cf", "#e879a6", "#7f96c4", "#adbac7"];

function dim(hex, amount) {
  const n = parseInt(hex.slice(1), 16);
  const mix = (c) => Math.round(c * amount);
  return `#${[(n >> 16) & 255, (n >> 8) & 255, n & 255].map((c) => mix(c).toString(16).padStart(2, "0")).join("")}`;
}

/// The accent to start with.
///
/// A stored preference is only honoured if it is still one of the colours this
/// palette offers. Changing the palette otherwise leaves anyone who ever
/// clicked a swatch looking at the old accent over the new colours, and the
/// change appears not to have happened — which is exactly what it did.
function chosenAccent() {
  let saved = null;
  try {
    saved = localStorage.getItem("accent");
  } catch {
    saved = null;
  }
  // A stored swatch survives only if it is still on the palette; a custom pick
  // survives if it is a well-formed hex. Without the second clause the custom
  // colour picker persisted nothing — it reset to blue on the next launch.
  return ACCENTS.includes(saved) || /^#[0-9a-f]{6}$/i.test(saved || "")
    ? saved
    : ACCENTS[0];
}

/// The interface accent only. Amber is not offered here: it means "this needs
/// you", and a colour that means something cannot also be a preference.
function useAccent(hex) {
  const root = document.documentElement.style;
  root.setProperty("--accent", hex);
  root.setProperty("--accent-soft", dim(hex, 0.6));
  root.setProperty("--accent-wash", `${hex}1f`);
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
  useAccent(chosenAccent());
  el("menu").showModal();
});
on("custom-accent", "input", (e) => useAccent(e.target.value));
on("menu-close", "click", () => el("menu").close());
on("detail-close", "click", () => el("detail-text").close());
on("code-close", "click", () => el("code").close());
useAccent(chosenAccent());

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
    if (s || FLEETWIDE.includes(pane)) await painters[pane](s);
  }, delay);
}

async function paneTick() {
  const s = current();
  if (s || FLEETWIDE.includes(pane)) {
    try {
      await painters[pane](s);
    } catch (e) {
      say(String(e));
    }
  }
  // The conversation is worth looking at often; everything else is a list that
  // changes when the engine says so.
  setTimeout(paneTick, pane === "talk" ? 250 : 600);
}

draw();
startStream();
paneTick();
// Slow, because it is no longer how the window learns anything urgent. What is
// urgent arrives on the stream; this catches the measurements that nothing
// publishes, and puts right anything the patching above got slightly wrong.
setInterval(draw, 4000);


