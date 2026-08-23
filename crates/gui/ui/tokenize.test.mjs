// Checks for the highlighter in app.js.
//
//     node crates/gui/ui/tokenize.test.mjs
//
// The tokeniser is extracted from app.js rather than imported, because app.js
// reaches for `window` at load and there is no window here. That is a little
// crude, and it is worth it: the alternative is a build step for one file.
//
// The load-bearing check is the round trip. A highlighter that loses or
// reorders a character is worse than no highlighter, because the code you are
// reading is then not the code that is there.
import fs from "fs";
const src = fs.readFileSync(new URL("app.js", import.meta.url), "utf8");
const from = src.indexOf("const WORDS = {");
const to = src.indexOf("/// Show a body of code in the large view");
const code = src.slice(from, to);
const mod = new Function(code + "\nreturn { tokenize, tokenizeDiff, intoLines, langOf };")();

const fail = [];
const ok = (cond, what) => { if (!cond) fail.push(what); };
const classOf = (toks, word) => (toks.find(([, t]) => t === word) || [])[0];

// Rust
const rust = `// a comment
pub fn main() -> Result<()> {
    let n = 0x1f_u8;
    let s = "a \\" quoted string";
    #[derive(Debug)]
    struct Thing;
}`;
let t = mod.tokenize(rust, "rust");
ok(classOf(t, "// a comment") === "com", "rust line comment");
ok(classOf(t, "pub") === "kw", "rust keyword");
ok(classOf(t, "main") === "fnc", "rust call");
ok(classOf(t, "0x1f_u8") === "num", "rust suffixed hex");
ok(t.some(([c, x]) => c === "str" && x.includes('\\"')), "rust string keeps its escape");
ok(classOf(t, "Result") === "typ", "rust type");
ok(t.some(([c, x]) => c === "attr" && x.startsWith("#[derive")), "rust attribute");

// Python triple quotes span lines
t = mod.tokenize('x = """one\ntwo"""\n# after', "python");
ok(t.some(([c, x]) => c === "str" && x.includes("\n")), "python triple-quoted string spans lines");
ok(classOf(t, "# after") === "com", "python comment after it");

// Unterminated string must not swallow the world or throw
t = mod.tokenize('let s = "never closed\nlet n = 1;', "rust");
ok(t.length > 0, "unterminated string still tokenises");
ok(classOf(t, "let") === "kw", "and the next line is still read");

// Shell
t = mod.tokenize('echo "$HOME/x" # note', "sh");
ok(classOf(t, "# note") === "com", "shell comment");
ok(t.some(([c, x]) => c === "str" && x.includes("$HOME")), "shell string");

// JSON keys
t = mod.tokenize('{"name": "x", "n": 12}', "json");
ok(t.some(([c, x]) => c === "str" && x === '"name"'), "json key is a string");
ok(classOf(t, "12") === "num", "json number");

// Diff
t = mod.tokenizeDiff("@@ -1,2 +1,3 @@\n-old\n+new\n ctx");
ok(t.some(([c, x]) => c === "dhunk" && x.startsWith("@@")), "diff hunk");
ok(t.some(([c, x]) => c === "dadd" && x === "+new"), "diff addition");
ok(t.some(([c, x]) => c === "ddel" && x === "-old"), "diff deletion");

// Round trip: colouring must never change the text
for (const [text, lang] of [[rust, "rust"], ['{"a":1}', "json"], ["x = 1 # c", "python"]]) {
  const joined = mod.tokenize(text, lang).map(([, x]) => x).join("");
  ok(joined === text, `round trip preserves ${lang} exactly`);
}
// Lines
const lines = mod.intoLines(mod.tokenize(rust, "rust"));
ok(lines.length === rust.split("\n").length, `line count matches (${lines.length} vs ${rust.split("\n").length})`);

// Extensions
ok(mod.langOf("/a/b/main.rs") === "rust", "rs is rust");
ok(mod.langOf("Dockerfile") === "sh", "Dockerfile is shell");
ok(mod.langOf("x.unknownext") === "plain", "unknown falls back to plain");

// Speed, on something the size of a real file
const big = rust.repeat(2000);
const t0 = process.hrtime.bigint();
mod.tokenize(big, "rust");
const ms = Number(process.hrtime.bigint() - t0) / 1e6;
ok(ms < 400, `fast enough on ${(big.length/1024|0)}KB (took ${ms.toFixed(0)}ms)`);

console.log(fail.length ? "FAILED:\n  " + fail.join("\n  ") : `all checks passed (${(big.length/1024|0)}KB in ${ms.toFixed(0)}ms)`);
process.exit(fail.length ? 1 : 0);
