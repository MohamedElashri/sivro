import type { WorkbookInfo } from "./types";

// Workbook first, then one worksheet and header row; both are frozen for the
// opened session and changed only by confirming this dialog again.
export interface ExcelDialog {
  path: string; workbook: WorkbookInfo | null; sheet: string;
  header: boolean; headerRow: string; busy: boolean; error: string | null; epoch: number;
}

function html(value: string): string {
  return value.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;").replace(/'/g, "&#39;");
}

export function headerRowError(dialog: ExcelDialog): string | null {
  if (!dialog.header) return null;
  return /^[1-9]\d{0,6}$/.test(dialog.headerRow.trim()) ? null : "Enter the header's row number (1 or more), or choose no header.";
}

export function renderExcel(dialog: ExcelDialog | null): string {
  if (!dialog) return "";
  const book = dialog.workbook, invalid = headerRowError(dialog);
  const label = (visibility: string) => visibility === "hidden" ? " · hidden" : visibility === "very_hidden" ? " · very hidden" : "";
  const sheets = book ? book.sheets.map((sheet, i) => `<label class="excel-sheet ${sheet.selectable ? "" : "disabled"}">
      <input type="radio" name="excel-sheet" id="excel-sheet-${i}" data-excel="sheet" value="${html(sheet.name)}" ${sheet.name === dialog.sheet ? "checked" : ""} ${sheet.selectable ? "" : "disabled"} />
      <span>${html(sheet.name)}</span><small>${sheet.kind === "worksheet" ? "worksheet" : `${html(sheet.kind)} — no cell table`}${label(sheet.visibility)}</small></label>`).join("") : "";
  const ready = !!book && !!dialog.sheet && !invalid && !dialog.busy;
  return `<div class="overlay excel-overlay" role="dialog" aria-modal="true" aria-labelledby="excel-title"><div class="modal delimited-modal" data-modal-card>
    <div class="modal-head"><div><h2 id="excel-title">Excel workbook</h2><p class="modal-desc">${html(dialog.path)}</p></div><button class="btn-icon" data-action="cancel-excel" aria-label="Close workbook dialog">×</button></div>
    <div class="modal-body">
      ${dialog.busy && !book ? `<p class="modal-desc"><span class="spinner"></span> Checking the workbook…</p>` : ""}
      ${dialog.error ? `<p class="delimited-error" role="alert">${html(dialog.error)}</p>` : ""}
      ${book ? `<p class="modal-desc">${html(book.variant.toUpperCase())} · ${book.date1904 ? "1904" : "1900"} date system · ${book.sheets.length} sheets. Reading never changes the workbook.</p>
        <fieldset class="excel-sheets"><legend>Worksheet</legend>${sheets}</fieldset>
        <div class="delimited-settings">
          <label>Header<select id="excel-header" data-excel="header"><option value="yes" ${dialog.header ? "selected" : ""}>Row holds column names</option><option value="no" ${dialog.header ? "" : "selected"}>No header (columns A, B, …)</option></select></label>
          ${dialog.header ? `<label>Header row<input id="excel-header-row" data-excel="headerRow" inputmode="numeric" value="${html(dialog.headerRow)}" aria-invalid="${!!invalid}" /><small>Rows above it are not shown.</small></label>` : ""}
        </div>
        ${invalid ? `<p class="delimited-error" role="alert">${html(invalid)}</p>` : ""}
        ${book.warnings.map((w) => `<p class="modal-desc">${html(w)}</p>`).join("")}
        <p class="modal-desc">Each column gets one native type. Mixed values, errors and formulas without a cached result keep tagged JSON text. Only one sheet is loaded, within the cell limit.</p>` : ""}
    </div><div class="modal-foot"><button class="btn-ghost" data-action="cancel-excel">Cancel</button><button class="btn-primary" data-action="confirm-excel" ${ready ? "" : "disabled"}>${dialog.busy && book ? "Opening…" : "Open sheet"}</button></div>
  </div></div>`;
}
