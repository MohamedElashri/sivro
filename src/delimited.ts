import type { DelimitedOptions, DelimitedPreview } from "./types";
import { cellText } from "./cells";
export interface DelimitedDialog {
  path: string; options: DelimitedOptions; preview: DelimitedPreview | null;
  valid: boolean; busy: boolean; error: string | null; epoch: number;
}
function html(value: string): string {
  return value.replace(/&/g,"&amp;").replace(/</g,"&lt;").replace(/>/g,"&gt;").replace(/"/g,"&quot;").replace(/'/g,"&#39;");
}
export function renderDelimited(dialog: DelimitedDialog | null): string {
  if (!dialog) return "";
  const o = dialog.options, p = dialog.preview;
  const input = (key: string, label: string, value: string, hint: string) => `<label>${label}<input id="delimited-${key}" data-delimited="${key}" value="${html(value)}" aria-label="${label}" /><small>${hint}</small></label>`;
  return `<div class="overlay delimited-overlay" role="dialog" aria-modal="true" aria-labelledby="delimited-title"><div class="modal delimited-modal" data-modal-card>
    <div class="modal-head"><div><h2 id="delimited-title">CSV / TSV parsing</h2><p class="modal-desc">${html(dialog.path)}</p></div><button class="btn-icon" data-action="cancel-delimited" aria-label="Close parsing preview">×</button></div>
    <div class="modal-body">
      <p class="modal-desc">Confirm the preview before opening. UTF-8 with optional BOM is supported. Inference uses a bounded sample; force-text preserves leading zeros and numeric spelling. Later mismatches fail and require reopening.</p>
      <div class="delimited-settings">
        ${input("delimiter", "Delimiter", o.delimiter === "\t" ? "\\t" : o.delimiter ?? "auto", "One ASCII character, \\t for tab, or auto")}
        <label>Header<select id="delimited-header" data-delimited="header"><option value="auto" ${o.header === undefined ? "selected" : ""}>Detect from sample</option><option value="yes" ${o.header === true ? "selected" : ""}>First record is header</option><option value="no" ${o.header === false ? "selected" : ""}>No header</option></select></label>
        ${input("quote", "Quote", o.quote ?? '"', "Empty disables quoting")}
        ${input("escape", "Escape", o.escape ?? '"', "Usually the same as quote")}
        ${input("nullMarker", "NULL marker", o.nullMarker ?? "\\N", "Unquoted matches become NULL; quoted matches stay text")}
        <label>Types<select id="delimited-forceText" data-delimited="forceText"><option value="no" ${!o.forceText ? "selected" : ""}>Infer exact types</option><option value="yes" ${o.forceText ? "selected" : ""}>Force text</option></select><small>Per-column overrides take precedence. DOUBLE may round.</small></label>
      </div>
      ${dialog.error ? `<p class="delimited-error" role="alert">${html(dialog.error)}</p>` : ""}
      ${p ? `<p class="modal-desc">${dialog.valid ? "Preview ready" : "Settings changed — update preview"} · ${p.sampledRows.toLocaleString()} data records sampled${p.hasMore ? " (more records exist)" : ""} · ${p.options.header ? "header" : "no header"} · delimiter ${html(JSON.stringify(p.options.delimiter))}. Preview cells show original text; opening applies the displayed types.</p>
        <div class="delimited-preview"><table><thead><tr>${p.columns.map(c=>`<th>${html(c.name || "(blank)")} <small>${html(c.id)}</small><br/><span>${html(c.dataType)}</span><input id="delimited-type-${c.id}" data-delimited-type="${c.id}" value="${html(o.typeOverrides?.[c.id] ?? "")}" placeholder="Type override (optional)" aria-label="Type override ${html(c.id)}" /></th>`).join("")}</tr></thead><tbody>${p.rows.map(row=>`<tr>${row.map(c=>`<td>${c.kind === "null" ? "<i>NULL</i>" : html(cellText(c)) || "<i>(empty text)</i>"}</td>`).join("")}</tr>`).join("")}</tbody></table></div>
        ${p.warnings.map(w=>`<p class="modal-desc">${html(w)}</p>`).join("")}` : ""}
      <p class="modal-desc">Overrides: VARCHAR, BIGINT, UBIGINT, HUGEINT, DECIMAL(p,s), BOOLEAN, DATE, TIMESTAMP, DOUBLE. Blank lines are ignored; malformed records fail. Empty NULL markers make unquoted empty fields NULL. Duplicate/blank labels retain independent column IDs.</p>
    </div><div class="modal-foot"><button class="btn-ghost" data-action="cancel-delimited">Cancel</button><button class="btn-tool" data-action="preview-delimited" ${dialog.busy ? "disabled" : ""}>${dialog.busy ? "Working…" : "Update preview"}</button><button class="btn-primary" data-action="confirm-delimited" ${dialog.busy || !dialog.valid ? "disabled" : ""}>Open with these settings</button></div>
  </div></div>`;
}
