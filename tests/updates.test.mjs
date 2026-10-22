import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";
import test from "node:test";
import ts from "typescript";

function load(name, mocks = {}) {
  const module = { exports: {} };
  const source = readFileSync(new URL(`../src/${name}.ts`, import.meta.url), "utf8");
  const code = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText;
  vm.runInNewContext(`(function(require,module,exports){${code}\n})`, { Date })(id => mocks[id] ?? {}, module, module.exports);
  return module.exports;
}
const { UpdateController } = load("updates");
const { renderAbout } = load("about", { "./icons": { icon: () => "" } });
const defer = () => { let resolve, reject; const promise = new Promise((yes, no) => { resolve = yes; reject = no; }); return { promise, resolve, reject }; };
function fixture(overrides = {}) {
  const calls = [];
  const update = {
    version: "0.2.0", body: "Changes", date: "2026-10-22T00:00:00Z",
    download: async (event, options) => { calls.push(["download", options]); event({ event: "Started", data: {} }); event({ event: "Progress", data: { chunkLength: 4 } }); event({ event: "Finished" }); },
    install: async () => { calls.push(["install"]); },
    close: async () => { calls.push(["close"]); },
    ...overrides,
  };
  const controller = new UpdateController(async (options) => { calls.push(["check", options]); return update; }, async () => { calls.push(["restart"]); }, () => {});
  return { controller, update, calls };
}

test("manual check, separate consent and explicit restart are required", async () => {
  const { controller: c, calls } = fixture();
  assert.equal(c.state.phase, "idle"); assert.equal(calls.length, 0);
  await c.installConfirmed(); await c.restartNow(); c.confirm();
  assert.equal(calls.length, 0);
  await c.checkNow();
  assert.equal(c.state.phase, "available"); assert.equal(calls.length, 1);
  await c.installConfirmed(); assert.equal(calls.length, 1);
  c.confirm(); c.cancelConfirm(); assert.equal(c.state.phase, "available");
  c.confirm(); await c.installConfirmed();
  assert.equal(c.state.phase, "installed"); assert.equal(c.state.downloaded, 4);
  assert.deepEqual(calls.map(x => x[0]), ["check", "download", "install", "close"]);
  await c.checkNow(); assert.equal(c.state.phase, "installed");
  await c.restartNow(); assert.equal(calls.at(-1)[0], "restart");
});

test("duplicate checks and installs are serialized", async () => {
  const checked = defer(), downloaded = defer(); let checks = 0, downloads = 0, installs = 0;
  const c = new UpdateController(() => { checks++; return checked.promise; }, async () => {}, () => {});
  const first = c.checkNow(); await c.checkNow(); assert.equal(checks, 1);
  checked.resolve({ version: "0.2.0", download: () => { downloads++; return downloaded.promise; }, install: async () => { installs++; }, close: async () => {} });
  await first; c.confirm(); const installing = c.installConfirmed();
  await c.checkNow(); await c.installConfirmed(); c.cancelConfirm();
  assert.equal(c.state.phase, "downloading"); assert.equal(downloads, 1); assert.equal(installs, 0);
  downloaded.resolve(); await installing; assert.equal(installs, 1);
});

test("Finished does not install before signature verification resolves", async () => {
  let installs = 0;
  const { controller: c, calls } = fixture({ download: async (event) => { event({ event: "Finished" }); throw new Error("signature verification failed"); }, install: async () => { installs++; } });
  await c.checkNow(); c.confirm(); await c.installConfirmed();
  assert.equal(installs, 0); assert.equal(c.state.phase, "error");
  assert.match(c.state.error, /signature verification failed/);
  assert.equal(calls.at(-1)[0], "close");
  c.confirm(); await c.installConfirmed(); assert.equal(installs, 0);
  await c.checkNow(); assert.equal(c.state.phase, "available");
});

test("network and missing manifest errors can recover without downloads", async () => {
  let attempts = 0;
  const c = new UpdateController(async () => { if (++attempts === 1) throw new Error("404 latest.json"); return null; }, async () => {}, () => {});
  await c.checkNow(); assert.equal(c.state.phase, "error"); assert.equal(c.busy, false);
  await c.checkNow(); assert.equal(c.state.phase, "current"); assert.ok(c.state.checkedAt); assert.equal(c.state.error, undefined);
});

test("new checks release old update handles and clear old notes", async () => {
  const { controller: c, calls } = fixture();
  await c.checkNow(); await c.checkNow();
  assert.deepEqual(calls.map(x => x[0]), ["check", "close", "check"]);
});

test("installer failures release resources and require a fresh check", async () => {
  const { controller: c, calls } = fixture({ install: async () => { throw new Error("permission denied"); } });
  await c.checkNow(); c.confirm(); await c.installConfirmed();
  assert.equal(c.state.phase, "error"); assert.equal(c.busy, false); assert.match(c.state.error, /permission denied/);
  assert.equal(calls.at(-1)[0], "close");
});

test("restart failures retain installed state and allow retry", async () => {
  let restarts = 0;
  const { controller: c } = fixture();
  c.restart = async () => { if (++restarts === 1) throw new Error("OS refused"); };
  await c.checkNow(); c.confirm(); await c.installConfirmed(); await c.restartNow();
  assert.equal(c.state.phase, "installed"); assert.match(c.state.error, /Quit and reopen/);
  await c.restartNow(); assert.equal(c.state.phase, "restarting"); assert.equal(c.state.error, undefined);
});

const meta = { version: "0.1.1", author: "Mohamed Elashri", platform: "linux", arch: "x86_64", updateSupported: true, updateHint: "" };
test("About escapes remote notes, errors and metadata; invalid dates stay hidden", () => {
  const { controller: c } = fixture();
  c.state = { phase: "available", version: '<img onerror="bad">', notes: "<script>bad()</script>", date: "invalid", downloaded: 0, error: "<svg onload=bad()>" };
  const html = renderAbout({ ...meta, version: "<img>" }, null, c);
  assert.ok(!html.includes("<script>")); assert.ok(!html.includes("<svg")); assert.ok(!html.includes("<img"));
  assert.ok(html.includes("&lt;script&gt;")); assert.ok(!html.includes("Invalid Date")); assert.ok(html.includes("aria-modal=\"true\""));
});

test("unsupported installations allow manual checks but never offer install", () => {
  const { controller: c } = fixture(); c.state.phase = "available";
  const html = renderAbout({ ...meta, updateSupported: false, updateHint: "Use AppImage" }, null, c);
  assert.match(html, /check-updates/); assert.match(html, /Use AppImage/); assert.ok(!html.includes("confirm-update"));
  assert.ok(!renderAbout(null, "IPC failed", c).includes("check-updates"));
});

test("unknown download length is indeterminate and progress is bounded", () => {
  const { controller: c } = fixture(); c.state.phase = "downloading";
  assert.match(renderAbout(meta, null, c), /<progress[^>]*max="100"\s*>/);
  c.state.total = 10; c.state.downloaded = 20;
  assert.match(renderAbout(meta, null, c), /value="100"/);
});
