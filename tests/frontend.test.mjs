import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";
import vm from "node:vm";
import test from "node:test";
import ts from "typescript";

const flush = () => new Promise(setImmediate);
function frontend(options = {}) {
  const requests = [], calls = [], copied = [], modules = new Map();
  let nextOperation = 0;
  const context = vm.createContext({
    navigator: { userAgent: "Mac" }, localStorage: { getItem: () => null, setItem() {} },
    document: { readyState: options.readyState ?? "loading", addEventListener() {}, getElementById: () => null, querySelector: () => null },
    performance, TextEncoder, setTimeout: () => 0, clearTimeout() {}, console,
  });
  function load(name) {
    if (modules.has(name)) return modules.get(name);
    const source = readFileSync(fileURLToPath(new URL(`../src/${name}.ts`, import.meta.url)), "utf8");
    const code = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText;
    const module = { exports: {} };
    const bridge = name === "main" ? `
      module.exports.testing = {
        renderGrid, renderDrawer, renderPagination, rowJson, handleAction, loadPage, validateFilter: require("./filters").validateFilter, filterInputType: require("./filters").filterInputType,
        loadProfile, loadCount, refreshTab, openFileByPath, runExport, queryFor,
        openDelimited, previewDelimited, changeDelimited, confirmDelimited, cancelDelimited,
        renderDelimited: require("./delimited").renderDelimited, renderExportModal, defaultExportFileName,
        loadDocument, copyDocument, loadRawRecord, renderMain, renderSidebar, renderFormatPrompt, getFormatPrompt() { return formatPrompt; },
        openExcel, changeExcel, confirmExcel, cancelExcel, renderExcel: require("./excel").renderExcel, getExcelDialog() { return excelDialog; },
        openSqlite, changeSqlite, confirmSqlite, cancelSqlite, renderSqlite: require("./sqlite").renderSqlite, getSqliteDialog() { return sqliteDialog; },
        getDialog() { return delimitedDialog; },
        openAbout, closeAbout, updates, renderTitlebar, chooseAndOpenFile, init, hasPendingView, renderTabStrip,
        getAppMetadata() { return appMetadata; }, getAboutOpen() { return aboutOpen; },
        useTab(tab) { tabs = [tab]; activePath = tab.info.path; },
        useTabs(values, active) { tabs = values; activePath = active; },
        getTabs() { return tabs; }, getLastExport() { return lastExport; },
        showExport(format) { exportModalOpen = true; if (format) exportFormat = format; }
      };` : "";
    const require = (id) => {
      if (id === "@tauri-apps/api/core") return {
        invoke(command, args) {
          calls.push({ command, args });
          if (command === "begin_operation" && !options.deferBegin) return Promise.resolve({ id: `op-${++nextOperation}` });
          if (command === "cancel_operation") return Promise.resolve({ status: "cancel_pending" });
          if (command === "discard_preview") return Promise.resolve();
          if (command === "close_dataset") return Promise.resolve({ closed: true });
          if (command === "app_metadata") return Promise.resolve({ version: "0.1.1", author: "Mohamed Elashri", platform: "linux", arch: "x86_64", updateSupported: options.updateSupported ?? true, updateHint: "Use AppImage" });
          return new Promise((resolve, reject) => requests.push({ command, args, resolve, reject }));
        },
      };
      if (id === "@tauri-apps/api/window") return { getCurrentWindow: () => ({ onCloseRequested: async () => { calls.push({ command: "register-close" }); } }) };
      if (id === "@tauri-apps/plugin-dialog") return { save: async () => options.saveDestination ?? "/export.parquet", open: async () => { calls.push({ command: "file-dialog" }); return options.openDialog ? options.openDialog() : null; } };
      if (id === "@tauri-apps/plugin-updater") return { check: async () => { calls.push({ command: "check-update" }); return options.update ?? null; } };
      if (id === "@tauri-apps/plugin-process") return { relaunch: async () => { calls.push({ command: "restart-update" }); } };
      if (id === "@tauri-apps/plugin-clipboard-manager") return { writeText: async (value) => copied.push(value) };
      if (id === "./icons") return { icon: () => "" };
      if (id.endsWith(".css")) return {};
      if (id.startsWith(".")) return load(path.posix.normalize(path.posix.join(path.posix.dirname(name), id)));
      return {};
    };
    vm.runInContext(`(function(require, module, exports) { ${code}\n${bridge}\n})`, context)(require, module, module.exports);
    modules.set(name, module.exports);
    return module.exports;
  }
  return { ...load("main").testing, requests, calls, copied };
}
const number = (value) => ({ kind: "number", value, logicalType: "BIGINT", byteLength: String(value.length), truncated: false });
const text = (value) => ({ kind: "text", value, byteLength: String(Buffer.byteLength(value)), truncated: false });
const json = (value) => ({ kind: "json", value, byteLength: String(Buffer.byteLength(value)), truncated: false });
function descriptor(suffix = "a", generation = 1) {
  return {
    apiVersion: 1, sessionId: `session-${suffix}`, generation, format: "parquet", path: `/${suffix}.parquet`, fileName: `${suffix}.parquet`, fileSize: "1024",
    object: { id: `object-${suffix}-${generation}`, label: "Data", kind: "table", rowCount: { status: "known", value: "1" }, warnings: [],
      capabilities: { page: true, filter: true, sort: true, profile: true, exactCount: true, copy: true, exports: ["parquet", "csv", "json"] },
      columns: [
        { id: "col-0", name: "id", sqlName: "field_0", kind: "number", dataType: "BIGINT", nullable: false, operators: ["eq", "ne", "gt", "lt", "is_null", "is_not_null"] },
        { id: "col-1", name: "name", sqlName: "field_1", kind: "text", dataType: "VARCHAR", nullable: true, operators: ["eq", "contains", "is_null", "is_not_null"] },
        { id: "col-2", name: "details", sqlName: "field_2", kind: "nested", dataType: "STRUCT(value BIGINT)", nullable: true, operators: ["contains", "is_null", "is_not_null"] },
      ],
    }, rowGroups: [], createdBy: null, formatVersion: 1,
    policy: { defaultPageSize: 100, maxPageSize: 1000, timeoutMs: 30000, maxCacheEntries: 32, fullCellBytes: 1048576, cellPreviewBytes: 4096, maxExportRows: 10000000 },
  };
}
function pageFor(info, columnIds = info.object.columns.map((c) => c.id), rows) {
  const values = { "col-0": number("9007199254740993"), "col-1": text("Alice"), "col-2": json('{"value":9007199254740993}') };
  return { sessionGeneration: info.generation, objectId: info.object.id, columnIds, rowIds: ["0"], rows: rows ?? [columnIds.map((id) => values[id])], offset: 0, limit: 100, hasMore: false, count: { status: "known", value: "1" }, queryMs: 2 };
}
function dataset(suffix = "a") {
  const info = descriptor(suffix), p = pageFor(info);
  return { info, visibleColumns: [...p.columnIds], pageColumns: [...p.columnIds], page: p,
    currentPage: 1, pageSize: 100, sortColumn: null, sortDesc: false, filters: [], quickSearch: "", loading: false, error: null,
    requestId: 0, selectedRow: 0, profileColumn: null, profile: null, profileLoading: false, profileError: null,
    profileCache: new Map(), profileRequests: new Map(), operations: new Set(), pageOperation: null, countOperation: null,
    pageQuery: { columnIds: [...p.columnIds], pageSize: 100, offset: 0, filter: { kind: "all", terms: [] }, quickSearch: "", sort: null },
    profileRequestId: 0, stale: false, closed: false, refreshing: false,
  };
}
function profileFor(info, columnId = "col-0") {
  return { sessionGeneration: info.generation, objectId: info.object.id, columnId, scope: "current_view", nullCount: "0", nonNullCount: "1", distinctCount: "1", min: number("1"), max: number("1"), topValues: [{ value: number("1"), count: "1" }] };
}

test("column changes preserve grid, drawer and exact copy mappings until the new page arrives", async () => {
  const ui = frontend(), tab = dataset(); ui.useTab(tab);
  const loading = ui.handleAction({ dataset: { action: "toggle-column", col: "col-0" } }); await flush();
  assert.equal(tab.loading, true); assert.equal(tab.visibleColumns.join(","), "col-1,col-2");
  assert.equal(ui.requests[0].args.request.query.columnIds.join(","), "col-1,col-2");
  assert.match(ui.renderGrid(tab), /th-name">id</); assert.match(ui.renderDrawer(tab), /kv-name" title="id">id</);
  assert.equal(ui.rowJson(tab, tab.page.rows[0]), '{\n  "id": 9007199254740993,\n  "name": "Alice",\n  "details": {"value":9007199254740993}\n}');
  await ui.handleAction({ dataset: { action: "copy-row-json" } }); assert.equal(ui.copied[0], ui.rowJson(tab, tab.page.rows[0]));
  await ui.handleAction({ dataset: { action: "copy-cell", row: "0", ci: "2" } }); assert.equal(ui.copied[1], '{"value":9007199254740993}');
  ui.requests[0].resolve(pageFor(tab.info, ["col-1", "col-2"])); await loading;
  assert.equal(tab.loading, false); assert.equal(tab.pageColumns.join(","), "col-1,col-2"); assert.doesNotMatch(ui.renderGrid(tab), /th-name">id</);
  assert.equal(ui.rowJson(tab, tab.page.rows[0]), '{\n  "name": "Alice",\n  "details": {"value":9007199254740993}\n}');
});

test("a superseded page cannot overwrite the latest column mapping and cancels native work", async () => {
  const ui = frontend(), tab = dataset(); ui.useTab(tab);
  const hidden = ui.handleAction({ dataset: { action: "toggle-column", col: "col-0" } }); await flush();
  const restored = ui.handleAction({ dataset: { action: "columns-reset" } }); await flush();
  assert.ok(ui.calls.some((c) => c.command === "cancel_operation"));
  ui.requests[1].resolve(pageFor(tab.info)); await restored;
  ui.requests[0].resolve(pageFor(tab.info, ["col-1", "col-2"])); await hidden;
  assert.equal(tab.pageColumns.join(","), "col-0,col-1,col-2"); assert.equal(tab.page.rows[0][0].value, "9007199254740993");
});

test("failed column reload clears both page data and its mapping", async () => {
  const ui = frontend(), tab = dataset(); ui.useTab(tab);
  const loading = ui.handleAction({ dataset: { action: "toggle-column", col: "col-0" } }); await flush();
  ui.requests[0].reject(new Error("File unavailable")); await loading;
  assert.equal(tab.page, null); assert.equal(tab.pageColumns.length, 0); assert.match(ui.renderGrid(tab), /File unavailable/); assert.equal(ui.renderDrawer(tab), "");
});

test("query requests contain backend handles and typed values rather than paths or SQL", async () => {
  const ui = frontend(), tab = dataset(); ui.useTab(tab);
  tab.filters = [{ column: "col-0", op: "eq", value: "9007199254740993" }]; const work = ui.loadPage(tab.info.path); await flush();
  const request = ui.requests[0].args.request;
  assert.equal(request.sessionId, "session-a"); assert.equal(request.objectId, "object-a-1"); assert.equal(request.generation, 1); assert.equal(request.query.path, undefined);
  assert.equal(request.query.filter.terms[0].columnId, "col-0"); assert.deepEqual(JSON.parse(JSON.stringify(request.query.filter.terms[0].value)), { kind: "number", value: "9007199254740993" });
  ui.requests[0].resolve(pageFor(tab.info)); await work;
});

test("a closed tab rejects late pages and releases its backend session", async () => {
  const ui = frontend(), tab = dataset(); ui.useTab(tab); const work = ui.loadPage(tab.info.path); await flush();
  await ui.handleAction({ dataset: { action: "close-tab", path: tab.info.path } }); ui.requests[0].resolve(pageFor(tab.info)); await work;
  assert.equal(ui.getTabs().length, 0); assert.equal(tab.page, null); assert.equal(tab.closed, true);
  assert.ok(ui.calls.some((c) => c.command === "close_dataset" && c.args.sessionId === "session-a"));
});

test("responses from another object or generation cannot install rows", async () => {
  for (const change of [{ objectId: "object-b-1" }, { sessionGeneration: 2 }, { columnIds: ["col-1"] }]) {
    const ui = frontend(), tab = dataset(); ui.useTab(tab); const work = ui.loadPage(tab.info.path); await flush();
    ui.requests[0].resolve({ ...pageFor(tab.info), ...change }); await work;
    assert.equal(tab.page, null); assert.match(tab.error, /requested schema/);
  }
});

test("refresh clears old headers and blocks stale page/profile responses", async () => {
  const ui = frontend(), tab = dataset(); ui.useTab(tab);
  const oldPage = ui.loadPage(tab.info.path); await flush(); const oldProfile = ui.loadProfile(tab, "col-0"); await flush();
  const refresh = ui.refreshTab(tab); await flush(); assert.equal(tab.page, null); assert.equal(tab.pageColumns.length, 0);
  const fresh = descriptor("a", 2); fresh.object.columns[0].name = "new id";
  ui.requests.find((r) => r.command === "refresh_dataset").resolve(fresh); await flush();
  ui.requests.find((r) => r.command === "column_profile").resolve(profileFor(descriptor("a"))); await oldProfile;
  ui.requests.filter((r) => r.command === "fetch_page")[0].resolve(pageFor(descriptor("a"))); await oldPage;
  const newest = ui.requests.filter((r) => r.command === "fetch_page")[1]; newest.resolve(pageFor(fresh)); await refresh;
  assert.equal(tab.info.generation, 2); assert.equal(tab.profile, null); assert.equal(tab.profileCache.size, 0); assert.match(ui.renderGrid(tab), /new id/); assert.equal(tab.page.sessionGeneration, 2);
});

test("a changed source clears old data and offers explicit refresh", async () => {
  const ui = frontend(), tab = dataset(); ui.useTab(tab); const work = ui.loadPage(tab.info.path); await flush();
  ui.requests[0].reject({ code: "stale_dataset", message: "The source changed. Refresh it." }); await work;
  assert.equal(tab.stale, true); assert.equal(tab.page, null); assert.match(ui.renderGrid(tab), /refresh-source/);
  const count = ui.requests.length; await ui.loadPage(tab.info.path); assert.equal(ui.requests.length, count);
});

test("filtered counts stay unknown until requested and late counts cannot overwrite another view", async () => {
  const ui = frontend(), tab = dataset(); ui.useTab(tab); tab.page.count = { status: "unknown" };
  assert.match(ui.renderPagination(tab), /Count rows/); assert.match(ui.renderPagination(tab), /Not counted/);
  const counting = ui.loadCount(tab); await flush(); assert.equal(tab.page.count.status, "pending");
  const loading = ui.loadPage(tab.info.path); await flush(); const page = pageFor(tab.info); page.count = { status: "unknown" };
  ui.requests.find((r) => r.command === "fetch_page").resolve(page); await loading;
  ui.requests.find((r) => r.command === "count_rows").resolve({ sessionGeneration: 1, objectId: tab.info.object.id, count: { status: "known", value: "999" } }); await counting;
  assert.equal(tab.page.count.status, "unknown");
});

test("independent tabs keep their own rows when replies arrive out of order", async () => {
  const ui = frontend(), a = dataset("a"), b = dataset("b"); ui.useTabs([a, b], a.info.path);
  const work = ui.loadPage(a.info.path); await flush(); ui.useTabs([a, b], b.info.path);
  b.page.rows[0][1] = text("Bob"); ui.requests[0].resolve(pageFor(a.info)); await work;
  assert.equal(b.page.rows[0][1].value, "Bob"); assert.match(ui.renderGrid(b), /Bob/); assert.doesNotMatch(ui.renderGrid(b), /Alice/);
});

test("truncated nested values load exact full cells before copying", async () => {
  const ui = frontend(), tab = dataset(); ui.useTab(tab); tab.page.rows[0][2] = { ...json('{"value":'), truncated: true, byteLength: "100" };
  assert.match(ui.renderDrawer(tab), /Expand value/);
  const copy = ui.handleAction({ dataset: { action: "copy-cell", row: "0", ci: "2" } }); await flush();
  assert.equal(ui.requests[0].command, "read_row"); assert.equal(ui.copied.length, 0);
  const raw = '{"value":9007199254740993,"decimal":0.12345678901234567890}';
  ui.requests[0].resolve({ sessionGeneration: 1, objectId: tab.info.object.id, rowId: "0", columnIds: ["col-2"], cells: [json(raw)] }); await copy;
  assert.equal(ui.copied[0], raw); assert.doesNotMatch(ui.renderDrawer(tab), /Expand value/);
});

test("late expanded cells cannot copy from a closed or changed selection", async () => {
  const ui = frontend(), tab = dataset(); ui.useTab(tab); tab.page.rows[0][2] = { ...json('{"value":'), truncated: true };
  const work = ui.handleAction({ dataset: { action: "copy-cell", row: "0", ci: "2" } }); await flush(); tab.selectedRow = null;
  ui.requests[0].resolve({ sessionGeneration: 1, objectId: tab.info.object.id, rowId: "0", columnIds: ["col-2"], cells: [json('{"value":1}')] }); await work; assert.equal(ui.copied.length, 0);
});

test("copy preserves NULL, empty text, false, zero, nonfinite values and exact digits", () => {
  const ui = frontend(), tab = dataset(); ui.useTab(tab);
  tab.pageColumns = ["col-0", "col-1", "col-2"];
  assert.equal(ui.rowJson(tab, [{ kind: "null" }, text(""), { kind: "boolean", value: false }]), '{\n  "id": null,\n  "name": "",\n  "details": false\n}');
  assert.equal(ui.rowJson(tab, [number("0"), number("NaN"), number("9007199254740993")]), '{\n  "id": 0,\n  "name": "NaN",\n  "details": 9007199254740993\n}');
});

test("cancelling before reservation returns still submits and drains the cancelled handle", async () => {
  const ui = frontend({ deferBegin: true }), tab = dataset(); ui.useTab(tab);
  const old = ui.loadPage(tab.info.path); await flush(); const newer = ui.loadPage(tab.info.path); await flush();
  ui.requests[0].resolve({ id: "old-operation" }); await flush();
  const cancel = ui.calls.findIndex((c) => c.command === "cancel_operation" && c.args.operationId === "old-operation");
  const submitted = ui.calls.findIndex((c) => c.command === "fetch_page" && c.args.request.operationId === "old-operation");
  assert.ok(cancel >= 0 && submitted > cancel);
  ui.requests.find((r) => r.command === "fetch_page").reject({ code: "cancelled", message: "Cancelled" }); await old;
  ui.requests.filter((r) => r.command === "begin_operation")[1].resolve({ id: "new-operation" }); await flush();
  ui.requests.filter((r) => r.command === "fetch_page")[1].resolve(pageFor(tab.info)); await newer; assert.equal(tab.page.rows[0][0].value, "9007199254740993");
});

test("export uses a loaded session view and cannot populate another tab's export dialog", async () => {
  const ui = frontend(), a = dataset("a"), b = dataset("b"); ui.useTabs([a, b], a.info.path); ui.showExport();
  const work = ui.runExport(); await flush(); const req = ui.requests.find((r) => r.command === "export_data");
  assert.equal(req.args.request.sessionId, "session-a"); assert.equal(req.args.request.query.view.path, undefined);
  ui.useTabs([a, b], b.info.path); req.resolve({ sessionGeneration: 1, objectId: a.info.object.id, destination: "/export.parquet", exportedRows: "1", truncated: false, warnings: [] }); await work; assert.equal(ui.getLastExport(), null);
});


test("integer/decimal filter forms and validation preserve textual precision", () => {
  const ui = frontend(), info = descriptor();
  const integer = info.object.columns[0];
  assert.equal(ui.filterInputType(integer, "eq"), "text");
  assert.equal(ui.validateFilter(integer, { column: integer.id, op: "eq", value: "9007199254740993" }), null);
  assert.match(ui.validateFilter(integer, { column: integer.id, op: "eq", value: "9223372036854775808" }), /range/);
  const decimal = { ...integer, dataType: "DECIMAL(38,20)" };
  assert.equal(ui.filterInputType(decimal, "eq"), "text");
  assert.equal(ui.validateFilter(decimal, { column: decimal.id, op: "eq", value: "123456789012345678.12345678901234567890" }), null);
  assert.match(ui.validateFilter(decimal, { column: decimal.id, op: "eq", value: "123456789012345678.123456789012345678901" }), /rounding/);
  const huge = { ...integer, dataType: "BIGNUM" };
  assert.equal(ui.validateFilter(huge, { column: huge.id, op: "eq", value: "1".repeat(500) }), null);
});


function textPreview(token = "preview-1") {
  const info = descriptor("csv");
  return { token, format:"csv", options:{delimiter:",",header:true,quote:'"',escape:'"',nullMarker:"\\N",forceText:false,typeOverrides:{}}, columns:info.object.columns.slice(0,2), rows:[[text("001"),text("Alice")]],sampledRows:1,hasMore:false,warnings:["Schema is frozen after confirmation."] };
}
function textDescriptor() {
  const info=descriptor("csv");info.path="/data.csv";info.fileName="data.csv";info.format="csv";info.object.rowCount={status:"unknown"};info.delimited=textPreview().options;return info;
}

test("CSV opening previews first and sends only a confirmed backend token to open", async () => {
  const ui=frontend();const preparing=ui.openFileByPath("/data.csv");await flush();
  assert.equal(ui.requests[0].command,"preview_delimited");assert.equal(ui.requests[0].args.options.delimiter,",");assert.ok(!ui.calls.some(c=>c.command==="open_dataset"));
  ui.requests[0].resolve(textPreview());await preparing;assert.equal(ui.getDialog().valid,true);assert.match(ui.renderDelimited(ui.getDialog()),/Open with these settings/);
  const confirming=ui.confirmDelimited();await flush();assert.equal(ui.requests[1].command,"open_dataset");assert.equal(ui.requests[1].args.openOptions.previewToken,"preview-1");
  const info=textDescriptor();ui.requests[1].resolve(info);await flush();assert.equal(ui.requests[2].command,"fetch_page");assert.equal(ui.requests[2].args.request.sessionId,info.sessionId);
  const result=pageFor(info);result.count={status:"unknown"};ui.requests[2].resolve(result);await confirming;assert.equal(ui.getTabs().length,1);assert.equal(ui.getDialog(),null);assert.match(ui.renderPagination(ui.getTabs()[0]),/Count rows/);
});

test("edited parsing settings invalidate pending previews and confirmation until re-previewed", async () => {
  const ui=frontend();const opening=ui.openDelimited("/data.csv");await flush();
  ui.changeDelimited("delimiter",";");ui.requests[0].resolve(textPreview("old-token"));await opening;
  assert.equal(ui.getDialog().valid,false);assert.equal(ui.getDialog().preview,null);assert.ok(ui.calls.some(c=>c.command==="discard_preview"&&c.args.token==="old-token"));
  await ui.confirmDelimited();assert.ok(!ui.calls.some(c=>c.command==="open_dataset"));
  const updating=ui.previewDelimited();await flush();assert.equal(ui.requests[1].args.options.delimiter,";");ui.requests[1].resolve(textPreview("new-token"));await updating;assert.equal(ui.getDialog().valid,true);
  ui.changeDelimited("forceText","yes");ui.changeDelimited("","DECIMAL(38,20)","col-0");assert.equal(ui.getDialog().valid,false);
  const third=ui.previewDelimited();await flush();assert.equal(ui.requests[2].args.options.forceText,true);assert.equal(ui.requests[2].args.options.typeOverrides["col-0"],"DECIMAL(38,20)");ui.requests[2].reject({message:"Would round input",code:"invalid_input"});await third;assert.equal(ui.getDialog().error,"Would round input");
});

test("TSV defaults to tab and closing a preview discards late results", async () => {
  const ui=frontend();const opening=ui.openDelimited("/data.tsv");await flush();assert.equal(ui.requests[0].args.options.delimiter,"\t");
  ui.cancelDelimited();ui.requests[0].resolve(textPreview("late"));await opening;assert.equal(ui.getDialog(),null);assert.equal(ui.getTabs().length,0);assert.ok(ui.calls.some(c=>c.command==="discard_preview"&&c.args.token==="late"));
});

test("closing parsing while open is pending closes the unused new session", async () => {
  const ui=frontend();const preparing=ui.openDelimited("/data.csv");await flush();ui.requests[0].resolve(textPreview());await preparing;
  const opening=ui.confirmDelimited();await flush();ui.cancelDelimited();const info=textDescriptor();ui.requests[1].resolve(info);await opening;assert.equal(ui.getTabs().length,0);assert.ok(ui.calls.some(c=>c.command==="close_dataset"&&c.args.sessionId===info.sessionId));
});

test("reopening parsing replaces a tab only after successful confirmation", async () => {
  const ui=frontend(),old=dataset();old.info=textDescriptor();old.info.sessionId="old-session";old.info.delimited.typeOverrides={"col-0":"VARCHAR"};ui.useTab(old);
  const preparing=ui.openDelimited(old.info.path);await flush();assert.equal(ui.requests[0].args.options.typeOverrides["col-0"],"VARCHAR");ui.requests[0].resolve(textPreview());await preparing;assert.equal(ui.getTabs()[0],old);
  const opening=ui.confirmDelimited();await flush();const fresh=textDescriptor();fresh.sessionId="new-session";ui.requests[1].resolve(fresh);await flush();assert.equal(old.closed,true);assert.equal(ui.getTabs()[0].info.sessionId,"new-session");ui.requests[2].resolve(pageFor(fresh));await opening;
  assert.ok(ui.calls.some(c=>c.command==="close_dataset"&&c.args.sessionId==="old-session"));
});

test("duplicate and blank labels use independent IDs for copied JSON and preview HTML is escaped", () => {
  const ui=frontend(),tab=dataset();ui.useTab(tab);tab.info.object.columns[0].name="same";tab.info.object.columns[1].name="same";tab.info.object.columns[2].name="";
  const output=JSON.parse(ui.rowJson(tab,tab.page.rows[0]));assert.equal(output["col-1"],"Alice");assert.ok("col-0" in output);assert.ok("col-2" in output);
  const preview=textPreview();preview.columns[0].name='<script>bad</script>';preview.rows[0][0]=text('<img src=x>');
  const markup=ui.renderDelimited({path:"/x.csv",options:{},preview,valid:true,busy:false,error:null,epoch:1});assert.doesNotMatch(markup,/<script>|<img /);assert.match(markup,/&lt;script&gt;/);assert.match(markup,/&lt;img/);
});

test("all five export choices are available for CSV tabs and output names strip input extensions", () => {
  const ui=frontend(),tab=dataset();tab.info=textDescriptor();ui.useTab(tab);
  for (const format of ["parquet","csv","tsv","json","jsonl"]) { ui.showExport(format);assert.match(ui.renderExportModal(tab),new RegExp(`value="${format}"`));assert.match(ui.defaultExportFileName("source.csv",format),new RegExp(`^source-export-.*[.]${format}$`)); }
  ui.showExport("jsonl");assert.match(ui.renderExportModal(tab),/one JSON object per line/);
});


test("editing settings during pending confirmation cannot install the old configuration", async () => {
  const ui=frontend();const preparing=ui.openDelimited("/data.csv");await flush();ui.requests[0].resolve(textPreview());await preparing;
  const opening=ui.confirmDelimited();await flush();ui.changeDelimited("forceText","yes");const info=textDescriptor();ui.requests[1].resolve(info);await opening;
  assert.equal(ui.getTabs().length,0);assert.equal(ui.getDialog().valid,false);assert.equal(ui.getDialog().busy,false);assert.ok(ui.calls.some(c=>c.command==="close_dataset"&&c.args.sessionId===info.sessionId));
});


function documentInfo() {
  const info = descriptor("doc"); info.format = "json"; info.path = "/doc.json"; info.fileName = "doc.json";
  info.object = { ...info.object, kind: "document", columns: [], rowCount: { status: "unknown" }, capabilities: { page: false, filter: false, sort: false, profile: false, exactCount: false, copy: true, exports: [] } };
  info.document = { rootKind: "object", childCount: "2", reason: "The root is an object; it is shown as a document, not a table." };
  info.policy.jsonChildren = 100; return info;
}
const docNode = (index, key, kind, value, childCount = null) => ({ index, key, keyTruncated: false, kind, value, byteLength: String(value?.length ?? 0), truncated: false, childCount });
function docResult(info, path, children, extra = {}) {
  return { sessionGeneration: info.generation, objectId: info.object.id, path, node: docNode(path.at(-1) ?? 0, null, "object", null, String(children.length)), children, offset: 0, hasMore: false, text: null, ...extra };
}

test("JSON documents open without a parsing dialog and render exact tokens from bounded backend pages", async () => {
  const ui = frontend(), info = documentInfo(), wide = "12345678901234567890123456789012345678901234567890.000000000000000000001";
  const opening = ui.openFileByPath("/doc.json"); await flush();
  assert.equal(ui.requests[0].command, "open_dataset"); assert.deepEqual(JSON.parse(JSON.stringify(ui.requests[0].args.openOptions)), {});
  ui.requests[0].resolve(info); await flush();
  const root = ui.requests[1]; assert.equal(root.command, "read_document");
  assert.deepEqual(JSON.parse(JSON.stringify(root.args.request.query)), { path: [], offset: 0, limit: 100, mode: "children" });
  root.resolve(docResult(info, [], [docNode(0, "n", "number", wide), docNode(1, "<list>", "array", null, "250")], { hasMore: false })); await opening;
  const tab = ui.getTabs()[0], markup = ui.renderMain(tab);
  assert.ok(markup.includes(wide)); assert.match(markup, /&quot;&lt;list&gt;&quot;/); assert.match(markup, /Array · 250 items/); assert.doesNotMatch(markup, /fetch_page|th-name/);
  assert.ok(!ui.calls.some((c) => c.command === "fetch_page"));

  const expand = ui.handleAction({ dataset: { action: "doc-toggle", node: "1" } }); await flush();
  const first = ui.requests[2]; assert.deepEqual([...first.args.request.query.path], [1]);
  first.resolve(docResult(info, [1], Array.from({ length: 100 }, (_, i) => docNode(i, null, "number", String(i))), { hasMore: true, node: docNode(1, "<list>", "array", null, "250") })); await expand;
  assert.match(ui.renderMain(tab), /Show more \(100 of 250 shown\)/);
  const more = ui.handleAction({ dataset: { action: "doc-more", node: "1" } }); await flush();
  assert.equal(ui.requests[3].args.request.query.offset, 100);
  ui.requests[3].resolve(docResult(info, [1], [docNode(100, null, "number", "100")], { offset: 100 })); await more;
  assert.equal(tab.doc.branches.get("1").children.length, 101);

  const copy = ui.handleAction({ dataset: { action: "doc-copy", node: "0" } }); await flush();
  assert.equal(ui.requests[4].args.request.query.mode, "copy");
  ui.requests[4].resolve(docResult(info, [0], [], { text: wide })); await copy; assert.equal(ui.copied[0], wide);
  const collapse = ui.handleAction({ dataset: { action: "doc-toggle", node: "1" } }); await collapse;
  assert.doesNotMatch(ui.renderMain(tab), /Show more/);
});

test("late document pages for another node, generation or closed tab are ignored", async () => {
  const ui = frontend(), info = documentInfo(), tab = dataset("doc"); tab.info = info; tab.doc = null; ui.useTab(tab);
  const loading = ui.loadDocument(tab, ""); await flush();
  ui.requests[0].resolve(docResult(info, [0], [docNode(0, "x", "number", "1")])); await loading;
  assert.match(tab.doc.branches.get("").error, /does not match/);
  const again = ui.loadDocument(tab, ""); await flush();
  await ui.handleAction({ dataset: { action: "close-tab", path: info.path } });
  ui.requests[1].resolve(docResult(info, [], [docNode(0, "x", "number", "1")])); await again;
  assert.equal(tab.closed, true); assert.equal(tab.doc, null);
});

test("a JSON/JSON Lines mismatch asks before an explicit reopen", async () => {
  const ui = frontend(); const opening = ui.openFileByPath("/lines.json"); await flush();
  ui.requests[0].reject({ code: "format_mismatch", message: "This .json file contains more than one top-level JSON value." }); await opening;
  assert.equal(ui.getTabs().length, 0); assert.equal(ui.getFormatPrompt().format, "jsonl"); assert.match(ui.renderFormatPrompt(), /Reopen as JSON Lines/);
  const reopening = ui.handleAction({ dataset: { action: "reopen-format" } }); await flush();
  assert.equal(ui.requests[1].args.openOptions.format, "jsonl"); assert.equal(ui.getFormatPrompt(), null);
  ui.requests[1].reject({ code: "format_mismatch", message: "still wrong" }); await reopening;
  assert.equal(ui.getFormatPrompt(), null, "an explicit reopen is never re-offered in a loop");
});

test("JSON record tables disable raw-token sorting, explain profiles and load exact source records", async () => {
  const ui = frontend(), tab = dataset(); tab.info.format = "jsonl";
  tab.info.object.columns[2] = { ...tab.info.object.columns[2], dataType: "JSON_NUMBER", kind: "number", sortable: false, operators: ["eq", "ne", "is_null", "is_not_null"] };
  tab.page.rows[0][2] = { kind: "number", value: "1e+400", logicalType: "JSON_NUMBER", byteLength: "6", truncated: false };
  ui.useTab(tab);
  const grid = ui.renderGrid(tab); assert.doesNotMatch(grid, /data-action="sort-col" data-col="col-2"/); assert.match(grid, /cannot be sorted/);
  assert.equal(ui.validateFilter(tab.info.object.columns[2], { column: "col-2", op: "eq", value: "1E400" }), null);
  assert.match(ui.validateFilter(tab.info.object.columns[2], { column: "col-2", op: "eq", value: "01" }), /JSON number token/);
  assert.equal(ui.rowJson(tab, tab.page.rows[0]).includes('"details": 1e+400'), true);
  assert.match(ui.renderDrawer(tab), /Show source record/);
  const raw = `{"id":9007199254740993,"details":1e400,"missing":null}`;
  const loading = ui.handleAction({ dataset: { action: "load-raw-record" } }); await flush();
  const req = ui.requests[0]; assert.equal(req.command, "read_row"); assert.equal(req.args.request.query.rawRecord, true);
  req.resolve({ sessionGeneration: 1, objectId: tab.info.object.id, rowId: "0", columnIds: ["col-0"], cells: [number("1")], rawRecord: raw }); await loading;
  assert.ok(ui.renderDrawer(tab).includes("9007199254740993")); assert.ok(ui.renderDrawer(tab).includes("1e400"));
  await ui.handleAction({ dataset: { action: "copy-raw-record" } }); assert.equal(ui.copied[0], raw);
  tab.selectedRow = null; tab.profileColumn = "col-2"; tab.profile = { ...profileFor(tab.info, "col-2"), min: { kind: "null" }, max: { kind: "null" }, warnings: ["Min/max are unavailable for raw JSON values."] };
  assert.match(ui.renderDrawer(tab), /Min\/max are unavailable/);
  assert.match(ui.defaultExportFileName("records.ndjson", "json"), /^records-export-.*[.]json$/);
});


function workbook() {
  return { variant: "xlsx", date1904: false, warnings: ["Formulas are never recalculated and macros are never run; Sivro reads only cached cell values."], sheets: [
    { name: "Chart", kind: "chartsheet", visibility: "visible", selectable: false },
    { name: "<Secret>", kind: "worksheet", visibility: "hidden", selectable: true },
    { name: "Data", kind: "worksheet", visibility: "visible", selectable: true },
  ] };
}
function excelDescriptor(sheet = "Data", headerRow = 1) {
  const info = descriptor("xl"); info.format = "excel"; info.path = "/book.xlsx"; info.fileName = "book.xlsx"; info.object.label = sheet;
  info.excel = { sheet, headerRow, workbook: workbook() }; return info;
}

test("Excel opens through a sheet/header dialog and sends only the chosen selection", async () => {
  const ui = frontend(); const opening = ui.openFileByPath("/book.xlsx"); await flush();
  assert.equal(ui.requests[0].command, "inspect_workbook"); assert.ok(!ui.calls.some((c) => c.command === "open_dataset"));
  ui.requests[0].resolve(workbook()); await opening;
  const dialog = ui.getExcelDialog(); assert.equal(dialog.sheet, "Data", "the first visible worksheet is preselected");
  const markup = ui.renderExcel(dialog);
  assert.match(markup, /chartsheet — no cell table/); assert.match(markup, /value="Chart"[^>]*disabled/); assert.match(markup, /&lt;Secret&gt;/); assert.match(markup, /· hidden/);
  ui.changeExcel("sheet", "<Secret>"); ui.changeExcel("headerRow", "0");
  await ui.confirmExcel(); assert.ok(!ui.calls.some((c) => c.command === "open_dataset"), "an invalid header row cannot be submitted");
  assert.match(ui.renderExcel(dialog), /header&#39;s row number/);
  ui.changeExcel("headerRow", "3");
  const confirming = ui.confirmExcel(); await flush();
  const open = ui.requests[1]; assert.equal(open.command, "open_dataset");
  assert.deepEqual(JSON.parse(JSON.stringify(open.args.openOptions)), { excel: { sheet: "<Secret>", headerRow: 3 } });
  const info = excelDescriptor("<Secret>", 3); open.resolve(info); await flush();
  ui.requests[2].resolve(pageFor(info)); await confirming;
  assert.equal(ui.getExcelDialog(), null); const tab = ui.getTabs()[0];
  assert.match(ui.renderMain(tab), /Sheet: &lt;Secret&gt;/); assert.match(ui.renderMain(tab), /&lt;Secret&gt; · 1 rows/);
  assert.match(ui.defaultExportFileName("book.xlsx", "csv"), /^book-export-.*[.]csv$/);
});

test("changing sheets replaces the tab only after success and late workbook replies are ignored", async () => {
  const ui = frontend(), old = dataset("xl"); old.info = excelDescriptor("Data", null); old.info.sessionId = "old-session"; ui.useTab(old);
  const reopening = ui.handleAction({ dataset: { action: "excel-sheets" } }); await flush();
  ui.requests[0].resolve(workbook()); await reopening;
  const dialog = ui.getExcelDialog(); assert.equal(dialog.sheet, "Data"); assert.equal(dialog.header, false, "the frozen no-header choice is prefilled");
  ui.changeExcel("header", "yes"); ui.changeExcel("headerRow", "2");
  const confirming = ui.confirmExcel(); await flush();
  assert.equal(ui.requests[1].args.openOptions.excel.headerRow, 2); assert.equal(ui.getTabs()[0], old);
  const fresh = excelDescriptor("Data", 2); fresh.sessionId = "new-session"; ui.requests[1].resolve(fresh); await flush();
  assert.equal(old.closed, true); assert.ok(ui.calls.some((c) => c.command === "close_dataset" && c.args.sessionId === "old-session"));
  ui.requests[2].resolve(pageFor(fresh)); await confirming;

  const late = ui.openExcel("/other.xlsx"); await flush(); ui.cancelExcel();
  ui.requests[3].resolve(workbook()); await late; assert.equal(ui.getExcelDialog(), null);
  const failed = ui.openExcel("/bad.xlsx"); await flush();
  ui.requests[4].reject({ code: "resource_limit", message: "The used range reaches XFD1048576" }); await failed;
  assert.match(ui.renderExcel(ui.getExcelDialog()), /XFD1048576/);
  const pending = ui.openExcel("/book.xlsx"); await flush(); ui.requests[5].resolve(workbook()); await pending;
  const opening = ui.confirmExcel(); await flush(); ui.cancelExcel();
  const unused = excelDescriptor(); unused.sessionId = "unused"; ui.requests[6].resolve(unused); await opening;
  assert.ok(ui.calls.some((c) => c.command === "close_dataset" && c.args.sessionId === "unused"));
});


function sqliteDatabase() {
  return { sqliteVersion: "3.53.2", journalMode: "wal", warnings: ["Read-only: Sivro never writes."], objects: [
    { name: "docs", kind: "virtual", columns: 1, withoutRowid: false, selectable: false, reason: "Virtual tables run extension modules; Sivro does not open them." },
    { name: 'odd "name" <✓>', kind: "table", columns: 3, withoutRowid: false, selectable: true, reason: null },
    { name: "pairs", kind: "table", columns: 3, withoutRowid: true, selectable: true, reason: null },
  ] };
}
function sqliteDescriptor(object = "pairs") {
  const info = descriptor("db"); info.format = "sqlite"; info.path = "/data.db"; info.fileName = "data.db"; info.object.label = object;
  info.object.rowCount = { status: "unknown" }; info.sqlite = { object, database: sqliteDatabase() }; return info;
}

test("SQLite opens through a table/view picker and sends only the chosen object", async () => {
  const ui = frontend(); const opening = ui.openFileByPath("/data.db"); await flush();
  assert.equal(ui.requests[0].command, "inspect_database"); assert.ok(!ui.calls.some((c) => c.command === "open_dataset"));
  ui.requests[0].resolve(sqliteDatabase()); await opening;
  const dialog = ui.getSqliteDialog(); assert.equal(dialog.object, 'odd "name" <✓>', "the first selectable object is preselected");
  const markup = ui.renderSqlite(dialog);
  assert.match(markup, /value="docs"[^>]*disabled/); assert.match(markup, /extension modules/); assert.match(markup, /without rowid/);
  assert.match(markup, /odd &quot;name&quot; &lt;✓&gt;/); assert.doesNotMatch(markup, /<✓>/);
  ui.changeSqlite("pairs");
  const confirming = ui.confirmSqlite(); await flush();
  const open = ui.requests[1]; assert.equal(open.command, "open_dataset");
  assert.deepEqual(JSON.parse(JSON.stringify(open.args.openOptions)), { sqlite: { object: "pairs" } });
  const info = sqliteDescriptor(); open.resolve(info); await flush(); ui.requests[2].resolve(pageFor(info)); await confirming;
  const tab = ui.getTabs()[0]; assert.match(ui.renderMain(tab), /Table: pairs/);
  assert.match(ui.defaultExportFileName("data.sqlite3", "json"), /^data-export-.*[.]json$/);
  const late = ui.openSqlite("/data.db"); await flush(); ui.cancelSqlite(); ui.requests[3].resolve(sqliteDatabase()); await late;
  assert.equal(ui.getSqliteDialog(), null);
});

test("SQLite number filters accept 64-bit integers and finite reals whatever the declaration", () => {
  const ui = frontend(), column = { id: "c", name: "big", kind: "number", dataType: "INTEGER", operators: ["eq"] };
  assert.equal(ui.validateFilter(column, { column: "c", op: "eq", value: "9007199254740993" }, true), null);
  assert.equal(ui.validateFilter(column, { column: "c", op: "eq", value: "1.5e3" }, true), null);
  assert.match(ui.validateFilter(column, { column: "c", op: "eq", value: "9223372036854775808" }, true), /64 bits/);
  assert.match(ui.validateFilter(column, { column: "c", op: "eq", value: "9007199254740993" }), /range/, "DuckDB INTEGER stays 32-bit");
});


test("slow opens show a pending entry and notes and disabled exports stay visible", async () => {
  const ui = frontend(); const opening = ui.openFileByPath("/big.jsonl"); await flush();
  assert.match(ui.renderSidebar(null), /Opening big\.jsonl…/);
  const info = descriptor("big"); info.path = "/big.jsonl"; info.fileName = "big.jsonl"; info.format = "jsonl";
  info.object.warnings = ["All 3 records were read when opening.", "Some numbers use exponents."];
  ui.requests[0].resolve(info); await flush(); ui.requests[1].resolve(pageFor(info)); await opening;
  assert.doesNotMatch(ui.renderSidebar(ui.getTabs()[0]), /Opening/);
  assert.match(ui.renderMain(ui.getTabs()[0]), /data-action="open-file-info"[^>]*>.*2 notes/s);
  const doc = dataset("doc"); doc.info = documentInfo(); doc.info.object.warnings = ["Numbers are exact source tokens."]; doc.doc = null; ui.useTab(doc);
  const markup = ui.renderMain(doc);
  assert.match(markup, /btn-export" disabled title="JSON documents are inspected/); assert.match(markup, /1 note</);
});

test("opening About reads local metadata without checking or downloading updates", async () => {
  const ui = frontend();
  assert.equal(ui.calls.length, 0);
  assert.match(ui.renderTitlebar(), /data-action="open-about"/);
  await ui.openAbout();
  assert.equal(ui.getAboutOpen(), true);
  assert.equal(ui.getAppMetadata().version, "0.1.1");
  assert.deepEqual(ui.calls.map(c => c.command), ["app_metadata"]);
  ui.closeAbout(); await ui.openAbout();
  assert.deepEqual(ui.calls.map(c => c.command), ["app_metadata"]);
  await ui.handleAction({ dataset: { action: "check-updates" } });
  assert.deepEqual(ui.calls.map(c => c.command), ["app_metadata", "check-update"]);
  assert.equal(ui.updates.state.phase, "current");
});

test("unsupported app packages cannot initiate installation", async () => {
  const ui = frontend({ updateSupported: false, update: { version: "0.2.0" } });
  await ui.openAbout(); await ui.handleAction({ dataset: { action: "check-updates" } });
  await ui.handleAction({ dataset: { action: "confirm-update" } });
  await ui.handleAction({ dataset: { action: "install-update" } });
  assert.equal(ui.updates.state.phase, "available");
  assert.deepEqual(ui.calls.map(c => c.command), ["app_metadata", "check-update"]);
});

test("startup is idempotent even when the DOM is already interactive", () => {
  const ui = frontend({ readyState: "interactive" });
  ui.init(); ui.init();
  assert.equal(ui.calls.filter(c => c.command === "register-close").length, 1);
});

test("simultaneous picker requests open one dialog, and cancellation allows reopening", async () => {
  let finish;
  const ui = frontend({ openDialog: () => new Promise(resolve => { finish = resolve; }) });
  const first = ui.chooseAndOpenFile(); await ui.chooseAndOpenFile();
  assert.equal(ui.calls.filter(c => c.command === "file-dialog").length, 1);
  finish(null); await first;
  const second = ui.chooseAndOpenFile(); assert.equal(ui.calls.filter(c => c.command === "file-dialog").length, 2);
  finish(null); await second;
});

test("picker errors do not leave future open requests locked", async () => {
  const ui = frontend({ openDialog: () => { throw new Error("portal failed"); } });
  await ui.chooseAndOpenFile(); await ui.chooseAndOpenFile();
  assert.equal(ui.calls.filter(c => c.command === "file-dialog").length, 2);
});

test("pending view changes have a Sublime-style dot until the loaded view catches up", () => {
  const ui = frontend(), tab = dataset(); ui.useTab(tab);
  assert.equal(ui.hasPendingView(tab), false);
  tab.quickSearch = "Alice";
  assert.equal(ui.hasPendingView(tab), true); assert.match(ui.renderTabStrip(), /tab-close has-pending/);
  tab.pageQuery = ui.queryFor(tab); assert.equal(ui.hasPendingView(tab), false);
  tab.filters.push({ column: "col-0", op: "eq", value: "1" }); assert.equal(ui.hasPendingView(tab), true);
  tab.filters = []; assert.equal(ui.hasPendingView(tab), false);
  tab.stale = true; assert.match(ui.renderTabStrip(), /Source changed on disk/); assert.match(ui.renderTabStrip(), /tab-source-warning/);
});
