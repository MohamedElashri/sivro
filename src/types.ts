export type FormatKind = "parquet" | "csv" | "tsv" | "json" | "jsonl" | "excel" | "sqlite";
export type ColumnKind = "number" | "text" | "bool" | "date" | "nested" | "binary" | "other";
export type FilterOp = "eq" | "ne" | "gt" | "lt" | "gte" | "lte" | "contains" | "not_contains" | "contains_any" | "not_contains_any" | "starts_with" | "ends_with" | "is_null" | "is_not_null";
// `sortable` is false for raw JSON tokens/values, whose text order is not value order.
export interface ColumnInfo { id: string; name: string; sqlName: string; dataType: string; nullable: boolean; kind: ColumnKind; operators: FilterOp[]; sortable?: boolean }
export type CountState = { status: "known"; value: string } | { status: "unknown" } | { status: "pending" } | { status: "failed"; reason: string };
export type Cell =
  | { kind: "null" }
  | { kind: "boolean"; value: boolean }
  | { kind: "number"; value: string; logicalType: string; byteLength: string; truncated: boolean }
  | { kind: "text" | "json"; value: string; byteLength: string; truncated: boolean }
  | { kind: "binary"; byteCount: string; previewHex: string; truncated: boolean };
export interface Capabilities { page: boolean; filter: boolean; sort: boolean; profile: boolean; exactCount: boolean; copy: boolean; exports: string[] }
export interface ObjectDescriptor { id: string; label: string; kind: "table" | "document" | "scalar"; columns: ColumnInfo[]; rowCount: CountState; capabilities: Capabilities; warnings: string[] }
export interface ResourcePolicy {
  defaultPageSize: number; maxPageSize: number; schemaSampleRows: number;
  jsonDocumentBytes: number; jsonDepth: number; jsonChildren: number; jsonlRecordBytes: number;
  cellPreviewBytes: number; fullCellBytes: number; blobPreviewBytes: number;
  excelWorkbookBytes: number; xlsWorkbookBytes: number; excelPayloadBytes: number; excelEntryBytes: number; excelCells: number;
  scratchBytes: number; duckdbMemoryBytes: number; expensiveOperations: number; timeoutMs: number;
  defaultExportRows: number; maxExportRows: number; maxSessions: number; maxPendingOperations: number;
  maxColumns: number; maxFilterNodes: number; maxFilterDepth: number; maxValueBytes: number; maxSearchBytes: number;
  maxPageBytes: number; maxCacheEntries: number; diskReserveBytes: number;
}
export interface DatasetInfo {
  apiVersion: 1; sessionId: string; generation: number; format: FormatKind;
  path: string; fileName: string; fileSize: string; object: ObjectDescriptor;
  rowGroups: { index: number; numRows: string; bytes: string; numColumns: number }[];
  createdBy: string | null; formatVersion: number; policy: ResourcePolicy; delimited?: FrozenDelimitedOptions | null;
  document?: DocumentInfo | null;
  excel?: ExcelSelection | null;
  sqlite?: SqliteSelection | null;
}
export interface DbObjectInfo { name: string; kind: "table" | "view" | "virtual"; columns: number; withoutRowid: boolean; selectable: boolean; reason: string | null }
export interface DatabaseInfo { sqliteVersion: string; journalMode: string; objects: DbObjectInfo[]; warnings: string[] }
export interface SqliteSelection { object: string; database: DatabaseInfo }
export interface ExcelOptions { sheet: string; headerRow: number | null }
export interface SheetInfo { name: string; kind: "worksheet" | "chartsheet" | "dialogsheet" | "macrosheet" | "other"; visibility: "visible" | "hidden" | "very_hidden"; selectable: boolean }
export interface WorkbookInfo { variant: string; date1904: boolean; sheets: SheetInfo[]; warnings: string[] }
export interface ExcelSelection { sheet: string; headerRow: number | null; workbook: WorkbookInfo }
export type NodeKind = "object" | "array" | "string" | "number" | "boolean" | "null";
export interface DocumentInfo { rootKind: NodeKind; childCount: string; reason: string }
// Numbers arrive as exact source tokens in `value`; never parse them into JS numbers.
export interface DocumentNode { index: number; key: string | null; keyTruncated: boolean; kind: NodeKind; value: string | null; byteLength: string; truncated: boolean; childCount: string | null }
export interface DocumentResult extends ScopedResult { path: number[]; node: DocumentNode; children: DocumentNode[]; offset: number; hasMore: boolean; text: string | null }
// Local form state uses a column ID, never the display name, in `column`.
export interface Filter { column: string; op: FilterOp; value: string }
export type FilterValue = { kind: "none" } | { kind: "text" | "number" | "temporal" | "json"; value: string } | { kind: "boolean"; value: boolean };
export type FilterExpr = { kind: "all" | "any"; terms: FilterExpr[] } | { kind: "predicate"; columnId: string; op: FilterOp; value: FilterValue };
export interface QueryArgs { columnIds: string[]; offset: number; pageSize: number; filter: FilterExpr; quickSearch: string; sort: { columnId: string; descending: boolean } | null }
export interface ScopedResult { sessionGeneration: number; objectId: string }
export interface PageResult extends ScopedResult { columnIds: string[]; rowIds: string[]; rows: Cell[][]; offset: number; limit: number; hasMore: boolean; count: CountState; queryMs: number }
export interface CountResult extends ScopedResult { count: CountState }
export interface ColumnProfile extends ScopedResult { columnId: string; scope: "current_view"; nullCount: string; nonNullCount: string; distinctCount: string; min: Cell; max: Cell; topValues: { value: Cell; count: string }[]; warnings?: string[] }
export interface RowResult extends ScopedResult { rowId: string; columnIds: string[]; cells: Cell[]; rawRecord?: string | null }
export type ExportFormat = "csv" | "tsv" | "json" | "jsonl" | "parquet";
export interface ExportResult extends ScopedResult { destination: string; exportedRows: string; truncated: boolean; warnings: string[] }
export interface AppError { code: string; message: string }

export interface DelimitedOptions {
  delimiter?: string; header?: boolean; quote?: string; escape?: string;
  nullMarker?: string; forceText?: boolean; typeOverrides?: Record<string, string>;
}
export interface FrozenDelimitedOptions {
  delimiter: string; header: boolean; quote: string; escape: string; nullMarker: string; forceText: boolean; typeOverrides: Record<string, string>;
}
export interface DelimitedPreview {
  token: string; format: "csv" | "tsv"; options: FrozenDelimitedOptions;
  columns: ColumnInfo[]; rows: Cell[][]; sampledRows: number; hasMore: boolean; warnings: string[];
}
