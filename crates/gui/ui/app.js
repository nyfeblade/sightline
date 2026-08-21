// Placeholder wiring: one poll, the same cadence the terminal view uses, so the
// command layer is exercised for real rather than mocked.
const invoke = window.__TAURI__.core.invoke;
const el = (id) => document.getElementById(id);

function line(name, meta, cls = "") {
  return `<div class="row"><span class="name ${cls}">${name}</span><span class="meta">${meta}</span></div>`;
}

async function draw() {
  const health = await invoke("readiness");
  el("checks").innerHTML =
    `<div class="detail">holds sessions: ${health.backend}</div>` +
    health.checks
      .map((c) => line(c.name, c.detail, c.ok ? "ok" : "miss"))
      .join("");

  const list = await invoke("sessions");
  const working = list.filter((s) => s.state === "working" || s.state === "running").length;
  const asking = list.filter((s) => s.asking).length;
  el("summary").textContent =
    `${list.length} sessions · ${working} working · ${asking} awaiting input`;
  el("sessions").innerHTML = list
    .map((s) =>
      line(
        `${s.name} — ${s.asking ? "needs permission" : s.state}`,
        `${s.cwd} · ctx ${s.context}/${s.window}`,
      ),
    )
    .join("");
}

el("tui").addEventListener("click", async () => {
  try {
    el("note").textContent = `opened in ${await invoke("open_tui")}`;
  } catch (e) {
    el("note").textContent = String(e);
  }
});

draw();
setInterval(draw, 1000);
