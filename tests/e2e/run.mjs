#!/usr/bin/env node
// GUI end-to-end smoke for the real Tauri application, driven through
// tauri-driver + WebKitWebDriver (Linux). No test framework or npm
// dependency: the W3C WebDriver protocol is plain HTTP.
//
//   node tests/e2e/run.mjs [--fixtures DIR] [--bin PATH] [--keep]
//
// Requires a `tauri build` binary (plain `cargo build` produces a dev-mode
// binary that expects the Vite dev server), `tauri-driver` on PATH or in
// ~/.cargo/bin, and WebKitWebDriver (Debian/Ubuntu: webkit2gtk-driver).
import { spawn, execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join, resolve } from "node:path";

const root = resolve(new URL("../..", import.meta.url).pathname);
const args = process.argv.slice(2);
const option = (name) => { const i = args.indexOf(name); return i >= 0 ? args[i + 1] : undefined; };
const bin = resolve(option("--bin") ?? process.env.SIVRO_BIN ?? join(root, "src-tauri/target/release/sivro"));
const work = mkdtempSync(join(tmpdir(), "sivro-e2e-"));
const fixtures = resolve(option("--fixtures") ?? join(work, "fixtures"));
const port = Number(process.env.SIVRO_E2E_PORT ?? 4444);
const base = `http://127.0.0.1:${port}`;

// --offline: prepare fixtures normally, then rerun inside a user + network
// namespace whose only interface is loopback (for WebDriver).
if (args.includes("--offline")) {
  execFileSync("node", ["scripts/with-duckdb.mjs", "cargo", "test", "--quiet", "--manifest-path", "src-tauri/Cargo.toml", "--lib", "write_e2e_fixtures", "--", "--ignored"],
    { cwd: root, env: { ...process.env, SIVRO_E2E_DIR: fixtures }, stdio: ["ignore", "ignore", "inherit"] });
  const inner = ["node", JSON.stringify(new URL(import.meta.url).pathname), "--fixtures", JSON.stringify(fixtures), "--bin", JSON.stringify(bin)].join(" ");
  const check = "(timeout 3 sh -c 'getent hosts example.com' >/dev/null && echo 'network reachable' && exit 1) || true";
  execFileSync("unshare", ["-rn", "sh", "-c", `ip link set lo up && ${check} && echo 'offline namespace: loopback only, no DNS' && ${inner}`], { stdio: "inherit" });
  rmSync(work, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
  process.exit(0);
}
if (!existsSync(bin)) throw new Error(`Application binary not found: ${bin}. Run \`npm run tauri:build -- --no-bundle\` first.`);
if (!option("--fixtures")) {
  execFileSync("node", ["scripts/with-duckdb.mjs", "cargo", "test", "--quiet", "--manifest-path", "src-tauri/Cargo.toml", "--lib", "write_e2e_fixtures", "--", "--ignored"],
    { cwd: root, env: { ...process.env, SIVRO_E2E_DIR: fixtures }, stdio: ["ignore", "ignore", "inherit"] });
}

// Isolated profile: recents, WebView storage and scratch live under `work`.
const profile = Object.fromEntries(["XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME"].map((k) => {
  const dir = join(work, k.toLowerCase()); mkdirSync(dir); return [k, dir];
}));
const driverBin = [join(homedir(), ".cargo/bin/tauri-driver"), "tauri-driver"].find((p) => p === "tauri-driver" || existsSync(p));
const driver = spawn(driverBin, ["--port", String(port)], { env: { ...process.env, ...profile }, stdio: ["ignore", "ignore", "pipe"] });
let driverLog = ""; driver.stderr.on("data", (d) => { driverLog += d; });

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function wd(method, path, body) {
  const response = await fetch(base + path, { method, headers: { "content-type": "application/json" }, body: body === undefined ? undefined : JSON.stringify(body) });
  const json = await response.json();
  if (json.value?.error) throw new Error(`${method} ${path}: ${json.value.error}: ${json.value.message}`);
  return json.value;
}
let sid = "";
const exec = (script, ...values) => wd("POST", `/session/${sid}/execute/sync`, { script, args: values });
const element = async (css) => Object.values(await wd("POST", `/session/${sid}/element`, { using: "css selector", value: css }))[0];
// The UI re-renders by replacing markup, so a found element can go stale
// before the click lands; find it again in that case.
async function click(css) {
  for (let attempt = 0; ; attempt++) {
    try { return await wd("POST", `/session/${sid}/element/${await element(css)}/click`, {}); }
    catch (error) { if (attempt >= 5 || !/stale element|no such element/.test(error.message)) throw error; await sleep(100); }
  }
}
async function waitFor(label, probe, ms = 30_000) {
  const started = Date.now();
  while (Date.now() - started < ms) {
    const value = await probe().catch(() => null);
    if (value) return value;
    await sleep(100);
  }
  throw new Error(`Timed out waiting for ${label}`);
}
const present = (css) => () => exec("return !!document.querySelector(arguments[0])", css);
const mainText = () => exec("return document.querySelector('main')?.innerText ?? ''");
const attr = (value) => value.replace(/["\\]/g, "\\$&");

const cases = [
  { file: "people.parquet", expect: ["9007199254740993", "Alice"] },
  { file: "people.csv", dialog: "confirm-delimited", expect: ["9007199254740993", "Bob"], count: true },
  { file: "people.tsv", dialog: "confirm-delimited", expect: ["9007199254740993", "Bob"], count: true },
  { file: "document.json", document: true, expect: ["0.123456789012345678901234567890", "Array · 3 items"] },
  { file: "records.jsonl", expect: ["9007199254740993", "1e+400"] },
  { file: "workbook.xlsx", dialog: "confirm-excel", expect: ["2024-02-29", "Émile ✓ <b>", "$excel_error"] },
  { file: "people.sqlite", dialog: "confirm-sqlite", expect: ["9007199254740993", "2 bytes · 0x00FF"], count: true },
];

const results = [];
let failure = null;
try {
  await waitFor("tauri-driver", () => fetch(`${base}/status`).then((r) => r.ok), 15_000);
  sid = (await wd("POST", "/session", { capabilities: { alwaysMatch: { "tauri:options": { application: bin } } } })).sessionId;
  await waitFor("start screen", present(".hero"));
  const paths = cases.map((c) => join(fixtures, c.file));
  await exec("localStorage.setItem('sivro-recents', JSON.stringify(arguments[0])); location.reload();", paths);
  await waitFor("recent files", () => exec("return document.querySelectorAll('.recent-item').length === arguments[0]", paths.length));
  for (const test of cases) {
    const started = Date.now(), steps = [];
    const path = join(fixtures, test.file);
    try {
      await click(`.recent-item[data-path="${attr(path)}"]`); steps.push("open");
      if (test.dialog) {
        await waitFor(`${test.file} dialog`, present(`[data-action="${test.dialog}"]:not([disabled])`));
        await click(`[data-action="${test.dialog}"]`); steps.push("confirm dialog");
      }
      await waitFor(`${test.file} view`, present(test.document ? ".doc-view .doc-row" : "table.grid tbody tr"));
      if (test.document) {
        await click(".doc-toggle"); steps.push("expand node");
        await waitFor("expanded children", present(".doc-children .doc-row"));
      } else {
        await click('.th-main[data-action="sort-col"]'); steps.push("sort");
        await waitFor("sorted page", present("th.sorted"));
        await waitFor("sorted page loaded", async () => !(await exec("return !!document.querySelector('.grid-scroll.is-loading, .grid-wrap.loading')")));
        await click('tr[data-action="select-row"] td.rownum-col'); steps.push("inspect row");
        await waitFor("row inspector", present(".drawer"));
        await click('[data-action="close-drawer"]');
        if (test.count) {
          await click('[data-action="count-rows"]'); steps.push("exact count");
          await waitFor("exact count", async () => (await exec("return document.querySelector('.statusbar')?.innerText ?? ''")).includes("total"));
        }
      }
      const text = await mainText();
      for (const token of test.expect) if (!text.includes(token)) throw new Error(`${test.file}: expected ${JSON.stringify(token)} on screen`);
      steps.push(`saw ${test.expect.length} exact values`);
      await click(`.tab-close[data-path="${attr(path)}"]`); steps.push("close");
      await waitFor("tab closed", async () => !(await exec("return !!document.querySelector(`.dataset-tab[data-path=\"${arguments[0]}\"]`)", attr(path))));
      results.push({ file: test.file, ms: Date.now() - started, steps });
    } catch (error) {
      const shot = await wd("GET", `/session/${sid}/screenshot`).catch(() => null);
      if (shot) writeFileSync(join(work, `${test.file}.png`), Buffer.from(shot, "base64"));
      throw new Error(`${test.file} after [${steps.join(", ")}]: ${error.message}`);
    }
  }
  // Fonts and scripts are bundled; any remote resource would be listed here.
  const remote = await exec("return performance.getEntriesByType('resource').map(e => e.name).filter(n => !/^(tauri|ipc|data|blob):|^http:\\/\\/(ipc|tauri)\\.localhost/.test(n))");
  results.push({ file: "remote resources", ms: 0, steps: remote.length ? remote : ["none"] });
  if (remote.length) throw new Error(`Remote resources were requested: ${remote.join(", ")}`);
} catch (error) {
  failure = error;
} finally {
  if (sid) await wd("DELETE", `/session/${sid}`).catch(() => undefined);
  const exited = new Promise((done) => driver.once("exit", done));
  driver.kill();
  await Promise.race([exited, sleep(5000)]);
}

for (const r of results) console.log(`✔ ${r.file.padEnd(16)} ${String(r.ms).padStart(6)} ms  ${r.steps.join(" → ")}`);
if (failure) {
  console.error(`✖ ${failure.message}\n  artifacts: ${work}`);
  if (driverLog.trim()) console.error(driverLog.trim().split("\n").slice(-10).join("\n"));
  process.exit(1);
}
// The app may still flush its profile while exiting; retry the cleanup.
if (args.includes("--keep")) console.log(`kept fixtures and profile: ${work}`);
else rmSync(work, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
console.log(`GUI end-to-end: ${cases.length} formats passed with ${bin}`);
