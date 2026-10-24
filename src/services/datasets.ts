import { invoke } from "@tauri-apps/api/core";
import type { AppError, DatasetInfo, ScopedResult, QueryArgs, PageResult, CountResult, ColumnProfile, RowResult, ExportResult, ExportFormat, DelimitedOptions, DelimitedPreview, DocumentResult, FormatKind, ExcelOptions, WorkbookInfo, DatabaseInfo, Hdf5Options, Hdf5Info } from "../types";

export function errorInfo(error: unknown): AppError {
  if (error && typeof error === "object" && "message" in error) {
    return { code: "code" in error ? String(error.code) : "unknown", message: String(error.message) };
  }
  return { code: "unknown", message: String(error) };
}
export function reference(info: DatasetInfo) { return { sessionId: info.sessionId, generation: info.generation, objectId: info.object.id }; }
export function matches(result: ScopedResult, info: DatasetInfo): boolean { return result.sessionGeneration === info.generation && result.objectId === info.object.id; }
export type OperationKind = "page" | "count" | "profile" | "read_row" | "export" | "document";
export interface OperationInputs {
  page: QueryArgs;
  count: QueryArgs;
  profile: { columnId: string; context: QueryArgs };
  read_row: { rowId: string; columnIds: string[]; rawRecord?: boolean };
  export: { destination: string; format: ExportFormat; maxRows: number; view: QueryArgs };
  document: { path: number[]; offset: number; limit: number; mode: "children" | "copy" };
}
export interface OperationOutputs { page: PageResult; count: CountResult; profile: ColumnProfile; read_row: RowResult; export: ExportResult; document: DocumentResult }
export function startOperation<K extends OperationKind>(info: DatasetInfo, kind: K, query: OperationInputs[K], changed: () => void): DatasetOperation<OperationOutputs[K]> {
  return new DatasetOperation<OperationOutputs[K]>(info, kind, query, changed);
}
export type OperationStatus = "queued" | "running" | "cancel_pending" | "finished";
const commands: Record<OperationKind, string> = { page: "fetch_page", count: "count_rows", profile: "column_profile", read_row: "read_row", export: "export_data", document: "read_document" };

// The backend owns operation IDs. Reserving before submission makes a cancel
// during admission/IPC setup safe, including when it precedes the native query.
export class DatasetOperation<T> {
  readonly result: Promise<T>;
  status: OperationStatus = "queued";
  private id: string | null = null;
  private cancelled = false;
  constructor(info: DatasetInfo, kind: OperationKind, query: unknown, private changed: () => void = () => {}) {
    const scope = reference(info);
    this.result = (async () => {
      let timer: ReturnType<typeof setTimeout> | undefined;
      let timedOut = false;
      try {
        const handle = await invoke<{ id: string }>("begin_operation", { ...scope, kind });
        this.id = handle.id;
        if (this.cancelled) await this.cancel();
        this.status = this.cancelled ? "cancel_pending" : "running";
        this.changed();
        timer = setTimeout(() => { if (!this.cancelled) { timedOut = true; void this.cancel(); } }, info.policy.timeoutMs);
        return await invoke<T>(commands[kind], { request: { ...scope, operationId: handle.id, query } });
      } catch (error) {
        if (timedOut && errorInfo(error).code === "cancelled") throw { code: "timeout", message: "The operation reached its time budget and was cancelled." };
        throw error;
      } finally {
        if (timer !== undefined) clearTimeout(timer);
        this.id = null; this.status = "finished"; this.changed();
      }
    })();
  }
  async cancel(): Promise<void> {
    if (this.status === "finished") return;
    this.cancelled = true; this.status = "cancel_pending"; this.changed();
    if (this.id) await invoke("cancel_operation", { operationId: this.id }).catch(() => undefined);
  }
}

export const datasets = {
  // `format` is only an explicit JSON <-> JSON Lines reopen after a mismatch;
  // `excel` carries the chosen worksheet and header row.
  open: (path: string, openOptions: { previewToken?: string; format?: FormatKind; excel?: ExcelOptions; sqlite?: { object: string }; hdf5?: Hdf5Options } = {}) => invoke<DatasetInfo>("open_dataset", { path, openOptions }),
  inspectHdf5: (path: string, group: string) => invoke<Hdf5Info>("inspect_hdf5", { path, group }),
  inspectWorkbook: (path: string) => invoke<WorkbookInfo>("inspect_workbook", { path }),
  inspectDatabase: (path: string) => invoke<DatabaseInfo>("inspect_database", { path }),
  preview: (path: string, options: DelimitedOptions) => invoke<DelimitedPreview>("preview_delimited", { path, options }),
  discardPreview: (token: string) => invoke("discard_preview", { token }),
  sample: (destination: string) => invoke<DatasetInfo>("generate_sample", { destination }),
  refresh: (sessionId: string) => invoke<DatasetInfo>("refresh_dataset", { sessionId }),
  close: (sessionId: string) => invoke("close_dataset", { sessionId }),
};
