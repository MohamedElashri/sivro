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
import { execFileSync } from "node:child_process";
import { startProcess } from "./process.mjs";
import { existsSync, mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join, resolve } from "node:path";

const root = resolve(new URL("../..", import.meta.url).pathname);
const expectedVersion = JSON.parse(readFileSync(join(root, "package.json"), "utf8")).version;
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
  if (!option("--fixtures")) {
    execFileSync("node", ["scripts/with-duckdb.mjs", "cargo", "test", "--quiet", "--manifest-path", "src-tauri/Cargo.toml", "--lib", "write_e2e_fixtures", "--", "--ignored"],
      { cwd: root, env: { ...process.env, SIVRO_E2E_DIR: fixtures }, stdio: ["ignore", "ignore", "inherit"] });
  }
  const inner = ["node", JSON.stringify(new URL(import.meta.url).pathname), "--fixtures", JSON.stringify(fixtures), "--bin", JSON.stringify(bin), "--offline-check"].join(" ");
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
const { child: driver, stop: stopDriver } = startProcess(driverBin, ["--port", String(port)], { env: { ...process.env, ...profile }, stdio: ["ignore", "ignore", "pipe"] });
let driverLog = ""; driver.stderr.on("data", (d) => { driverLog += d; });
driver.on("error", error => { driverLog += error.message; });

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function wd(method, path, body) {
  const response = await fetch(base + path, { method, headers: { "content-type": "application/json" }, body: body === undefined ? undefined : JSON.stringify(body), signal: AbortSignal.timeout(30_000) });
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
    catch (error) { if (attempt >= 5 || !/stale element|no such element/.test(error.message)) throw new Error(`Click ${css}: ${error.message}`); await sleep(100); }
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
async function chooseTheme(value) {
  await click('#theme-preference-trigger');
  await click(`.select-popup [data-value="${value}"]`);
  await waitFor(`${value} appearance`, () => exec("return document.documentElement.dataset.theme === arguments[0]", value));
}

const cases = [
  { file: "people.h5", dialog: "confirm-hdf5", expect: ["9007199254740993", "Alice"] },
  { file: "classic.h5", dialog: "confirm-hdf5", group: "/arrays", dataset: "/arrays/cube", slice: true, expect: ["9007199254741007", "9007199254741015"] },
  { file: "layout-v5.h5", dialog: "confirm-hdf5", group: "/filtered", dataset: "/filtered/btree", slice: true, expect: ["9007199254741001", "9007199254741008"] },
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
  await waitFor("tauri-driver", () => fetch(`${base}/status`, { signal: AbortSignal.timeout(2000) }).then((r) => r.ok), 15_000);
  sid = (await wd("POST", "/session", { capabilities: { alwaysMatch: { "tauri:options": { application: bin } } } })).sessionId;
  await waitFor("start screen", present(".hero"));
  await chooseTheme("dark");
  const darkBackground = await exec("return getComputedStyle(document.querySelector('.sidebar')).backgroundColor");
  if (darkBackground !== "rgb(24, 24, 37)") throw new Error(`Dark surface was ${darkBackground}`);
  await exec("location.reload()"); await waitFor("saved dark appearance", () => exec("return document.documentElement.dataset.theme === 'dark' && !!document.querySelector('.hero')"));
  await click('#theme-preference-trigger');
  const hoverOption = await element('.select-popup [data-value="light"]');
  await wd("POST", `/session/${sid}/actions`, { actions: [{ type: "pointer", id: "menu-pointer", parameters: { pointerType: "mouse" }, actions: [{ type: "pointerMove", duration: 100, origin: { "element-6066-11e4-a52e-4f735466cecf": hoverOption }, x: 0, y: 0 }] }] });
  await waitFor("dropdown pointer highlight", () => exec("return document.querySelector('.select-option.is-active')?.dataset.value === 'light'"));
  await exec("document.querySelector('#theme-preference-trigger').dispatchEvent(new KeyboardEvent('keydown',{key:'Home',bubbles:true,cancelable:true})); document.querySelector('#theme-preference-trigger').dispatchEvent(new KeyboardEvent('keydown',{key:'ArrowDown',bubbles:true,cancelable:true}))");
  if (!(await exec("return document.querySelector('.select-option.is-active')?.dataset.value === 'light'"))) throw new Error("Dropdown keyboard highlight failed");
  await exec("document.querySelector('#theme-preference-trigger').dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true,cancelable:true}))");
  if (await exec("return !!document.querySelector('.select-popup')")) throw new Error("Escape did not dismiss the dropdown");
  await chooseTheme("light");
  results.push({ file: "Appearance", ms: 0, steps: ["dark surfaces", "saved choice", "keyboard highlight", "Escape", "light mode"] });
  await click('[data-action="open-about"]');
  await waitFor("About version from the running app", () => exec("return document.querySelector('#about-version')?.textContent === arguments[0]", expectedVersion));
  const about = await exec("return document.querySelector('.about-modal')?.innerText ?? ''");
  for (const token of ["Mohamed Elashri", "MIT", "No automatic checks", "Check for updates"]) {
    if (!about.includes(token)) throw new Error(`About is missing ${JSON.stringify(token)}`);
  }
  if (args.includes("--keep")) {
    const shot = await wd("GET", `/session/${sid}/screenshot`);
    writeFileSync(join(work, "about.png"), Buffer.from(shot, "base64"));
  }
  if (args.includes("--offline-check")) {
    await click('[data-action="check-updates"]');
    await waitFor("recoverable offline update error", () => exec("return document.querySelector('.about-error')?.textContent.includes('Could not check for updates')"));
    if (!(await exec("return !!document.querySelector('[data-action=\"check-updates\"]')"))) throw new Error("Offline update check cannot be retried");
  }
  if (!(await exec("return document.activeElement?.closest('.about-modal') !== null"))) throw new Error("About did not receive keyboard focus");
  await click('#about-close');
  await waitFor("About focus restoration", () => exec("return document.activeElement?.id === 'open-about'"));
  if (args.includes("--keep")) {
    await chooseTheme("dark"); await click('[data-action="open-about"]');
    const shot = await wd("GET", `/session/${sid}/screenshot`); writeFileSync(join(work, "about-dark.png"), Buffer.from(shot, "base64"));
    await click('#about-close'); await chooseTheme("light");
  }
  results.push({ file: "About", ms: 0, steps: ["version", "author", "links", "manual updates", "focus restored"] });
  const paths = cases.map((c) => join(fixtures, c.file));
  await exec("localStorage.setItem('sivro-recents', JSON.stringify(arguments[0])); location.reload();", paths);
  await waitFor("recent files", () => exec("return document.querySelectorAll('.recent-item').length === arguments[0]", paths.length));
  for (const test of cases) {
    const started = Date.now(), steps = [];
    const path = join(fixtures, test.file);
    try {
      await click(`.recent-item[data-path="${attr(path)}"]`); steps.push("open");
      if (test.dialog) {
        if (test.slice) {
          await waitFor("HDF5 groups", present(`[data-action="browse-hdf5"][data-group="${test.group}"]`));
          await click(`[data-action="browse-hdf5"][data-group="${test.group}"]`);
          await waitFor("HDF5 cube", present(`[data-hdf5="object"][value="${test.dataset}"]:not([disabled])`));
          await click(`[data-hdf5="object"][value="${test.dataset}"]`);
          for (const [field, value] of [["index", "2"], ["rowStart", "1"], ["rowLimit", "1"]]) {
            await exec("const input=document.querySelector('[data-hdf5=\"'+arguments[0]+'\"]'); if(input) { input.value=arguments[1]; input.dispatchEvent(new Event('input',{bubbles:true})); }", field, value);
          }
          steps.push("browse group", "slice cube");
          if (args.includes("--keep")) {
            const shot = await wd("GET", `/session/${sid}/screenshot`);
            writeFileSync(join(work, `hdf5-picker-${test.file}.png`), Buffer.from(shot, "base64"));
          }
        }
        await waitFor(`${test.file} dialog`, present(`[data-action="${test.dialog}"]:not([disabled])`));
        await click(`[data-action="${test.dialog}"]`); steps.push("confirm dialog");
      }
      await waitFor(`${test.file} view`, present(test.document ? ".doc-view .doc-row" : "table.grid tbody tr"));
      if (test.file === "people.parquet") {
        await exec("const input=document.querySelector('#quick-search'); input.value='Alice'; input.dispatchEvent(new Event('input',{bubbles:true}))");
        if (!(await exec("return !!document.querySelector('.tab-close.has-pending')"))) throw new Error("Pending view indicator is missing");
        for (const mode of ["dark", "light"]) {
          await chooseTheme(mode);
          if (!(await exec("return !!document.querySelector('.tab-close.has-pending') && !!document.querySelector('table.grid tbody tr')"))) throw new Error("Theme change discarded the view or pending changes");
          if (args.includes("--keep")) {
            const shot = await wd("GET", `/session/${sid}/screenshot`); writeFileSync(join(work, `grid-${mode}.png`), Buffer.from(shot, "base64"));
          }
        }
        await exec("const input=document.querySelector('#quick-search'); input.value=''; input.dispatchEvent(new Event('input',{bubbles:true}))");
        if (await exec("return !!document.querySelector('.tab-close.has-pending')")) throw new Error("Pending view indicator did not clear after reverting");
        steps.push("pending view dot");
      }
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
  if (sid) {
    const shot = await wd("GET", `/session/${sid}/screenshot`).catch(() => null);
    if (shot) writeFileSync(join(work, "failure.png"), Buffer.from(shot, "base64"));
    const html = await exec("return document.documentElement.outerHTML").catch(() => "");
    writeFileSync(join(work, "failure.html"), html);
  }
} finally {
  if (sid) await wd("DELETE", `/session/${sid}`).catch(() => undefined);
  await stopDriver();
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
console.log(`GUI end-to-end: ${cases.length} input cases passed with ${bin}`);
