import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open, save } from "@tauri-apps/plugin-dialog";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { icon } from "./icons";
import { cellText, cellForCopy, cellJson, clipped, isTruncated, utf8Bytes } from "./cells";
import { datasets, DatasetOperation, startOperation, errorInfo, matches } from "./services/datasets";
import type { OperationKind, OperationInputs, OperationOutputs } from "./services/datasets";
import { defaultFilterOp, filterInputType, isComparison, isIntegerColumn, validateFilter } from "./filters";
import { populateJsonNode, structuredCellHtml } from "./structured";
import {
  ColumnInfo,
  ColumnProfile,
  DatasetInfo,
  ExportFormat,
  ExportResult,
  Filter,
  FilterOp,
  PageResult,
  QueryArgs,
  Cell,
  CountState,
  CountResult,
  FilterValue,
  FormatKind,
} from "./types";
import { renderDelimited } from "./delimited";
import type { DelimitedDialog } from "./delimited";
import { parsePath, pathKey, renderDocument } from "./document";
import { headerRowError, renderExcel } from "./excel";
import { renderSqlite } from "./sqlite";
import type { SqliteDialog } from "./sqlite";
import type { ExcelDialog } from "./excel";
import type { DocumentBranch, DocumentState } from "./document";
// Fonts ship with the app (OFL-1.1); nothing is fetched at runtime.
import "@fontsource/inter/latin-400.css";
import "@fontsource/inter/latin-500.css";
import "@fontsource/inter/latin-600.css";
import "@fontsource/inter/latin-700.css";
import "@fontsource/inter/latin-800.css";
import "@fontsource/jetbrains-mono/latin-400.css";
import "@fontsource/jetbrains-mono/latin-500.css";
import "@fontsource/jetbrains-mono/latin-600.css";
import "./styles.css";

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

interface DatasetTab {
  info: DatasetInfo;
  visibleColumns: string[];
  page: PageResult | null;
  // The column order used by the loaded page, independent of pending changes.
  pageColumns: string[];
  currentPage: number;
  pageSize: number;
  sortColumn: string | null;
  sortDesc: boolean;
  filters: Filter[];
  quickSearch: string;
  loading: boolean;
  error: string | null;
  requestId: number;
  queryMs: number | null;
  selectedRow: number | null;
  profileColumn: string | null;
  profile: ColumnProfile | null;
  profileLoading: boolean;
  profileError: string | null;
  profileCache: Map<string, ColumnProfile | null>;
  profileRequests: Map<string, Promise<ColumnProfile>>;
  operations: Set<DatasetOperation<unknown>>;
  pageOperation: DatasetOperation<PageResult> | null;
  countOperation: DatasetOperation<CountResult> | null;
  pageQuery: QueryArgs | null;
  profileRequestId: number;
  stale: boolean;
  closed: boolean;
  refreshing: boolean;
  // JSON documents: lazily fetched branches. Tables: one inspected source record.
  doc: DocumentState | null;
  rawRecord: { row: number; text: string | null; loading: boolean } | null;
}

interface Toast {
  id: number;
  kind: "success" | "error" | "info";
  message: string;
}

const PAGE_SIZES = [50, 100, 200, 500];
const DEFAULT_PAGE_SIZE = 100;

const FILTER_OPS: { value: FilterOp; label: string; needsValue: boolean; multi?: boolean }[] = [
  { value: "contains", label: "contains", needsValue: true },
  { value: "contains_any", label: "contains any", needsValue: true, multi: true },
  { value: "not_contains", label: "does not contain", needsValue: true },
  { value: "not_contains_any", label: "does not contain any", needsValue: true, multi: true },
  { value: "eq", label: "=  equals", needsValue: true },
  { value: "ne", label: "≠  not equals", needsValue: true },
  { value: "gt", label: ">  greater than", needsValue: true },
  { value: "lt", label: "<  less than", needsValue: true },
  { value: "gte", label: "≥  at least", needsValue: true },
  { value: "lte", label: "≤  at most", needsValue: true },
  { value: "starts_with", label: "starts with", needsValue: true },
  { value: "ends_with", label: "ends with", needsValue: true },
  { value: "is_null", label: "is empty (NULL)", needsValue: false },
  { value: "is_not_null", label: "is not empty", needsValue: false },
];

let delimitedDialog: DelimitedDialog | null = null;
let excelDialog: ExcelDialog | null = null;
let sqliteDialog: SqliteDialog | null = null;
let sqliteEpoch = 0;
let excelEpoch = 0;
// A JSON <-> JSON Lines mismatch is offered as an explicit reopen, never applied silently.
let formatPrompt: { path: string; format: FormatKind; message: string } | null = null;
let delimitedEpoch = 0;
let tabs: DatasetTab[] = [];
let activePath: string | null = null;
let recents: string[] = [];
let toasts: Toast[] = [];
let toastSeq = 0;

let filterPanelOpen = false;
let exportModalOpen = false;
let exportMaxRows = 100000;
let exportFormat: ExportFormat = "parquet";
let exportBusy = false;
let lastExport: ExportResult | null = null;
let fileInfoOpen = false;
let columnSearch = "";
let newFilter: Filter = { column: "", op: "contains", value: "" };
let filterError = "";
let filterValueChosen = false;
// Last known grid scroll, preserved across re-renders (e.g. sorting, paging)
// so horizontal position doesn't jump back to 0.
let lastGridScroll = { top: 0, left: 0 };

const isMac = navigator.userAgent.toLowerCase().includes("mac");

try {
  recents = JSON.parse(localStorage.getItem("sivro-recents") || "[]");
  if (!Array.isArray(recents)) recents = [];
} catch {
  recents = [];
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function escapeHtml(value: string): string {
  return value
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

function formatBytes(size: number | string): string {
  const bytes = Number(size);
  if (!Number.isFinite(bytes) || bytes < 0) return "—";
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = bytes / 1024;
  let u = 0;
  while (v >= 1024 && u < units.length - 1) {
    v /= 1024;
    u++;
  }
  return `${v.toFixed(v >= 100 ? 0 : 1)} ${units[u]}`;
}

function formatNumber(n: number | string): string {
  return (typeof n === "string" ? BigInt(n) : n).toLocaleString("en-US");
}
function formatCount(count: CountState): string {
  return count.status === "known" ? formatNumber(count.value) : count.status === "pending" ? "Counting…" : count.status === "failed" ? "Count unavailable" : "Not counted";
}
function safeCount(count: CountState): number | null {
  return count.status === "known" && BigInt(count.value) <= BigInt(Number.MAX_SAFE_INTEGER) ? Number(count.value) : null;
}
function columnLabel(tab: DatasetTab, id: string): string { return columnById(tab, id)?.name ?? id; }
function current(tab: DatasetTab, info: DatasetInfo): boolean { return !tab.closed && tabs.includes(tab) && tab.info === info; }
function task<K extends OperationKind>(tab: DatasetTab, kind: K, query: OperationInputs[K]): DatasetOperation<OperationOutputs[K]> {
  const operation = startOperation(tab.info, kind, query, () => { if (!tab.closed && tabs.includes(tab)) render(); });
  tab.operations.add(operation);
  const finished = () => { tab.operations.delete(operation); };
  void operation.result.then(finished, finished);
  return operation;
}
function invalidate(tab: DatasetTab): void {
  ++tab.requestId; ++tab.profileRequestId;
  for (const operation of tab.operations) void operation.cancel();
  tab.page = null; tab.pageColumns = []; tab.pageQuery = null; tab.selectedRow = null;
  tab.profile = null; tab.profileColumn = null; tab.profileLoading = false;
  tab.profileCache.clear(); tab.profileRequests.clear();
  tab.doc = null; tab.rawRecord = null;
}
function isDocument(tab: DatasetTab): boolean { return tab.info.object.kind !== "table"; }
function tabMeta(tab: DatasetTab): string {
  const doc = tab.info.document;
  if (tab.info.sqlite) return `${tab.info.sqlite.object} · ${formatCount(tab.info.object.rowCount)} rows · ${tab.info.object.columns.length} cols`;
  if (tab.info.excel) return `${tab.info.excel.sheet} · ${formatCount(tab.info.object.rowCount)} rows · ${tab.info.object.columns.length} cols`;
  if (doc) return `JSON ${doc.rootKind} · ${formatNumber(doc.childCount)} ${doc.rootKind === "array" ? "items" : doc.rootKind === "object" ? "fields" : "value"}`;
  return `${formatCount(tab.info.object.rowCount)} rows · ${tab.info.object.columns.length} cols`;
}
function failTab(tab: DatasetTab, error: unknown): string {
  const parsed = errorInfo(error);
  if (["stale_dataset", "invalid_generation", "invalid_session"].includes(parsed.code)) {
    invalidate(tab); tab.stale = true; tab.loading = false; tab.error = parsed.message;
  }
  return parsed.message;
}
function queryFor(tab: DatasetTab): QueryArgs {
  return {
    columnIds: [...tab.visibleColumns], offset: (tab.currentPage - 1) * tab.pageSize,
    pageSize: tab.pageSize, quickSearch: tab.quickSearch,
    sort: tab.sortColumn ? { columnId: tab.sortColumn, descending: tab.sortDesc } : null,
    filter: { kind: "all", terms: tab.filters.map((f) => {
      const col = columnById(tab, f.column);
      let value: FilterValue;
      if (["is_null", "is_not_null"].includes(f.op)) value = { kind: "none" };
      else if (!isComparison(f.op)) value = { kind: "text", value: f.value };
      else if (col?.kind === "number") value = { kind: "number", value: f.value };
      else if (col?.kind === "bool") value = { kind: "boolean", value: f.value.toLowerCase() === "true" };
      else if (col?.kind === "date") value = { kind: "temporal", value: f.value };
      else if (col?.kind === "nested") value = { kind: "json", value: f.value };
      else value = { kind: "text", value: f.value };
      return { kind: "predicate", columnId: f.column, op: f.op, value };
    }) },
  };
}

function debounce(fn: () => void, ms: number): () => void {
  let t: ReturnType<typeof setTimeout> | null = null;
  return () => {
    if (t) clearTimeout(t);
    t = setTimeout(fn, ms);
  };
}

function showToast(message: string, kind: Toast["kind"] = "success"): void {
  const id = ++toastSeq;
  toasts.push({ id, kind, message });
  if (toasts.length > 4) toasts = toasts.slice(-4);
  renderToasts();
  setTimeout(() => {
    toasts = toasts.filter((t) => t.id !== id);
    renderToasts();
  }, 4500);
}

async function copyText(value: string, label = "Copied to clipboard"): Promise<void> {
  try {
    if (utf8Bytes(value) > (activeTab()?.info.policy.fullCellBytes ?? 1048576)) throw new Error("Copy exceeds the 1 MiB limit. Use export for full data.");
    await writeText(value);
    showToast(label);
  } catch (err) {
    showToast(`Copy failed: ${err}`, "error");
  }
}

function getTab(path: string | null): DatasetTab | null {
  if (!path) return null;
  return tabs.find((t) => t.info.path === path) || null;
}

function activeTab(): DatasetTab | null {
  return getTab(activePath);
}

function columnById(tab: DatasetTab, name: string): ColumnInfo | undefined {
  return tab.info.object.columns.find((c) => c.id === name);
}

function persistRecents(): void {
  try {
    localStorage.setItem("sivro-recents", JSON.stringify(recents.slice(0, 12)));
  } catch {
    /* storage unavailable */
  }
}

function rememberRecent(path: string): void {
  recents = [path, ...recents.filter((p) => p !== path)].slice(0, 12);
  persistRecents();
}

function baseName(path: string): string {
  const parts = path.split(/[/\\]/);
  return parts[parts.length - 1] || path;
}

function opLabel(op: string): string {
  return FILTER_OPS.find((o) => o.value === op)?.label || op;
}

function opNeedsValue(op: string): boolean {
  return FILTER_OPS.find((o) => o.value === op)?.needsValue ?? true;
}

function opIsMulti(op: string): boolean {
  return FILTER_OPS.find((o) => o.value === op)?.multi ?? false;
}

function splitMultiInput(value: string): string[] {
  return value
    .split(",")
    .map((s) => s.trim())
    .filter((s) => s.length > 0);
}

function hasFilterValue(op: string, value: string): boolean {
  if (!opNeedsValue(op)) return true;
  if (opIsMulti(op)) return splitMultiInput(value).length > 0;
  return value.trim().length > 0;
}

function availableFilterOps(column?: ColumnInfo): typeof FILTER_OPS {
  return column ? FILTER_OPS.filter((op) => column.operators.includes(op.value)) : [];
}

function setFilterError(message: string): void {
  filterError = message;
  const error = document.getElementById("nf-error");
  if (error) error.textContent = message;
  const input = document.getElementById("nf-value") as HTMLInputElement | HTMLSelectElement | null;
  input?.setAttribute("aria-invalid", message ? "true" : "false");
  input?.setCustomValidity(message);
}

function rowJson(tab: DatasetTab, row: Cell[]): string {
  const labels = tab.info.object.columns.map(c => c.name.toLowerCase());
  const ambiguous = labels.includes("") || new Set(labels).size !== labels.length;
  return `{\n${tab.pageColumns.map((id, i) => `  ${JSON.stringify(ambiguous ? id : columnLabel(tab, id))}: ${cellJson(row[i])}`).join(",\n")}\n}`;
}
function profileKey(tab: DatasetTab, column: string): string {
  const context = tab.pageQuery ?? queryFor(tab);
  return JSON.stringify([tab.info.generation, tab.info.object.id, column, context.filter, context.quickSearch]);
}
async function getColumnProfile(tab: DatasetTab, column: string): Promise<ColumnProfile> {
  const key = profileKey(tab, column), cached = tab.profileCache.get(key);
  if (cached) return cached;
  const pending = tab.profileRequests.get(key);
  if (pending) return pending;
  const info = tab.info;
  const request = task(tab, "profile", { columnId: column, context: tab.pageQuery ?? queryFor(tab) }).result;
  tab.profileRequests.set(key, request);
  try {
    const profile = await request;
    if (!matches(profile, info) || profile.columnId !== column) throw new Error("The column response belongs to a different schema.");
    if (current(tab, info) && profileKey(tab, column) === key) {
      if (tab.profileCache.size >= info.policy.maxCacheEntries) tab.profileCache.clear();
      tab.profileCache.set(key, profile);
    }
    return profile;
  } finally { if (tab.profileRequests.get(key) === request) tab.profileRequests.delete(key); }
}
async function ensureFilterProfile(tab: DatasetTab, column: ColumnInfo): Promise<void> {
  const key = profileKey(tab, column.id), info = tab.info;
  if (tab.stale || !["eq", "ne"].includes(newFilter.op) || !["text", "number", "date"].includes(column.kind) || tab.profileCache.has(key) || tab.profileRequests.has(key)) return;
  // A failed optional profile is cached too; it must not retry on every render.
  tab.profileCache.set(key, null);
  try {
    await getColumnProfile(tab, column.id);
    if (current(tab, info) && filterPanelOpen && newFilter.column === column.id && document.activeElement?.id !== "nf-value") {
      const control = document.getElementById("nf-value-control");
      if (control) control.innerHTML = renderFilterValue(tab, column);
    }
  } catch (error) { failTab(tab, error); if (current(tab, info)) render(); }
}

function renderFilterValue(tab: DatasetTab, column: ColumnInfo): string {
  if (!opNeedsValue(newFilter.op)) return "";
  const comparison = isComparison(newFilter.op);
  const profile = tab.profileCache.get(profileKey(tab, column.id));
  const topValues = profile?.topValues.filter((top) => top.value.kind !== "null" && !isTruncated(top.value)) || [];
  const completeValues = profile && BigInt(profile.distinctCount) > 0n && BigInt(profile.distinctCount) <= 15n && BigInt(profile.distinctCount) === BigInt(topValues.length);
  const choices = comparison && column.kind === "bool" ? ["true", "false"]
    : ["eq", "ne"].includes(newFilter.op) && completeValues && column.kind !== "nested" ? topValues.map((top) => cellText(top.value)) : null;
  const attrs = `id="nf-value" aria-label="Filter value" aria-describedby="nf-error" aria-invalid="${!!filterError}"`;
  if (choices) {
    const custom = (newFilter.value !== "" || filterValueChosen) && !choices.includes(newFilter.value) ? [newFilter.value] : [];
    return `<select ${attrs} data-change="nf-value-choice">
      <option value="" disabled ${newFilter.value === "" && !filterValueChosen ? "selected" : ""}>Choose value…</option>
      ${[...custom, ...choices].map((value) => `<option value="${escapeHtml(JSON.stringify(value))}" ${value === newFilter.value && (value !== "" || filterValueChosen) ? "selected" : ""}>${escapeHtml(value || "(empty string)")}</option>`).join("")}
    </select>`;
  }
  let type = filterInputType(column, newFilter.op);
  // Native time controls only represent milliseconds. Retain precise or
  // zoned values when editing a filter or selecting a value from a profile.
  if (comparison && column.kind === "date" && newFilter.value &&
      (/\.\d{4,}/.test(newFilter.value) || /infinity/i.test(newFilter.value) ||
       (type === "datetime-local" && /(?:Z|[+-]\d{2}(?::?\d{2})?)$/.test(newFilter.value)))) type = "text";
  const value = type === "datetime-local" ? newFilter.value.replace(" ", "T") : newFilter.value;
  const step = type === "number" ? `step="${isIntegerColumn(column) ? "1" : "any"}"` : ["time", "datetime-local"].includes(type) ? 'step="any"' : "";
  const placeholder = opIsMulti(newFilter.op) ? "Values, comma-separated…" : comparison && column.kind === "nested" ? "JSON value…" : comparison && column.dataType.startsWith("TIMESTAMP") ? "YYYY-MM-DD HH:MM:SS±HH:MM" : comparison && column.dataType === "INTERVAL" ? "e.g. 2 days" : "Value…";
  return `<input ${attrs} type="${type}" ${column.kind === "number" ? 'inputmode="decimal"' : ""} ${step} placeholder="${placeholder}" value="${escapeHtml(value)}" autocomplete="off" />`;
}

const EXPORT_FORMATS: { value: ExportFormat; label: string; ext: string }[] = [
  { value: "parquet", label: "Parquet", ext: "parquet" },
  { value: "csv", label: "CSV", ext: "csv" },
  { value: "tsv", label: "TSV", ext: "tsv" },
  { value: "json", label: "JSON array", ext: "json" },
  { value: "jsonl", label: "JSON Lines", ext: "jsonl" },
];

function exportExt(format: ExportFormat): string {
  return EXPORT_FORMATS.find((f) => f.value === format)?.ext || "parquet";
}

function timestampSuffix(date = new Date()): string {
  const pad = (n: number) => String(n).padStart(2, "0");
  return (
    `${date.getFullYear()}${pad(date.getMonth() + 1)}${pad(date.getDate())}-` +
    `${pad(date.getHours())}${pad(date.getMinutes())}${pad(date.getSeconds())}`
  );
}

function defaultExportFileName(fileName: string, format: ExportFormat): string {
  const base = fileName.replace(/\.(parquet|pq|csv|tsv|json|jsonl|ndjson|xlsx|xlsm|xlsb|xls|sqlite3|sqlite|db)$/i, "") || "export";
  return `${base}-export-${timestampSuffix()}.${exportExt(format)}`;
}

// ---------------------------------------------------------------------------
// Backend calls
// ---------------------------------------------------------------------------

const opening = new Map<string, Promise<void>>();
function adoptDataset(info: DatasetInfo): DatasetTab | null {
  if (info.apiVersion !== 1) { void datasets.close(info.sessionId); showToast("Unsupported backend API version", "error"); return null; }
  const existing = getTab(info.path);
  if (existing) { void datasets.close(info.sessionId); activePath = existing.info.path; render(); return existing; }
  const tab: DatasetTab = {
    info, visibleColumns: info.object.columns.map((c) => c.id), page: null, pageColumns: [],
    currentPage: 1, pageSize: info.policy.defaultPageSize, sortColumn: null, sortDesc: false,
    filters: [], quickSearch: "", loading: false, error: null, requestId: 0, queryMs: null,
    selectedRow: null, profileColumn: null, profile: null, profileLoading: false,
    profileError: null, profileCache: new Map(), profileRequests: new Map(), operations: new Set(),
    pageOperation: null, countOperation: null, pageQuery: null, profileRequestId: 0,
    stale: false, closed: false, refreshing: false, doc: null, rawRecord: null,
  };
  tabs.push(tab); activePath = info.path; rememberRecent(info.path);
  newFilter = { column: info.object.columns[0]?.id || "", op: defaultFilterOp(info.object.columns[0]), value: "" };
  filterPanelOpen = false; filterError = ""; filterValueChosen = false;
  render(); return tab;
}
async function openFileByPath(path: string, format?: FormatKind): Promise<void> {
  const existing = getTab(path);
  if (existing) { activePath = path; render(); return; }
  const pending = opening.get(path); if (pending) return pending;
  const work = (async () => {
    try { if (/\.(csv|tsv)$/i.test(path)) { await openDelimited(path); return; } if (/\.(xlsx|xlsm|xlsb|xls)$/i.test(path)) { await openExcel(path); return; } if (/\.(sqlite3|sqlite|db)$/i.test(path)) { await openSqlite(path); return; } const tab = adoptDataset(await datasets.open(path, format ? { format } : {})); if (tab && !tab.page && !tab.loading && !tab.stale) await loadInitial(tab); }
    catch (error) {
      const parsed = errorInfo(error);
      if (parsed.code === "format_mismatch" && !format && /\.(json|jsonl|ndjson)$/i.test(path)) {
        formatPrompt = { path, format: /\.json$/i.test(path) ? "jsonl" : "json", message: parsed.message }; render();
      } else showToast(`Could not open file: ${parsed.message}`, "error");
    }
  })();
  opening.set(path, work); render();
  try { await work; } finally { opening.delete(path); render(); }
}
async function openDelimited(path: string): Promise<void> {
  cancelDelimited();
  const tab = getTab(path), frozen = tab?.info.delimited;
  delimitedDialog = { path, options: frozen ? { ...frozen, typeOverrides: { ...frozen.typeOverrides } } : { delimiter: /\.tsv$/i.test(path) ? "\t" : ",", quote: '"', escape: '"', nullMarker: "\\N", typeOverrides: {} }, preview: null, valid: false, busy: false, error: null, epoch: ++delimitedEpoch };
  render(); await previewDelimited();
}
async function previewDelimited(): Promise<void> {
  const dialog = delimitedDialog; if (!dialog || dialog.busy) return;
  if (dialog.preview) void datasets.discardPreview(dialog.preview.token).catch(() => undefined);
  const epoch = ++delimitedEpoch; dialog.epoch = epoch; dialog.busy = true; dialog.valid = false; dialog.error = null; render();
  try {
    const preview = await datasets.preview(dialog.path, { ...dialog.options, typeOverrides: { ...dialog.options.typeOverrides } });
    if (delimitedDialog === dialog && dialog.epoch === epoch) { dialog.preview = preview; dialog.valid = true; }
    else void datasets.discardPreview(preview.token).catch(() => undefined);
  } catch (error) { if (delimitedDialog === dialog && dialog.epoch === epoch) dialog.error = errorInfo(error).message; }
  finally { if (delimitedDialog === dialog) { dialog.busy = false; render(); } }
}
function changeDelimited(key: string, value: string, column?: string): void {
  const dialog = delimitedDialog; if (!dialog) return;
  if (column) { const overrides = dialog.options.typeOverrides ??= {}; if (value.trim()) overrides[column] = value; else delete overrides[column]; }
  else if (key === "header") dialog.options.header = value === "auto" ? undefined : value === "yes";
  else if (key === "forceText") dialog.options.forceText = value === "yes";
  else if (key === "delimiter") dialog.options.delimiter = value === "\\t" ? "\t" : value;
  else if (key === "quote" || key === "escape" || key === "nullMarker") dialog.options[key] = value;
  dialog.valid = false; dialog.error = null; dialog.epoch = ++delimitedEpoch; render();
}
function cancelDelimited(): void {
  if (delimitedDialog?.preview) void datasets.discardPreview(delimitedDialog.preview.token).catch(() => undefined);
  delimitedDialog = null; ++delimitedEpoch;
}
async function confirmDelimited(): Promise<void> {
  const dialog = delimitedDialog; if (!dialog?.valid || !dialog.preview || dialog.busy) return;
  const epoch = dialog.epoch; dialog.busy = true; render();
  try {
    const info = await datasets.open(dialog.path, { previewToken: dialog.preview.token });
    if (delimitedDialog !== dialog || dialog.epoch !== epoch) { void datasets.close(info.sessionId); return; }
    const previous = getTab(info.path);
    if (previous) { previous.closed = true; invalidate(previous); tabs = tabs.filter(t => t !== previous); void datasets.close(previous.info.sessionId); }
    delimitedDialog = null; const tab = adoptDataset(info); if (tab) await loadPage(tab.info.path);
  } catch (error) { if (delimitedDialog === dialog) { dialog.error = errorInfo(error).message; dialog.valid = false; } }
  finally { if (delimitedDialog === dialog) dialog.busy = false; render(); }
}
async function openExcel(path: string): Promise<void> {
  cancelExcel();
  const frozen = getTab(path)?.info.excel, epoch = ++excelEpoch;
  const dialog: ExcelDialog = { path, workbook: null, sheet: frozen?.sheet ?? "", header: frozen ? frozen.headerRow !== null : true, headerRow: String(frozen?.headerRow ?? 1), busy: true, error: null, epoch };
  excelDialog = dialog; render();
  try {
    const workbook = await datasets.inspectWorkbook(path);
    if (excelDialog !== dialog || dialog.epoch !== epoch) return;
    dialog.workbook = workbook;
    if (!workbook.sheets.some((sheet) => sheet.selectable && sheet.name === dialog.sheet)) {
      dialog.sheet = (workbook.sheets.find((sheet) => sheet.selectable && sheet.visibility === "visible") ?? workbook.sheets.find((sheet) => sheet.selectable))?.name ?? "";
    }
    if (!dialog.sheet) dialog.error = "This workbook has no worksheets with cells to open.";
  } catch (error) { if (excelDialog === dialog && dialog.epoch === epoch) dialog.error = errorInfo(error).message; }
  finally { if (excelDialog === dialog && dialog.epoch === epoch) { dialog.busy = false; render(); } }
}
function changeExcel(key: string, value: string): void {
  const dialog = excelDialog; if (!dialog || dialog.busy) return;
  if (key === "sheet") dialog.sheet = value;
  else if (key === "header") dialog.header = value === "yes";
  else if (key === "headerRow") {
    // Typing keeps focus: update only the confirm button, not the whole dialog.
    dialog.headerRow = value; dialog.error = null;
    document.querySelector<HTMLButtonElement>('[data-action="confirm-excel"]')?.toggleAttribute("disabled", !!headerRowError(dialog));
    return;
  }
  dialog.error = null; dialog.epoch = ++excelEpoch; render();
}
function cancelExcel(): void { excelDialog = null; ++excelEpoch; }
async function confirmExcel(): Promise<void> {
  const dialog = excelDialog; if (!dialog?.workbook || !dialog.sheet || dialog.busy || headerRowError(dialog)) return;
  const epoch = dialog.epoch; dialog.busy = true; dialog.error = null; render();
  try {
    const info = await datasets.open(dialog.path, { excel: { sheet: dialog.sheet, headerRow: dialog.header ? Number(dialog.headerRow.trim()) : null } });
    if (excelDialog !== dialog || dialog.epoch !== epoch) { void datasets.close(info.sessionId); return; }
    const previous = getTab(info.path);
    if (previous) { previous.closed = true; invalidate(previous); tabs = tabs.filter(t => t !== previous); void datasets.close(previous.info.sessionId); }
    excelDialog = null; const tab = adoptDataset(info); if (tab) await loadPage(tab.info.path);
  } catch (error) { if (excelDialog === dialog) dialog.error = errorInfo(error).message; }
  finally { if (excelDialog === dialog) dialog.busy = false; render(); }
}
async function openSqlite(path: string): Promise<void> {
  cancelSqlite();
  const frozen = getTab(path)?.info.sqlite, epoch = ++sqliteEpoch;
  const dialog: SqliteDialog = { path, database: null, object: frozen?.object ?? "", busy: true, error: null, epoch };
  sqliteDialog = dialog; render();
  try {
    const database = await datasets.inspectDatabase(path);
    if (sqliteDialog !== dialog || dialog.epoch !== epoch) return;
    dialog.database = database;
    if (!database.objects.some((o) => o.selectable && o.name === dialog.object)) dialog.object = database.objects.find((o) => o.selectable)?.name ?? "";
    if (!dialog.object) dialog.error = "This database has no tables or views that Sivro can open.";
  } catch (error) { if (sqliteDialog === dialog && dialog.epoch === epoch) dialog.error = errorInfo(error).message; }
  finally { if (sqliteDialog === dialog && dialog.epoch === epoch) { dialog.busy = false; render(); } }
}
function changeSqlite(value: string): void {
  const dialog = sqliteDialog; if (!dialog || dialog.busy) return;
  dialog.object = value; dialog.error = null; dialog.epoch = ++sqliteEpoch; render();
}
function cancelSqlite(): void { sqliteDialog = null; ++sqliteEpoch; }
async function confirmSqlite(): Promise<void> {
  const dialog = sqliteDialog; if (!dialog?.database || !dialog.object || dialog.busy) return;
  const epoch = dialog.epoch; dialog.busy = true; dialog.error = null; render();
  try {
    const info = await datasets.open(dialog.path, { sqlite: { object: dialog.object } });
    if (sqliteDialog !== dialog || dialog.epoch !== epoch) { void datasets.close(info.sessionId); return; }
    const previous = getTab(info.path);
    if (previous) { previous.closed = true; invalidate(previous); tabs = tabs.filter(t => t !== previous); void datasets.close(previous.info.sessionId); }
    sqliteDialog = null; const tab = adoptDataset(info); if (tab) await loadPage(tab.info.path);
  } catch (error) { if (sqliteDialog === dialog) dialog.error = errorInfo(error).message; }
  finally { if (sqliteDialog === dialog) dialog.busy = false; render(); }
}
async function loadPage(path: string): Promise<void> {
  const tab = getTab(path); if (!tab || !tab.visibleColumns.length || tab.stale || tab.refreshing) return;
  for (const filter of tab.filters) {
    const column = columnById(tab, filter.column), error = column ? validateFilter(column, filter, tab.info.format === "sqlite") : "Unknown column";
    if (error) { showToast(error, "error"); return; }
  }
  const requestId = ++tab.requestId, info = tab.info;
  void tab.pageOperation?.cancel(); void tab.countOperation?.cancel(); tab.countOperation = null;
  ++tab.profileRequestId; tab.profile = null; tab.profileColumn = null; tab.profileLoading = false;
  tab.loading = true; tab.error = null;
  const query = queryFor(tab), operation = task(tab, "page", query);
  tab.pageOperation = operation; render();
  try {
    const page = await operation.result;
    if (!current(tab, info) || requestId !== tab.requestId) return;
    if (!matches(page, info) || page.columnIds.join("\0") !== query.columnIds.join("\0") || page.rows.some((row) => row.length !== page.columnIds.length) || page.rowIds.length !== page.rows.length) throw new Error("The page response does not match the requested schema.");
    tab.page = page; tab.pageColumns = [...page.columnIds]; tab.pageQuery = query;
    tab.queryMs = page.queryMs; tab.currentPage = Math.floor(page.offset / page.limit) + 1;
    tab.selectedRow = null; tab.rawRecord = null;
  } catch (error) {
    if (!current(tab, info) || requestId !== tab.requestId) return;
    tab.error = failTab(tab, error); tab.page = null; tab.pageColumns = []; tab.pageQuery = null;
  } finally {
    if (current(tab, info) && requestId === tab.requestId) { tab.loading = false; tab.pageOperation = null; }
    if (current(tab, info)) render();
  }
}
async function loadInitial(tab: DatasetTab): Promise<void> {
  if (isDocument(tab)) await loadDocument(tab, ""); else await loadPage(tab.info.path);
}
async function loadDocument(tab: DatasetTab, key: string, offset = 0): Promise<void> {
  if (tab.stale || tab.refreshing || tab.closed) return;
  const info = tab.info, state = tab.doc ??= { branches: new Map(), expanded: new Set() };
  const branch: DocumentBranch = state.branches.get(key) ?? { node: null, children: [], hasMore: false, loading: false, error: null };
  if (branch.loading) return;
  branch.loading = true; branch.error = null; state.branches.set(key, branch); render();
  try {
    const result = await task(tab, "document", { path: parsePath(key), offset, limit: info.policy.jsonChildren ?? 100, mode: "children" }).result;
    if (!current(tab, info) || tab.doc !== state) return;
    if (!matches(result, info) || pathKey(result.path) !== key || result.offset !== offset) throw new Error("The document response does not match this node.");
    branch.node = result.node; branch.children = offset ? [...branch.children, ...result.children] : result.children; branch.hasMore = result.hasMore;
  } catch (error) {
    if (current(tab, info) && tab.doc === state) { const message = failTab(tab, error); if (tab.doc === state) branch.error = message; }
  } finally { branch.loading = false; if (current(tab, info)) render(); }
}
async function copyDocument(tab: DatasetTab, key: string): Promise<void> {
  if (tab.stale) return;
  const info = tab.info;
  try {
    const result = await task(tab, "document", { path: parsePath(key), offset: 0, limit: 1, mode: "copy" }).result;
    if (!current(tab, info) || !matches(result, info) || pathKey(result.path) !== key || result.text === null) return;
    await copyText(result.text, "Copied JSON");
  } catch (error) { if (current(tab, info)) { showToast(failTab(tab, error), "error"); render(); } }
}
async function loadRawRecord(tab: DatasetTab): Promise<void> {
  const page = tab.page, row = tab.selectedRow, info = tab.info;
  if (!page || row === null || tab.stale || tab.rawRecord?.loading) return;
  const record = { row, text: null as string | null, loading: true }; tab.rawRecord = record; render();
  try {
    const result = await task(tab, "read_row", { rowId: page.rowIds[row], columnIds: [tab.pageColumns[0]], rawRecord: true }).result;
    if (current(tab, info) && tab.rawRecord === record && tab.page === page && matches(result, info) && result.rowId === page.rowIds[row]) record.text = result.rawRecord ?? null;
  } catch (error) { if (current(tab, info) && tab.rawRecord === record) { tab.rawRecord = null; showToast(failTab(tab, error), "error"); } }
  finally { record.loading = false; if (current(tab, info)) render(); }
}
async function loadCount(tab: DatasetTab): Promise<void> {
  if (!tab.page || !tab.pageQuery || tab.stale || tab.countOperation) return;
  const page = tab.page, info = tab.info, requestId = tab.requestId;
  page.count = { status: "pending" };
  const operation = task(tab, "count", tab.pageQuery); tab.countOperation = operation; render();
  try {
    const result = await operation.result;
    if (!matches(result, info)) throw new Error("The count response does not match this view.");
    if (current(tab, info) && tab.page === page && tab.requestId === requestId && matches(result, info)) page.count = result.count;
  } catch (error) {
    if (current(tab, info) && tab.page === page && tab.requestId === requestId) {
      page.count = { status: "failed", reason: failTab(tab, error) };
    }
  } finally { if (tab.countOperation === operation) tab.countOperation = null; if (current(tab, info)) render(); }
}
async function loadProfile(tab: DatasetTab, column: string): Promise<void> {
  if (tab.stale) return;
  const token = ++tab.profileRequestId, info = tab.info, key = profileKey(tab, column);
  tab.profileColumn = column; tab.profile = null; tab.profileError = null; tab.profileLoading = true; tab.selectedRow = null; render();
  try {
    const result = await getColumnProfile(tab, column);
    if (current(tab, info) && token === tab.profileRequestId && profileKey(tab, column) === key && tab.profileColumn === column) tab.profile = result;
  } catch (error) { if (current(tab, info) && token === tab.profileRequestId) tab.profileError = failTab(tab, error); }
  finally { if (current(tab, info) && token === tab.profileRequestId) tab.profileLoading = false; if (current(tab, info)) render(); }
}
async function refreshTab(tab: DatasetTab): Promise<void> {
  if (tab.refreshing) return;
  const info = tab.info; invalidate(tab); tab.stale = true; tab.refreshing = true; tab.loading = false; render();
  try {
    const fresh = await datasets.refresh(info.sessionId);
    if (!current(tab, info)) { if (tab.closed) void datasets.close(fresh.sessionId); return; }
    tab.info = fresh; tab.visibleColumns = fresh.object.columns.map((c) => c.id);
    tab.filters = []; tab.quickSearch = ""; tab.sortColumn = null; tab.sortDesc = false;
    tab.currentPage = 1; tab.pageSize = fresh.policy.defaultPageSize; tab.stale = false; tab.error = null;
    newFilter = { column: fresh.object.columns[0]?.id || "", op: defaultFilterOp(fresh.object.columns[0]), value: "" };
    filterError = ""; filterValueChosen = false; tab.refreshing = false;
    await loadInitial(tab);
  } catch (error) { if (!tab.closed) { tab.error = errorInfo(error).message; tab.stale = true; } }
  finally { tab.refreshing = false; if (!tab.closed) render(); }
}
async function fullCells(tab: DatasetTab, row: number, columns: string[]): Promise<Cell[] | null> {
  const page = tab.page, info = tab.info; if (!page || tab.stale) return null;
  const result = await task(tab, "read_row", { rowId: page.rowIds[row], columnIds: columns }).result;
  if (!current(tab, info) || activeTab() !== tab || tab.page !== page || tab.selectedRow !== row || !matches(result, info) || result.rowId !== page.rowIds[row] || result.columnIds.join("\0") !== columns.join("\0")) return null;
  return result.cells;
}
async function chooseAndOpenFile(): Promise<void> {
  const selected = await open({ multiple: false, filters: [{ name: "Parquet, CSV, TSV, JSON, JSON Lines, Excel and SQLite", extensions: ["parquet", "pq", "csv", "tsv", "json", "jsonl", "ndjson", "xlsx", "xlsm", "xlsb", "xls", "sqlite", "sqlite3", "db"] }] });
  if (typeof selected === "string" && selected) await openFileByPath(selected);
}
async function createSampleFile(): Promise<void> {
  const dest = await save({ defaultPath: "sivro-sample.parquet", filters: [{ name: "Parquet", extensions: ["parquet"] }] }); if (!dest) return;
  try { const tab = adoptDataset(await datasets.sample(dest)); if (tab) await loadPage(tab.info.path); }
  catch (error) { showToast(`Could not create sample: ${errorInfo(error).message}`, "error"); }
}
async function runExport(): Promise<void> {
  const tab = activeTab(); if (!tab || exportBusy || !tab.pageQuery || tab.stale || tab.loading) return;
  const info = tab.info, view = tab.pageQuery, format = exportFormat, maxRows = exportMaxRows;
  const dest = await save({ defaultPath: defaultExportFileName(info.fileName, format), filters: [{ name: EXPORT_FORMATS.find(f => f.value === format)?.label ?? format, extensions: [exportExt(format)] }] });
  if (!dest || !current(tab, info)) return;
  exportBusy = true; render();
  try {
    const result = await task(tab, "export", { destination: dest, format, maxRows, view }).result;
    if (!current(tab, info) || !matches(result, info)) return;
    if (activeTab() === tab && exportModalOpen) lastExport = result;
    showToast(`Exported ${formatNumber(result.exportedRows)} rows${result.truncated ? " — row cap reached; additional rows omitted" : ""}`, result.truncated ? "info" : "success");
  } catch (error) { if (current(tab, info)) showToast(`Export failed: ${failTab(tab, error)}`, "error"); }
  finally { exportBusy = false; render(); }
}

// ---------------------------------------------------------------------------
// Render
// ---------------------------------------------------------------------------

function kindBadge(col: ColumnInfo): string {
  const labels: Record<string, string> = {
    number: "#",
    text: "Aa",
    bool: "✓",
    date: "◷",
    other: "◇",
    nested: "{}",
  };
  return `<span class="kind-badge kind-${col.kind}" title="${escapeHtml(col.dataType)}">${labels[col.kind] || "◇"}</span>`;
}

function renderTitlebar(): string {
  const platform = isMac ? "macos" : "windows";
  return `
    <header class="titlebar" data-platform="${platform}" data-tauri-drag-region>
      <div class="titlebar-leading">
        <div class="brand-mark">${icon("table")}</div>
        <span class="titlebar-title">Sivro</span>
        <span class="titlebar-sub">local data explorer</span>
      </div>
      <div class="titlebar-trailing">
        <div class="window-controls">
          <button class="win-btn win-min" data-action="win-min" title="Minimize">${icon("minimize")}</button>
          <button class="win-btn win-max" data-action="win-max" title="Maximize">${icon("maximize")}</button>
          <button class="win-btn win-close" data-action="win-close" title="Close">${icon("close")}</button>
        </div>
      </div>
    </header>`;
}

function renderSidebar(tab: DatasetTab | null): string {
  const visibleCols = columnSearch.trim().toLowerCase();
  const schemaList = tab
    ? tab.info.object.columns.filter(
        (c) =>
          !visibleCols ||
          c.name.toLowerCase().includes(visibleCols) ||
          c.dataType.toLowerCase().includes(visibleCols)
      )
    : [];

  return `
    <aside class="sidebar">
      <div class="side-actions">
        <button class="btn-primary" data-action="open-file">${icon("upload")}<span>Open file</span><kbd>${isMac ? "⌘O" : "Ctrl+O"}</kbd></button>
        <button class="btn-ghost" data-action="make-sample" title="Generate a demo parquet file to explore">${icon("sparkles")}<span>Sample</span></button>
      </div>

      <div class="side-section">
        <div class="side-heading"><span>Open files</span><span class="count-pill">${tabs.length}</span></div>
        <div class="open-list">
          ${[...opening.keys()].filter((p) => !getTab(p)).map((p) => `
            <div class="open-item pending" role="status" title="${escapeHtml(p)}"><span class="spinner"></span><span class="open-name">Opening ${escapeHtml(baseName(p))}…</span></div>`).join("")}
          ${tabs.length === 0 && opening.size === 0 ? `<div class="side-empty">No files open yet.</div>` : tabs.map((t) => `
            <button class="open-item ${t.info.path === activePath ? "active" : ""}" data-action="switch-tab" data-path="${escapeHtml(t.info.path)}" title="${escapeHtml(t.info.path)}">
              <span class="open-dot"></span>
              <span class="open-name">${escapeHtml(t.info.fileName)}</span>
              <span class="open-rows">${t.info.document ? "document" : formatCount(t.info.object.rowCount)}</span>
              <span class="open-info" data-action="open-file-info" data-path="${escapeHtml(t.info.path)}" title="File details">${icon("info")}</span>
              <span class="open-close" data-action="close-tab" data-path="${escapeHtml(t.info.path)}" title="Close file">${icon("close")}</span>
            </button>`).join("")}
        </div>
      </div>

      ${tab && !isDocument(tab) ? `
      <div class="side-section side-schema">
        <div class="side-heading"><span>Schema · ${tab.visibleColumns.length}/${tab.info.object.columns.length}</span>
          <button class="link-btn" data-action="columns-reset" title="Show all columns">reset</button>
        </div>
        <div class="schema-search">${icon("search")}<input id="column-search" type="text" placeholder="Search columns…" value="${escapeHtml(columnSearch)}" autocomplete="off" /></div>
        <div class="schema-list">
          ${schemaList.map((c) => {
            const visible = tab.visibleColumns.includes(c.id);
            return `
            <div class="schema-row ${visible ? "" : "hidden-col"} ${tab.profileColumn === c.id ? "profiling" : ""}">
              <button class="eye-btn" data-action="toggle-column" data-col="${escapeHtml(c.id)}" title="${visible ? "Hide column" : "Show column"}">${icon(visible ? "eye" : "eye-off")}</button>
              <button class="schema-main" data-action="show-profile" data-col="${escapeHtml(c.id)}" title="${escapeHtml(c.dataType)}">
                ${kindBadge(c)}
                <span class="schema-name">${escapeHtml(c.name)}</span>
                <span class="schema-type">${escapeHtml(c.dataType)}</span>
              </button>
            </div>`;
          }).join("") || `<div class="side-empty">No columns match.</div>`}
        </div>
      </div>` : ""}

      <div class="side-section side-recents">
        <div class="side-heading"><span>${icon("clock")} Recent</span>
          ${recents.length ? `<button class="link-btn" data-action="clear-recents">clear</button>` : ""}
        </div>
        <div class="recent-list">
          ${recents.length === 0 ? `<div class="side-empty">Files you open will appear here.</div>` : recents.map((p) => `
            <button class="recent-item" data-action="open-recent" data-path="${escapeHtml(p)}" title="${escapeHtml(p)}">
              ${icon("file")}<span>${escapeHtml(baseName(p))}</span>
            </button>`).join("")}
        </div>
      </div>
    </aside>`;
}

function renderTabStrip(): string {
  if (tabs.length === 0) return "";
  return `
    <div class="tabstrip" role="tablist" aria-label="Open datasets">
      ${tabs.map((t) => `
        <button class="dataset-tab ${t.info.path === activePath ? "active" : ""}" data-action="switch-tab" data-path="${escapeHtml(t.info.path)}" role="tab" aria-selected="${t.info.path === activePath}">
          <span class="tab-file">${escapeHtml(t.info.fileName)}</span>
          <span class="tab-meta">${escapeHtml(tabMeta(t))}</span>
          <span class="tab-info" data-action="open-file-info" data-path="${escapeHtml(t.info.path)}" title="File details">${icon("info")}</span>
          <span class="tab-close" data-action="close-tab" data-path="${escapeHtml(t.info.path)}" title="Close">${icon("close")}</span>
        </button>`).join("")}
    </div>`;
}

// Precision, typing and limit notes stay one click away instead of only in File details.
function notesButton(tab: DatasetTab): string {
  const notes = tab.info.object.warnings.length;
  return notes ? `<button class="btn-tool" data-action="open-file-info" data-path="${escapeHtml(tab.info.path)}" title="Precision, typing and limit notes for this source">${icon("info")}<span>${notes} ${notes === 1 ? "note" : "notes"}</span></button>` : "";
}

function renderToolbar(tab: DatasetTab): string {
  const activeFilterCount = tab.filters.length + (tab.quickSearch.trim() ? 1 : 0);
  return `
    <div class="toolbar">
      <div class="search-wrap">
        ${icon("search", "search-ic")}
        <input id="quick-search" type="text" placeholder="Search all text columns…  (${isMac ? "⌘K" : "Ctrl+K"})" value="${escapeHtml(tab.quickSearch)}" autocomplete="off" />
        ${tab.quickSearch ? `<button class="icon-btn" data-action="clear-search" title="Clear search">${icon("close")}</button>` : ""}
      </div>
      <div class="toolbar-actions">
        ${tab.info.format === "csv" || tab.info.format === "tsv" ? `<button class="btn-tool" data-action="parsing-settings">Parsing settings</button>` : ""}
        ${tab.info.sqlite ? `<button class="btn-tool" data-action="sqlite-objects" title="Choose another table or view">Table: ${escapeHtml(tab.info.sqlite.object)}</button>` : ""}
        ${tab.info.excel ? `<button class="btn-tool" data-action="excel-sheets" title="Choose another sheet or header row">Sheet: ${escapeHtml(tab.info.excel.sheet)}</button>` : ""}
        ${notesButton(tab)}
        <button class="btn-tool" data-action="refresh-source" ${tab.refreshing ? "disabled" : ""} title="Reopen changed source and reset its view">${icon("refresh")}<span>Refresh</span></button>
        ${tab.operations.size ? `<button class="btn-tool" data-action="cancel-work">${icon("close")}<span>Cancel work</span></button>` : ""}
        <button class="btn-tool ${filterPanelOpen || tab.filters.length ? "active" : ""}" data-action="toggle-filters">
          ${icon("filter")}<span>Filters</span>
          ${tab.filters.length ? `<span class="pill">${tab.filters.length}</span>` : ""}
        </button>
        <button class="btn-tool btn-apply" data-action="apply-filters" title="Apply search and filters">${icon("check")}<span>Apply</span></button>
        <button class="btn-tool btn-export" data-action="open-export" title="Export current view" ${tab.loading || tab.stale || !tab.page ? "disabled" : ""}>${icon("download")}<span>Export</span></button>
        ${activeFilterCount ? `<button class="btn-tool btn-danger-ghost" data-action="reset-view" title="Clear search, filters and sorting">${icon("x-circle")}<span>Reset</span></button>` : ""}
      </div>
    </div>
    ${filterPanelOpen ? renderFilterPanel(tab) : ""}
    ${tab.filters.length && !filterPanelOpen ? `
      <div class="active-chips">
        ${tab.filters.map((f, i) => `
          <span class="chip"><strong>${escapeHtml(columnLabel(tab, f.column))}</strong> ${escapeHtml(opLabel(f.op))}${opNeedsValue(f.op) ? ` <em>“${escapeHtml(f.value)}”</em>` : ""}
            <button data-action="edit-filter" data-index="${i}" title="Edit filter">${icon("pencil")}</button>
            <button data-action="remove-filter" data-index="${i}" title="Remove filter">${icon("close")}</button>
          </span>`).join("")}
      </div>` : ""}`;
}

function renderFilterPanel(tab: DatasetTab): string {
  const cols = tab.info.object.columns;
  if (!newFilter.column || !cols.some((c) => c.id === newFilter.column)) {
    newFilter.column = cols[0]?.id || "";
  }
  const column = columnById(tab, newFilter.column) || cols[0];
  if (column) void ensureFilterProfile(tab, column);
  return `
    <div class="filter-panel">
      <div class="filter-list">
        ${tab.filters.map((f, i) => `
          <span class="chip static"><strong>${escapeHtml(columnLabel(tab, f.column))}</strong> ${escapeHtml(opLabel(f.op))}${opNeedsValue(f.op) ? ` <em>“${escapeHtml(f.value)}”</em>` : ""}
            <button data-action="edit-filter" data-index="${i}" title="Edit filter">${icon("pencil")}</button>
            <button data-action="remove-filter" data-index="${i}" title="Remove filter">${icon("close")}</button>
          </span>`).join("") || `<div class="filter-hint">No column filters yet — add one below, then click Apply in the toolbar.</div>`}
      </div>
      <div class="filter-builder">
        <select id="nf-column" data-change="nf-column" aria-label="Filter column">
          ${cols.map((c) => `<option value="${escapeHtml(c.id)}" ${c.id === newFilter.column ? "selected" : ""}>${escapeHtml(c.name)}</option>`).join("")}
        </select>
        <select id="nf-op" data-change="nf-op" aria-label="Filter operator">
          ${availableFilterOps(column).map((o) => `<option value="${o.value}" ${o.value === newFilter.op ? "selected" : ""}>${escapeHtml(o.label)}</option>`).join("")}
        </select>
        <div id="nf-value-control" class="filter-value-control">${column ? renderFilterValue(tab, column) : ""}</div>
        <button class="icon-btn icon-btn-primary" data-action="add-filter" title="Add filter to the list" aria-label="Add filter">${icon("plus")}</button>
        <button class="icon-btn" data-action="clear-filter-builder" title="Clear filter value" aria-label="Clear filter value">${icon("close")}</button>
      </div>
      <div id="nf-error" class="filter-error" role="alert">${escapeHtml(filterError)}</div>
    </div>`;
}

function cellHtml(cell: Cell, _kind: string): string {
  if (cell.kind === "null") return `<span class="null-badge">NULL</span>`;
  if (cell.kind === "boolean") return `<span class="bool-pill ${cell.value ? "on" : "off"}">${cell.value}</span>`;
  return `<span class="cell-text">${escapeHtml(clipped(cellText(cell)))}${isTruncated(cell) && cell.kind !== "binary" ? "…" : ""}</span>`;
}
function drawerCell(cell: Cell, row: number, column: number): string {
  const value = cellText(cell);
  if (isTruncated(cell)) return `<pre>${escapeHtml(value)}…</pre><button class="btn-ghost btn-sm" data-action="expand-cell" data-row="${row}" data-ci="${column}">Expand value (up to 1 MiB)</button>`;
  return cell.kind === "json" ? structuredCellHtml(cell.value) : escapeHtml(value);
}

function renderGrid(tab: DatasetTab): string {
  if (tab.refreshing) return `<div class="grid-empty"><span class="spinner"></span><p>Refreshing source…</p></div>`;
  // While a next page / sort / filter is loading, keep the current rows on
  // screen — the loading state lives in the footer. Only show the skeleton
  // on the very first load when there is nothing to display yet.
  if (tab.loading && (!tab.page || tab.page.rows.length === 0) && !tab.error) {
    return `
      <div class="grid-wrap loading">
        <div class="skeleton-head"></div>
        ${Array.from({ length: 12 }).map((_, i) => `<div class="skeleton-row" style="animation-delay:${i * 45}ms"></div>`).join("")}
      </div>`;
  }
  if (tab.error) {
    return `
      <div class="grid-empty">
        <div class="empty-art">${icon("x-circle")}</div>
        <h3>${tab.stale ? "Source changed or session unavailable" : "Couldn't read this file"}</h3>
        <p>${escapeHtml(tab.error)}</p>
        <button class="btn-primary" data-action="${tab.stale ? "refresh-source" : "retry-page"}">${icon("refresh")}<span>${tab.stale ? "Refresh source" : "Try again"}</span></button>
      </div>`;
  }
  const page = tab.page;
  if (!page || page.rows.length === 0) {
    return `
      <div class="grid-empty">
        <div class="empty-art">${icon("search")}</div>
        <h3>No rows match</h3>
        <p>Try widening the search or removing a filter.</p>
        <button class="btn-primary" data-action="reset-view">${icon("x-circle")}<span>Reset view</span></button>
      </div>`;
  }
  const cols = tab.pageColumns;
  const kinds = cols.map((c) => columnById(tab, c)?.kind || "other");
  return `
    <div class="grid-scroll${tab.loading ? " is-loading" : ""}" id="grid-scroll">
      <table class="grid">
        <thead>
          <tr>
            <th class="rownum-col">#</th>
            ${cols.map((c, ci) => {
              const sorted = tab.sortColumn === c, sortable = columnById(tab, c)?.sortable !== false;
              return `
              <th class="data-col ${kinds[ci] === "number" ? "num" : ""} ${sorted ? "sorted" : ""}" data-col-index="${ci}">
                <button class="th-main" ${sortable ? `data-action="sort-col" data-col="${escapeHtml(c)}" title="Sort by ${escapeHtml(columnLabel(tab, c))}"` : `disabled title="Raw JSON values cannot be sorted by value"`}>
                  <span class="th-name">${escapeHtml(columnLabel(tab, c))}</span>
                  <span class="th-sort">${!sortable ? "" : sorted ? icon(tab.sortDesc ? "sort-desc" : "sort-asc") : icon("sort")}</span>
                </button>
                <span class="th-type">${escapeHtml(columnById(tab, c)?.dataType || "")}</span>
              </th>`;
            }).join("")}
          </tr>
        </thead>
        <tbody>
          ${page.rows.map((row, ri) => {
            const absolute = page.offset + ri + 1;
            return `
            <tr class="${tab.selectedRow === ri ? "selected" : ""}" data-action="select-row" data-row="${ri}">
              <td class="rownum-col">${formatNumber(absolute)}</td>
              ${row.map((value, ci) => `
                <td class="${kinds[ci] === "number" ? "num" : ""}" title="${escapeHtml(clipped(cellText(value)))}">${cellHtml(value, kinds[ci])}</td>`).join("")}
            </tr>`;
          }).join("")}
        </tbody>
      </table>
    </div>`;
}

function renderPagination(tab: DatasetTab): string {
  const count = tab.page?.count ?? { status: "unknown" } as CountState, total = safeCount(count);
  const totalPages = total === null ? null : Math.max(1, Math.ceil(total / tab.pageSize));
  const page = tab.currentPage, start = tab.page?.rows.length ? tab.page.offset + 1 : 0;
  const end = tab.page?.rows.length ? tab.page.offset + tab.page.rows.length : 0;
  const busy = tab.loading || tab.stale || tab.refreshing;
  const pending = [...tab.operations].some((op) => op.status === "cancel_pending");
  return `<div class="statusbar">
    <div class="page-controls">
      <button class="icon-btn" data-action="goto-page" data-page="1" ${page <= 1 || busy ? "disabled" : ""} title="First page">${icon("chevrons-left")}</button>
      <button class="icon-btn" data-action="goto-page" data-page="${page - 1}" ${page <= 1 || busy ? "disabled" : ""} title="Previous page">${icon("chevron-left")}</button>
      <span class="page-info">Rows <strong>${formatNumber(start)}–${formatNumber(end)}</strong> · <strong>${formatCount(count)}</strong>${count.status === "known" ? " total" : ""}</span>
      <button class="icon-btn" data-action="goto-page" data-page="${page + 1}" ${!tab.page?.hasMore || busy ? "disabled" : ""} title="Next page">${icon("chevron-right")}</button>
      <button class="icon-btn" data-action="goto-page" data-page="${totalPages ?? page}" ${totalPages === null || page >= totalPages || busy ? "disabled" : ""} title="Last page (requires an exact count)">${icon("chevrons-right")}</button>
      ${count.status !== "known" ? `<button class="link-btn" data-action="count-rows" ${busy || count.status === "pending" || !tab.page ? "disabled" : ""} title="${count.status === "failed" ? escapeHtml(count.reason) : "An exact count may scan the entire view"}">${count.status === "pending" ? "Counting…" : "Count rows"}</button>` : ""}
    </div>
    <div class="status-center">${pending ? "Cancellation requested; waiting…" : busy ? "Loading…" : ""}</div>
    <div class="status-meta"><span class="meta-chip">page ${formatNumber(page)}${totalPages === null ? "" : ` / ${formatNumber(totalPages)}`}</span>
      <label class="pagesize">rows <select data-change="page-size" ${busy ? "disabled" : ""}>${[...new Set([tab.info.policy.defaultPageSize, ...PAGE_SIZES])].sort((a, b) => a - b).filter((s) => s <= tab.info.policy.maxPageSize).map((s) => `<option value="${s}" ${s === tab.pageSize ? "selected" : ""}>${s}</option>`).join("")}</select></label>
      <span class="meta-chip">${formatBytes(tab.info.fileSize)}</span>
    </div></div>`;
}

function renderMain(tab: DatasetTab | null): string {
  if (!tab) {
    return `
    <main class="main hero-wrap">
      <div class="hero" id="drop-hero">
        <h1>Drop a <em>data</em> file<br/>to peek inside</h1>
        <p>Open Parquet, CSV, TSV, JSON, JSON Lines, Excel and SQLite files right on your machine — filter, sort and export with ease.</p>
        <div class="hero-actions">
          <button class="btn-primary btn-lg" data-action="open-file">${icon("upload")}<span>Open data file</span></button>
          <button class="btn-ghost btn-lg" data-action="make-sample">${icon("sparkles")}<span>Try a sample file</span></button>
        </div>
        ${recents.length ? `
        <div class="hero-recents">
          <div class="hero-recents-title">${icon("clock")} Jump back in</div>
          ${recents.slice(0, 5).map((p) => `
            <button class="hero-recent" data-action="open-recent" data-path="${escapeHtml(p)}" title="${escapeHtml(p)}">
              ${icon("file")}<span>${escapeHtml(baseName(p))}</span>
            </button>`).join("")}
        </div>` : ""}
      </div>
    </main>`;
  }
  if (isDocument(tab)) {
    return `
    <main class="main">
      ${renderTabStrip()}
      <div class="toolbar"><div class="toolbar-actions">
        ${notesButton(tab)}
        <button class="btn-tool" data-action="refresh-source" ${tab.refreshing ? "disabled" : ""} title="Reopen changed source">${icon("refresh")}<span>Refresh</span></button>
        <button class="btn-tool btn-export" disabled title="JSON documents are inspected and copied node by node; table export needs a root array of objects or JSON Lines">${icon("download")}<span>Export</span></button>
        ${tab.operations.size ? `<button class="btn-tool" data-action="cancel-work">${icon("close")}<span>Cancel work</span></button>` : ""}
      </div></div>
      ${tab.refreshing ? `<div class="grid-empty"><span class="spinner"></span><p>Refreshing source…</p></div>` : renderDocument(tab.info, tab.doc, tab.error, tab.stale)}
    </main>`;
  }
  return `
    <main class="main">
      ${renderTabStrip()}
      ${renderToolbar(tab)}
      ${renderGrid(tab)}
      ${renderPagination(tab)}
    </main>`;
}

function renderDrawer(tab: DatasetTab | null): string {
  if (!tab) return "";
  const hasRow = tab.selectedRow !== null && tab.page?.rows[tab.selectedRow];
  const hasProfile = tab.profileColumn !== null;
  if (!hasRow && !hasProfile) return "";

  if (hasRow) {
    const ri = tab.selectedRow as number;
    const row = (tab.page as PageResult).rows[ri];
    const absolute = (tab.page as PageResult).offset + ri + 1;
    const pairs = tab.pageColumns.map((c, ci) => ({ name: columnLabel(tab, c), value: row[ci], kind: columnById(tab, c)?.kind || "other", type: columnById(tab, c)?.dataType || "" }));
    const json = row.some(isTruncated) ? "Large values are previewed. Copy row loads full values up to 1 MiB." : rowJson(tab, row);
    return `
    <aside class="drawer">
      <div class="drawer-header">
        <span class="drawer-title">Row ${formatNumber(absolute)}</span>
        <button class="icon-btn drawer-close" data-action="close-drawer" title="Close panel (Esc)">${icon("close")}</button>
      </div>
      <div class="drawer-body">
        <button class="btn-ghost btn-sm drawer-copy" data-action="copy-row-json">${icon("copy")}<span>Copy row as JSON</span></button>
        ${row.some((cell) => cell.kind === "binary" || cell.kind === "number" && /^[+-]?(?:NaN|Infinity)$/i.test(cell.value) || cell.kind === "json" && !isTruncated(cell) && cellForCopy(cell) !== cell.value) ? `<p class="modal-desc">Non-finite values copy as strings; binary values use tagged hex objects.</p>` : ""}
        ${pairs.map((p, ci) => `
          <div class="kv">
            <div class="kv-head">
              <span class="kv-name" title="${escapeHtml(p.name)}">${escapeHtml(p.name)}</span>
              <span class="kv-type">${escapeHtml(p.type)}</span>
              <button class="mini-btn" data-action="copy-cell" data-row="${ri}" data-ci="${ci}" title="Copy value">${icon("copy")}</button>
            </div>
            <div class="kv-value ${p.value.kind === "null" ? "is-null" : ""} ${p.kind === "nested" ? "is-structured" : ""}">${drawerCell(p.value, ri, ci)}</div>
          </div>`).join("")}
        <div class="kv json-block"><div class="kv-head"><span class="kv-name">JSON</span></div><pre>${escapeHtml(json)}</pre></div>
        ${tab.info.format === "json" || tab.info.format === "jsonl" ? `<div class="kv"><div class="kv-head"><span class="kv-name">Source record</span>${tab.rawRecord?.row === ri && tab.rawRecord.text !== null ? `<button class="mini-btn" data-action="copy-raw-record" title="Copy source record">${icon("copy")}</button>` : ""}</div>
          <div class="kv-value is-structured">${tab.rawRecord?.row === ri ? (tab.rawRecord.loading ? `<span class="spinner"></span>` : tab.rawRecord.text === null ? "Unavailable" : structuredCellHtml(tab.rawRecord.text)) : `<button class="btn-ghost btn-sm" data-action="load-raw-record">Show source record (all keys, null vs missing)</button>`}</div></div>` : ""}
      </div>
    </aside>`;
  }

  // Column details view (single panel, no tabs)
  const col = tab.profileColumn as string;
  const info = columnById(tab, col);
  const profile = tab.profile;
  const maxCount = Math.max(1, ...(profile?.topValues.map((t) => Number(t.count)) || [1]));
  return `
    <aside class="drawer">
      <div class="drawer-header">
        <span class="drawer-title" title="${escapeHtml(columnLabel(tab, col))}">Column · ${escapeHtml(columnLabel(tab, col))}</span>
        <button class="icon-btn drawer-close" data-action="close-drawer" title="Close panel (Esc)">${icon("close")}</button>
      </div>
      <div class="drawer-body">
        <div class="prof-head">
          ${info ? kindBadge(info) : ""}
          <div><div class="prof-name">${escapeHtml(columnLabel(tab, col))}</div><div class="muted">Statistics for the loaded view</div><div class="prof-type">${escapeHtml(info?.dataType || "")}</div></div>
        </div>
        ${tab.profileLoading ? `<div class="prof-loading"><span class="spinner"></span> Loading…</div>` : ""}
        ${tab.profileError ? `<div class="prof-error">${escapeHtml(tab.profileError)}</div>` : ""}
        ${profile ? `
          <div class="stat-grid">
            <div class="stat"><span class="stat-num">${formatNumber(profile.nonNullCount)}</span><span class="stat-label">filled</span></div>
            <div class="stat"><span class="stat-num warn">${formatNumber(profile.nullCount)}</span><span class="stat-label">empty</span></div>
            <div class="stat"><span class="stat-num accent">${formatNumber(profile.distinctCount)}</span><span class="stat-label">distinct</span></div>
          </div>
          ${(profile.warnings ?? []).map((warning) => `<p class="modal-desc">${escapeHtml(warning)}</p>`).join("")}
          <div class="minmax">
            <div><span>min</span><strong>${profile.min.kind === "null" ? "—" : escapeHtml(clipped(cellText(profile.min)))}</strong></div>
            <div><span>max</span><strong>${profile.max.kind === "null" ? "—" : escapeHtml(clipped(cellText(profile.max)))}</strong></div>
          </div>
          <div class="tops-title">Top values</div>
          <div class="tops">
            ${profile.topValues.map((t) => `
              <button class="top-row" data-action="filter-top-value" data-col="${escapeHtml(col)}" data-value="${escapeHtml(cellText(t.value))}" data-null="${t.value.kind === "null" ? "1" : ""}" ${isTruncated(t.value) || t.value.kind === "binary" ? "disabled" : ""} title="Filter to this value">
                <span class="top-value">${t.value.kind === "null" ? "<i>NULL</i>" : escapeHtml(clipped(cellText(t.value), 80))}</span>
                <span class="top-bar"><span style="width:${Math.max(3, Math.round((Number(t.count) / maxCount) * 100))}%"></span></span>
                <span class="top-count">${formatNumber(t.count)}</span>
              </button>`).join("") || `<div class="side-empty">No values.</div>`}
          </div>` : ""}
      </div>
    </aside>`;
}

function renderFileInfoModal(tab: DatasetTab | null): string {
  if (!fileInfoOpen || !tab) return "";
  const info = tab.info;
  return `
    <div class="overlay" data-action="close-modals">
      <div class="modal modal-sm" role="dialog" aria-modal="true" data-modal-card>
        <div class="modal-head">
          <div class="modal-title">${icon("info")}<span>File details</span></div>
          <button class="icon-btn" data-action="close-file-info" title="Close">${icon("close")}</button>
        </div>
        <div class="modal-body">
          <div class="file-title">${icon("file")}<strong>${escapeHtml(info.fileName)}</strong></div>
          <div class="file-path">${escapeHtml(info.path)}</div>
          <p class="modal-desc">${info.format === "parquet" ? `Parquet version ${info.formatVersion} · ${info.rowGroups.length} row groups${info.createdBy ? ` · ${escapeHtml(info.createdBy)}` : ""}` : info.sqlite ? `SQLite ${escapeHtml(info.sqlite.database.sqliteVersion)} database · ${escapeHtml(info.sqlite.object)} · journal mode ${escapeHtml(info.sqlite.database.journalMode)} · read-only, live` : info.excel ? `${escapeHtml(info.excel.workbook.variant.toUpperCase())} workbook · sheet ${escapeHtml(info.excel.sheet)} · ${info.excel.headerRow === null ? "no header row" : `header row ${info.excel.headerRow}`} · ${info.excel.workbook.date1904 ? "1904" : "1900"} date system` : info.format === "json" || info.format === "jsonl" ? `${info.document ? "JSON document" : info.format === "json" ? "JSON array of records" : "JSON Lines records"} · UTF-8 · fully validated when opened` : `${info.format.toUpperCase()} · UTF-8 · frozen parsing settings`}</p>
          ${info.document ? `<p class="modal-desc">${escapeHtml(info.document.reason)}</p>` : ""}
          ${info.object.warnings.map((warning) => `<p class="modal-desc muted">${escapeHtml(warning)}</p>`).join("")}
          <div class="modal-actions-row">
            <button class="btn-ghost btn-sm" data-action="reveal-file">${icon("folder")}<span>Reveal</span></button>
            <button class="btn-ghost btn-sm" data-action="open-external">${icon("external")}<span>Open externally</span></button>
          </div>
          <div class="stat-grid stat-grid-3">
            ${info.document ? `<div class="stat"><span class="stat-num">${formatNumber(info.document.childCount)}</span><span class="stat-label">top-level ${info.document.rootKind === "object" ? "fields" : "items"}</span></div><div class="stat"><span class="stat-num">${escapeHtml(info.document.rootKind)}</span><span class="stat-label">root</span></div>` : `
            <div class="stat"><span class="stat-num">${formatCount(info.object.rowCount)}</span><span class="stat-label">rows</span></div>
            <div class="stat"><span class="stat-num">${info.object.columns.length}</span><span class="stat-label">columns</span></div>`}
            <div class="stat"><span class="stat-num">${formatBytes(info.fileSize)}</span><span class="stat-label">size</span></div>
          </div>
        </div>
      </div>
    </div>`;
}

function renderExportModal(tab: DatasetTab | null): string {
  if (!exportModalOpen || !tab) return "";
  const filtered = tab.pageQuery?.filter.kind === "all" && tab.pageQuery.filter.terms.length > 0 || !!tab.pageQuery?.quickSearch.trim();
  return `
    <div class="overlay" data-action="close-modals">
      <div class="modal modal-sm" role="dialog" aria-modal="true" data-modal-card>
        <div class="modal-head">
          <div class="modal-title">${icon("download")}<span>Export</span></div>
          <button class="icon-btn" data-action="close-modals">${icon("close")}</button>
        </div>
        <div class="modal-body">
          <p class="modal-desc">Exports the loaded view: <strong>${tab.pageColumns.length} visible columns</strong>${filtered ? " with your <strong>filters applied</strong>" : ""}${tab.sortColumn ? ` ordered by <strong>${escapeHtml(columnLabel(tab, tab.sortColumn))}</strong>` : ""}.</p>
          <label class="field-label">Format</label>
          <div class="export-formats" role="radiogroup" aria-label="Export format">
            ${EXPORT_FORMATS.map((f) => `
              <label class="export-format ${exportFormat === f.value ? "selected" : ""}">
                <input type="radio" name="export-format" value="${f.value}" data-change="export-format" ${exportFormat === f.value ? "checked" : ""} />
                <span>${f.label}</span>${f.value === "parquet" ? `<em class="default-tag">default</em>` : ""}
              </label>`).join("")}
          </div>
          ${(exportFormat === "csv" || exportFormat === "tsv") ? `<p class="modal-desc">NULL is written as \\N; literal values matching this marker are rejected. Binary cells use hex:-prefixed text.</p>` : (exportFormat === "json" || exportFormat === "jsonl") ? `<p class="modal-desc">${exportFormat === "jsonl" ? "Writes one JSON object per line." : "Writes one JSON array."} Binary cells use tagged hex objects. Non-finite numbers require Parquet.</p>` : ""}
          ${tab.info.format === "json" || tab.info.format === "jsonl" ? `<p class="modal-desc">JSON/JSONL output writes exact numeric tokens and nested values unquoted. Parquet refuses JSON_NUMBER columns; nested values become JSON text in Parquet, CSV and TSV.</p>` : ""}
          <label class="field-label" for="export-max">Max rows</label>
          <input id="export-max" type="number" min="1" max="${tab.info.policy.maxExportRows}" value="${exportMaxRows}" />
          <p class="modal-desc muted">File name will default to include the current date-time, e.g. <strong>${escapeHtml(defaultExportFileName(tab.info.fileName, exportFormat))}</strong>.</p>
          ${lastExport ? `
          <div class="export-done">
            <div class="export-done-title">${icon("check")} Wrote ${formatNumber(lastExport.exportedRows)} rows${lastExport.truncated ? " · row cap reached" : ""}</div>
            <div class="export-done-path">${escapeHtml(lastExport.destination)}</div>
            ${lastExport.warnings.map((warning) => `<p class="modal-desc">${escapeHtml(warning)}</p>`).join("")}
            <div class="modal-actions-row">
              <button class="btn-ghost btn-sm" data-action="reveal-export">${icon("folder")}<span>Reveal</span></button>
              <button class="btn-ghost btn-sm" data-action="open-export-file">${icon("external")}<span>Open</span></button>
            </div>
          </div>` : ""}
        </div>
        <div class="modal-foot">
          <button class="btn-ghost" data-action="close-modals">Cancel</button>
          <button class="btn-primary" data-action="run-export" ${exportBusy ? "disabled" : ""}>${exportBusy ? `<span class="spinner"></span><span>Exporting…</span>` : `${icon("download")}<span>Choose location & export</span>`}</button>
        </div>
      </div>
    </div>`;
}

function renderFormatPrompt(): string {
  if (!formatPrompt) return "";
  const target = formatPrompt.format === "jsonl" ? "JSON Lines" : "a JSON document";
  return `<div class="overlay" role="dialog" aria-modal="true" aria-labelledby="format-title"><div class="modal modal-sm" data-modal-card>
    <div class="modal-head"><div class="modal-title"><span id="format-title">Format mismatch</span></div><button class="icon-btn" data-action="cancel-format" aria-label="Close">${icon("close")}</button></div>
    <div class="modal-body"><p class="modal-desc">${escapeHtml(formatPrompt.message)}</p><p class="modal-desc">Nothing was reinterpreted. You can reopen ${escapeHtml(baseName(formatPrompt.path))} explicitly as ${target}; that reader validates the whole file again.</p></div>
    <div class="modal-foot"><button class="btn-ghost" data-action="cancel-format">Cancel</button><button class="btn-primary" data-action="reopen-format">Reopen as ${target}</button></div>
  </div></div>`;
}

function renderToasts(): void {
  const host = document.getElementById("toasts");
  if (!host) return;
  host.innerHTML = toasts
    .map(
      (t) => `
      <div class="toast ${t.kind}">
        <span class="toast-ic">${icon(t.kind === "success" ? "check" : t.kind === "error" ? "x-circle" : "info")}</span>
        <span>${escapeHtml(t.message)}</span>
        <button data-action="dismiss-toast" data-id="${t.id}">${icon("close")}</button>
      </div>`
    )
    .join("");
}

function render(): void {
  const app = document.getElementById("app");
  if (!app) return;
  // Preserve focus + grid scroll across re-renders.
  // NOTE: during loading there is no #grid-scroll element — keep the last
  // known offsets instead of resetting to 0, so sorting/paging doesn't
  // jump the horizontal scrollbar back to the start.
  const active = document.activeElement as HTMLInputElement | null;
  const focusId = active?.id || null;
  const selStart = active?.selectionStart ?? null;
  const selEnd = active?.selectionEnd ?? null;
  const grid = document.getElementById("grid-scroll");
  if (grid) {
    lastGridScroll = { top: grid.scrollTop, left: grid.scrollLeft };
  }

  const tab = activeTab();
  app.innerHTML = `
    ${renderTitlebar()}
    <div class="shell">
      ${renderSidebar(tab)}
      ${renderMain(tab)}
      ${renderDrawer(tab)}
    </div>
    ${renderFileInfoModal(tab)}
    ${renderDelimited(delimitedDialog)}
    ${renderFormatPrompt()}
    ${renderExcel(excelDialog)}
    ${renderSqlite(sqliteDialog)}
    ${renderExportModal(tab)}
    <div id="toasts" class="toasts"></div>`;

  renderToasts();
  if (focusId) {
    const next = document.getElementById(focusId) as HTMLInputElement | null;
    if (next) {
      next.focus();
      try {
        if (selStart !== null && next.type !== "number") next.setSelectionRange(selStart, selEnd ?? selStart);
      } catch {
        /* ignore */
      }
    }
  }
  const nextGrid = document.getElementById("grid-scroll");
  if (nextGrid) {
    nextGrid.scrollTop = lastGridScroll.top;
    nextGrid.scrollLeft = lastGridScroll.left;
  }
}

// ---------------------------------------------------------------------------
// Events (delegation — the DOM is fully re-rendered on state changes)
// ---------------------------------------------------------------------------

const debouncedColumnSearch = debounce(() => render(), 200);

async function handleAction(el: HTMLElement): Promise<void> {
  const action = el.dataset.action;
  if (!action) return;
  const tab = activeTab();

  switch (action) {
    case "parsing-settings": if (tab) await openDelimited(tab.info.path); break;
    case "preview-delimited": await previewDelimited(); break;
    case "confirm-delimited": await confirmDelimited(); break;
    case "cancel-delimited": cancelDelimited(); render(); break;
    case "reopen-format": {
      const prompt = formatPrompt; formatPrompt = null; render();
      if (prompt) await openFileByPath(prompt.path, prompt.format);
      break;
    }
    case "cancel-format": formatPrompt = null; render(); break;
    case "excel-sheets": if (tab) await openExcel(tab.info.path); break;
    case "sqlite-objects": if (tab) await openSqlite(tab.info.path); break;
    case "confirm-sqlite": await confirmSqlite(); break;
    case "cancel-sqlite": cancelSqlite(); render(); break;
    case "confirm-excel": await confirmExcel(); break;
    case "cancel-excel": cancelExcel(); render(); break;
    case "doc-toggle": {
      const key = el.dataset.node ?? "", state = tab?.doc;
      if (!tab || !state) break;
      if (state.expanded.delete(key)) { render(); break; }
      state.expanded.add(key);
      if (state.branches.get(key)?.node) render(); else await loadDocument(tab, key);
      break;
    }
    case "doc-more": {
      const key = el.dataset.node ?? "", branch = tab?.doc?.branches.get(key);
      if (tab && branch) await loadDocument(tab, key, branch.children.length);
      break;
    }
    case "doc-copy": if (tab) await copyDocument(tab, el.dataset.node ?? ""); break;
    case "load-raw-record": if (tab) await loadRawRecord(tab); break;
    case "copy-raw-record": if (tab?.rawRecord?.text) await copyText(tab.rawRecord.text, "Source record copied"); break;
    case "win-min":
      await invoke("window_minimize").catch(() => undefined);
      break;
    case "win-max":
      await invoke("window_maximize").catch(() => undefined);
      break;
    case "win-close":
      await invoke("window_close").catch(() => undefined);
      break;
    case "open-file":
      await chooseAndOpenFile();
      break;
    case "make-sample":
      await createSampleFile();
      break;
    case "open-recent":
      if (el.dataset.path) await openFileByPath(el.dataset.path);
      break;
    case "clear-recents":
      recents = [];
      persistRecents();
      render();
      break;
    case "switch-tab":
      if (el.dataset.path && el.dataset.path !== activePath) {
        activePath = el.dataset.path;
        exportModalOpen = false; lastExport = null;
        const next = activeTab();
        if (next) {
          newFilter = { column: next.info.object.columns[0]?.id || "", op: defaultFilterOp(next.info.object.columns[0]), value: "" };
          filterError = "";
          filterValueChosen = false;
          filterPanelOpen = false;
          render();
          if (isDocument(next) ? !next.doc : !next.page && !next.loading) await loadInitial(next);
        }
      }
      break;
    case "close-tab": {
      const path = el.dataset.path;
      if (!path) break;
      const closing = getTab(path);
      if (closing) { closing.closed = true; invalidate(closing); void datasets.close(closing.info.sessionId).catch(() => undefined); }
      exportModalOpen = false; lastExport = null;
      tabs = tabs.filter((t) => t.info.path !== path);
      if (activePath === path) {
        activePath = tabs.length ? tabs[tabs.length - 1].info.path : null;
        const next = activeTab();
        newFilter = { column: next?.info.object.columns[0]?.id || "", op: defaultFilterOp(next?.info.object.columns[0]), value: "" };
        filterError = "";
        filterValueChosen = false;
        filterPanelOpen = false;
      }
      render();
      break;
    }
    case "toggle-column":
      if (tab && el.dataset.col) {
        const col = el.dataset.col;
        if (tab.visibleColumns.includes(col)) {
          if (tab.visibleColumns.length <= 1) {
            showToast("Keep at least one column visible", "info");
            break;
          }
          tab.visibleColumns = tab.visibleColumns.filter((c) => c !== col);
          if (tab.sortColumn === col) tab.sortColumn = null;
        } else {
          const order = tab.info.object.columns.map((c) => c.id);
          tab.visibleColumns = [...tab.visibleColumns, col].sort(
            (a, b) => order.indexOf(a) - order.indexOf(b)
          );
        }
        tab.currentPage = 1;
        render();
        await loadPage(tab.info.path);
      }
      break;
    case "columns-reset":
      if (tab) {
        tab.visibleColumns = tab.info.object.columns.map((c) => c.id);
        tab.currentPage = 1;
        render();
        await loadPage(tab.info.path);
      }
      break;
    case "sort-col":
      if (tab && el.dataset.col) {
        const col = el.dataset.col;
        if (tab.sortColumn !== col) {
          tab.sortColumn = col;
          tab.sortDesc = false;
        } else if (!tab.sortDesc) {
          tab.sortDesc = true;
        } else {
          tab.sortColumn = null;
          tab.sortDesc = false;
        }
        tab.currentPage = 1;
        await loadPage(tab.info.path);
      }
      break;
    case "toggle-filters":
      filterPanelOpen = !filterPanelOpen;
      render();
      break;
    case "add-filter": {
      if (!tab) break;
      const column = newFilter.column || tab.info.object.columns[0]?.id || "";
      if (!column) break;
      const info = columnById(tab, column);
      if (!info) break;
      const input = document.getElementById("nf-value") as HTMLInputElement | HTMLSelectElement | null;
      const error = input instanceof HTMLSelectElement && !input.value ? "Choose a filter value first." : validateFilter(info, { ...newFilter, column }, tab.info.format === "sqlite");
      setFilterError(error || "");
      if (error || (input && !input.checkValidity())) {
        if (!error && input) setFilterError(input.validationMessage);
        input?.reportValidity();
        break;
      }
      // Stage only — nothing is queried until Apply is clicked.
      tab.filters.push({ column, op: newFilter.op, value: newFilter.value });
      newFilter = { column, op: defaultFilterOp(info), value: "" };
      filterValueChosen = false;
      tab.currentPage = 1;
      render();
      break;
    }
    case "apply-filters": {
      if (!tab) break;
      // Explicit Apply: never stages the pending builder row — it only
      // queries with the already-added filter chips + search text.
      // Warn when a pending builder value would otherwise be silently ignored
      // (the common "typed values but forgot +" confusion).
      const pendingColumn = newFilter.column || tab.info.object.columns[0]?.id || "";
      if (!!pendingColumn && (filterValueChosen || hasFilterValue(newFilter.op, newFilter.value))) {
        showToast("Pending filter not added — click + to add it first", "info");
      }
      tab.currentPage = 1;
      await loadPage(tab.info.path);
      break;
    }
    case "clear-filter-builder": {
      if (!tab) break;
      newFilter = {
        column: tab.info.object.columns[0]?.id || newFilter.column,
        op: defaultFilterOp(tab.info.object.columns[0]),
        value: "",
      };
      filterError = "";
      filterValueChosen = false;
      render();
      break;
    }
    case "remove-filter":
      if (tab) {
        const idx = Number(el.dataset.index);
        tab.filters = tab.filters.filter((_, i) => i !== idx);
        tab.currentPage = 1;
        render();
      }
      break;
    case "edit-filter": {
      if (!tab) break;
      const idx = Number(el.dataset.index);
      const f = tab.filters[idx];
      if (!f) break;
      // Load the chip back into the builder for editing; drop the original
      // so clicking + re-stages the edited version. Staged only — Apply runs it.
      const colExists = tab.info.object.columns.some((c) => c.id === f.column);
      newFilter = {
        column: colExists ? f.column : tab.info.object.columns[0]?.id || "",
        op: f.op,
        value: f.value,
      };
      filterError = "";
      filterValueChosen = true;
      tab.filters = tab.filters.filter((_, i) => i !== idx);
      tab.currentPage = 1;
      filterPanelOpen = true;
      render();
      const focusEl =
        (opNeedsValue(newFilter.op) && document.getElementById("nf-value")) ||
        document.getElementById("nf-op");
      (focusEl as HTMLInputElement | HTMLSelectElement | null)?.focus?.();
      break;
    }
    case "clear-search":
      if (tab) {
        tab.quickSearch = "";
        tab.currentPage = 1;
        render();
      }
      break;
    case "reset-view":
      if (tab) {
        tab.filters = [];
        tab.quickSearch = "";
        tab.sortColumn = null;
        tab.sortDesc = false;
        tab.currentPage = 1;
        filterPanelOpen = false;
        await loadPage(tab.info.path);
      }
      break;
    case "goto-page":
      if (tab) {
        const total = safeCount(tab.page?.count ?? { status: "unknown" });
        const totalPages = total === null ? Number.MAX_SAFE_INTEGER : Math.max(1, Math.ceil(total / tab.pageSize));
        tab.currentPage = Math.min(Math.max(1, Number(el.dataset.page) || 1), totalPages);
        await loadPage(tab.info.path);
      }
      break;
    case "refresh-source":
      if (tab) await refreshTab(tab);
      break;
    case "count-rows":
      if (tab) await loadCount(tab);
      break;
    case "cancel-work":
      if (tab) for (const operation of tab.operations) void operation.cancel();
      break;
    case "retry-page":
      if (tab) await loadPage(tab.info.path);
      break;
    case "select-row":
      if (tab) {
        const ri = Number(el.dataset.row);
        tab.selectedRow = tab.selectedRow === ri ? null : ri; tab.rawRecord = null;
        if (tab.selectedRow !== null) {
          ++tab.profileRequestId;
          // Row and column panels are exclusive — no tabs.
          tab.profileColumn = null;
          tab.profile = null;
          tab.profileError = null;
          tab.profileLoading = false;
        }
        render();
      }
      break;
    case "close-drawer":
      if (tab) {
        ++tab.profileRequestId;
        tab.selectedRow = null;
        tab.profileColumn = null;
        tab.profile = null;
        render();
      }
      break;
    case "show-profile":
      if (tab && el.dataset.col) {
        await loadProfile(tab, el.dataset.col);
      }
      break;
    case "filter-top-value":
      if (tab && el.dataset.col) {
        const isNull = el.dataset.null === "1";
        tab.filters.push({
          column: el.dataset.col,
          op: isNull ? "is_null" : columnById(tab, el.dataset.col)?.operators.includes("eq") ? "eq" : "contains",
          value: isNull ? "" : el.dataset.value || "",
        });
        tab.currentPage = 1;
        filterPanelOpen = true;
        render();
        showToast("Filter added — click Apply to run it", "info");
      }
      break;
    case "expand-cell":
    case "copy-cell": {
      if (!tab || tab.selectedRow === null || !tab.page || tab.stale) break;
      const ri = Number(el.dataset.row), ci = Number(el.dataset.ci), page = tab.page;
      const rowIdx = Number.isFinite(ri) ? ri : tab.selectedRow;
      let value = page.rows[rowIdx]?.[ci]; if (!value) break;
      try {
        if (isTruncated(value)) {
          const cells = await fullCells(tab, rowIdx, [tab.pageColumns[ci]]); if (!cells || tab.page !== page) break;
          value = cells[0]; page.rows[rowIdx][ci] = value;
        }
        if (action === "copy-cell") await copyText(cellForCopy(value)); else render();
      } catch (error) { showToast(failTab(tab, error), "error"); }
      break;
    }
    case "copy-row-json": {
      if (!tab || tab.selectedRow === null || !tab.page || tab.stale) break;
      const page = tab.page, selected = tab.selectedRow;
      let row = page.rows[selected];
      try {
        if (row.some(isTruncated)) {
          const cells = await fullCells(tab, selected, tab.pageColumns); if (!cells || tab.selectedRow !== selected) break;
          row = cells; page.rows[selected] = cells;
        }
        await copyText(rowJson(tab, row), "Row copied as JSON");
      } catch (error) { showToast(failTab(tab, error), "error"); }
      break;
    }
    case "json-more": {
      const details = el.closest<HTMLDetailsElement>("details.json-node");
      if (details) populateJsonNode(details, true);
      break;
    }
    case "open-file-info": {
      const targetPath = el.dataset.path || null;
      if (targetPath && targetPath !== activePath && getTab(targetPath)) {
        activePath = targetPath;
        const next = activeTab();
        if (next) {
          newFilter = { column: next.info.object.columns[0]?.id || "", op: defaultFilterOp(next.info.object.columns[0]), value: "" };
          filterError = "";
          filterValueChosen = false;
        }
      }
      if (!activeTab()) break;
      fileInfoOpen = true;
      render();
      break;
    }
    case "open-export":
      lastExport = null;
      exportFormat = "parquet";
      exportModalOpen = true;
      render();
      break;
    case "run-export":
      await runExport();
      break;
    case "reveal-file":
      if (tab) {
        try {
          await invoke("reveal_path", { path: tab.info.path });
        } catch (e) {
          showToast(String(e), "error");
        }
      }
      break;
    case "open-external":
      if (tab) {
        try {
          await invoke("open_external", { path: tab.info.path });
        } catch (e) {
          showToast(String(e), "error");
        }
      }
      break;
    case "reveal-export":
      if (lastExport) {
        try {
          await invoke("reveal_path", { path: lastExport.destination });
        } catch (e) {
          showToast(String(e), "error");
        }
      }
      break;
    case "open-export-file":
      if (lastExport) {
        try {
          await invoke("open_external", { path: lastExport.destination });
        } catch (e) {
          showToast(String(e), "error");
        }
      }
      break;
    case "close-modals":
    case "close-file-info":
      fileInfoOpen = false;
      exportModalOpen = false;
      render();
      break;
    case "dismiss-toast": {
      const id = Number(el.dataset.id);
      toasts = toasts.filter((t) => t.id !== id);
      renderToasts();
      break;
    }
  }
}

function setupEvents(): void {
  const app = document.getElementById("app");
  if (!app) return;

  app.addEventListener("click", (e) => {
    const target = (e.target as HTMLElement).closest("[data-action]") as HTMLElement | null;
    if (!target || !app.contains(target)) return;
    // Clicks on the overlay backdrop (outside the modal card) close the modal.
    // Clicks inside the card must NOT bubble up to the overlay handler:
    // only treat it as an overlay click when the action element itself is
    // the overlay. Explicit close buttons (close-modals / close-file-info)
    // always run.
    if (
      target.dataset.action === "close-modals" &&
      target.classList.contains("overlay") &&
      (e.target as HTMLElement).closest("[data-modal-card]")
    ) {
      return;
    }
    // Nested action (tab close inside tab switch): run the innermost only.
    e.stopPropagation();
    void handleAction(target);
  });

  app.addEventListener("input", (e) => {
    const target = e.target as HTMLInputElement;
    const tab = activeTab();
    if (target.dataset.delimitedType) { changeDelimited("", target.value, target.dataset.delimitedType); return; }
    if (target.dataset.delimited && target.tagName !== "SELECT") { changeDelimited(target.dataset.delimited, target.value); return; }
    if (target.dataset.excel === "headerRow") { changeExcel("headerRow", target.value); return; }
    if (target.id === "quick-search" && tab) {
      // Staged — no query while typing. Click Apply to run.
      tab.quickSearch = target.value;
      tab.currentPage = 1;
    } else if (target.id === "column-search") {
      columnSearch = target.value;
      debouncedColumnSearch();
    } else if (target.id === "nf-value") {
      if (target instanceof HTMLSelectElement) return;
      newFilter.value = target.value;
      filterValueChosen = true;
      setFilterError("");
    } else if (target.id === "export-max") {
      exportMaxRows = Number(target.value) || exportMaxRows;
    }
  });

  app.addEventListener("change", (e) => {
    const target = e.target as HTMLInputElement | HTMLSelectElement;
    const kind = (target as HTMLElement).dataset.change;
    if (target.dataset.delimited) { changeDelimited(target.dataset.delimited, target.value); return; }
    if (target.dataset.excel) { changeExcel(target.dataset.excel, target.value); return; }
    if (target.dataset.sqlite) { changeSqlite(target.value); return; }
    if (!kind) return;
    const tab = activeTab();
    if (kind === "nf-column") {
      newFilter.column = target.value;
      newFilter.op = defaultFilterOp(tab ? columnById(tab, target.value) : undefined);
      newFilter.value = "";
      filterError = "";
      filterValueChosen = false;
      render();
    } else if (kind === "nf-op") {
      newFilter.op = target.value as FilterOp;
      newFilter.value = "";
      filterError = "";
      filterValueChosen = false;
      render();
    } else if (kind === "nf-value-choice") {
      newFilter.value = target.value ? JSON.parse(target.value) as string : "";
      filterValueChosen = !!target.value;
      setFilterError("");
    } else if (kind === "export-format") {
      const v = target.value as ExportFormat;
      if (EXPORT_FORMATS.some(f => f.value === v)) {
        exportFormat = v;
        render();
      }
    } else if (kind === "page-size" && tab) {
      tab.pageSize = Number(target.value) || DEFAULT_PAGE_SIZE;
      tab.currentPage = 1;
      void loadPage(tab.info.path);
    }
  });

  app.addEventListener("toggle", (e) => {
    const details = e.target;
    if (details instanceof HTMLDetailsElement && details.open && details.classList.contains("json-node")) populateJsonNode(details);
  }, true);

  app.addEventListener("keydown", (e) => {
    const target = e.target as HTMLElement;
    const typing = target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.tagName === "SELECT";
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "o") {
      e.preventDefault();
      void chooseAndOpenFile();
    } else if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
      e.preventDefault();
      document.getElementById("quick-search")?.focus();
    } else if (e.key === "Escape") {
      if (sqliteDialog) { cancelSqlite(); render(); }
      else if (excelDialog) { cancelExcel(); render(); }
      else if (formatPrompt) { formatPrompt = null; render(); }
      else if (delimitedDialog) { cancelDelimited(); render(); }
      else if (fileInfoOpen || exportModalOpen) {
        fileInfoOpen = false;
        exportModalOpen = false;
        render();
      } else {
        const tab = activeTab();
        if (tab && (tab.selectedRow !== null || tab.profileColumn !== null)) {
          tab.selectedRow = null;
          tab.profileColumn = null;
          render();
        }
      }
    } else if (e.key === "Enter" && !typing) {
      /* noop */
    }
  });

  // Desktop drag & drop of supported table files.
  getCurrentWebview()
    .onDragDropEvent((event) => {
      if (event.payload.type === "drop" && event.payload.paths.length > 0) {
        const first = event.payload.paths[0];
        if (/\.(parquet|pq|csv|tsv|json|jsonl|ndjson|xlsx|xlsm|xlsb|xls|sqlite3|sqlite|db)$/i.test(first)) {
          void openFileByPath(first);
        } else {
          showToast("Choose a Parquet, CSV, TSV, JSON, JSON Lines, Excel or SQLite file", "error");
        }
      }
    })
    .catch(() => undefined);
}

// ---------------------------------------------------------------------------
// Init
// ---------------------------------------------------------------------------

function init(): void {
  render();
  setupEvents();
  // If launched with a file path (e.g. "open with"), Tauri passes it as argv —
  // best effort: try to surface it via recents. Direct argv handling happens
  // in the opener flow, so here we simply present the hero state.
}

document.addEventListener("DOMContentLoaded", init);
if (document.readyState !== "loading") init();
