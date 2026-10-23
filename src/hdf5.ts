import type { Hdf5Attribute, Hdf5Info, Hdf5Object, Hdf5Options } from "./types";
export interface Hdf5Dialog { path: string; info: Hdf5Info | null; selected: string; columns: boolean; rowAxis: number; columnAxis: number | null; indices: string[]; rowStart: string; rowLimit: string; busy: boolean; error: string | null; epoch: number }
const html = (s: string) => s.replace(/&/g,"&amp;").replace(/</g,"&lt;").replace(/>/g,"&gt;").replace(/"/g,"&quot;").replace(/'/g,"&#39;");
export const selectedHdf5 = (d: Hdf5Dialog): Hdf5Object | undefined => d.info?.objects.find(o => o.path === d.selected && o.kind === "dataset");
export function chooseHdf5(d: Hdf5Dialog, path: string, columns = false): void {
  d.selected = path; d.columns = columns; const object = selectedHdf5(d), rank = columns ? 1 : object?.shape.length ?? 0;
  d.rowAxis = 0; d.columnAxis = !columns && rank > 1 && !object?.dataType.startsWith("compound") ? 1 : null;
  d.indices = Array(rank).fill("0"); d.rowStart = "0"; d.rowLimit = ""; d.error = null;
}
export function hdf5Error(d: Hdf5Dialog): string | null {
  if (!d.info || !d.selected) return "Choose a dataset or column group.";
  const o = selectedHdf5(d); if (d.columns ? !d.info.columnsSelectable : !o?.selectable) return "This object cannot be opened as a table.";
  const shape = d.columns ? d.info.objects.find(o => o.kind === "dataset")?.shape ?? [] : o?.shape ?? [];
  if (shape.length && (d.rowAxis >= shape.length || d.columnAxis === d.rowAxis)) return "Choose distinct row and column axes.";
  if (!/^\d{1,20}$/.test(d.rowStart) || BigInt(d.rowStart) > BigInt(shape[d.rowAxis] ?? "1")) return "Row start must be inside the dataset (zero-based).";
  if (d.rowLimit && (!/^\d{1,9}$/.test(d.rowLimit) || Number(d.rowLimit) < 1)) return "Enter a positive row limit, or leave it blank for all rows.";
  for (let i = 0; i < shape.length; i++) if (i !== d.rowAxis && i !== d.columnAxis && (!/^\d{1,20}$/.test(d.indices[i] ?? "") || BigInt(d.indices[i]) >= BigInt(shape[i]))) return `Index for dimension ${i} is outside the dataset.`;
  return null;
}
export function hdf5Options(d: Hdf5Dialog): Hdf5Options { return { object: d.selected, columns: d.columns, rowAxis: d.rowAxis, columnAxis: d.columnAxis, indices: [...d.indices], rowStart: d.rowStart, rowLimit: d.rowLimit ? Number(d.rowLimit) : null }; }
function attrs(values: Hdf5Attribute[]): string { return values.length ? `<details class="json-block"><summary>${values.length} attributes</summary><pre>${html(values.map(a => `${a.name} · ${a.dataType}\n${a.value ?? "Value unavailable for this attribute type"}`).join("\n\n"))}</pre></details>` : ""; }
export function renderHdf5(d: Hdf5Dialog | null): string {
  if (!d) return ""; const info = d.info, object = selectedHdf5(d), invalid = hdf5Error(d);
  const rank = d.columns ? 1 : object?.shape.length ?? 0;
  const axes = (selected: number | null, none: boolean) => `${none ? `<option value="none" ${selected === null ? "selected" : ""}>Named fields / one value</option>` : ""}${Array.from({length: rank}, (_, i) => `<option value="${i}" ${selected === i ? "selected" : ""}>Dimension ${i}${object ? ` · ${html(object.shape[i])}` : ""}</option>`).join("")}`;
  const parent = info?.group.replace(/\/?[^/]+\/?$/, "") || "/";
  return `<div class="overlay excel-overlay" role="dialog" aria-modal="true" aria-labelledby="hdf5-title"><div class="modal delimited-modal hdf5-modal" data-modal-card>
    <div class="modal-head"><div><h2 id="hdf5-title">HDF5 explorer</h2><p class="modal-desc">${html(d.path)}</p></div><button class="btn-icon" data-action="cancel-hdf5" aria-label="Close HDF5 dialog">×</button></div>
    <div class="modal-body">
      ${d.busy ? `<p class="modal-desc"><span class="spinner"></span> ${info ? "Opening selection…" : "Reading group…"}</p>` : ""}
      ${d.error ? `<p class="delimited-error" role="alert">${html(d.error)}</p>` : ""}
      ${info ? `<div class="modal-actions-row"><button class="btn-ghost btn-sm" data-action="browse-hdf5" data-group="${html(parent)}" ${d.busy || info.group === "/" ? "disabled" : ""}>↑ Parent</button><strong>${html(info.group)}</strong></div>
        ${attrs(info.attributes)}
        <fieldset class="excel-sheets"><legend>Groups and datasets</legend>
          ${info.columnsSelectable ? `<label class="excel-sheet"><input type="radio" name="hdf5-object" data-hdf5="columns" value="${html(info.group)}" ${d.columns ? "checked" : ""} ${d.busy ? "disabled" : ""}/><span>Open this group as columns</span><small>Equally sized vectors · separate column names</small></label>` : ""}
          ${info.objects.map(o => o.kind === "group" ? `<button class="btn-ghost" data-action="browse-hdf5" data-group="${html(o.path)}" ${d.busy ? "disabled" : ""}>▸ ${html(o.name)}/</button>` : `<label class="excel-sheet ${o.selectable ? "" : "disabled"}"><input type="radio" name="hdf5-object" data-hdf5="object" value="${html(o.path)}" ${!d.columns && d.selected === o.path ? "checked" : ""} ${!o.selectable || d.busy ? "disabled" : ""}/><span>${html(o.name)}</span><small>${html(o.dataType)} · ${o.shape.length ? html(o.shape.join(" × ")) : "scalar / null"}${o.reason ? ` · ${html(o.reason)}` : ""}</small></label>`).join("")}
          ${!info.objects.length ? `<p class="modal-desc">This group has no browsable hard-linked objects.</p>` : ""}
        </fieldset>
        ${object && !d.columns ? `<p class="modal-desc">${html(object.layout)}${object.chunkShape ? ` · chunks ${html(object.chunkShape.join(" × "))}` : ""}${object.filters.length ? ` · filter IDs ${object.filters.join(", ")}` : ""}</p>${attrs(object.attributes)}` : ""}
        ${d.selected ? `<div class="delimited-settings">
          ${rank && !d.columns ? `<label>Rows<select data-hdf5="rowAxis" ${d.busy ? "disabled" : ""}>${axes(d.rowAxis, false)}</select></label><label>Columns<select data-hdf5="columnAxis" ${d.busy ? "disabled" : ""}>${axes(d.columnAxis, true)}</select></label>` : ""}
          ${Array.from({length: rank}, (_, i) => i !== d.rowAxis && i !== d.columnAxis ? `<label>Dimension ${i} index<input data-hdf5="index" data-dimension="${i}" value="${html(d.indices[i] ?? "0")}" inputmode="numeric" ${d.busy ? "disabled" : ""}/></label>` : "").join("")}
          <label>First row (zero-based)<input data-hdf5="rowStart" value="${html(d.rowStart)}" inputmode="numeric" ${d.busy ? "disabled" : ""}/></label>
          <label>Row limit<input data-hdf5="rowLimit" value="${html(d.rowLimit)}" placeholder="All rows" inputmode="numeric" ${d.busy ? "disabled" : ""}/></label>
        </div>` : ""}
        ${invalid && d.selected ? `<p class="delimited-error">${html(invalid)}</p>` : ""}
        ${info.warnings.map(w => `<p class="modal-desc">${html(w)}</p>`).join("")}` : ""}
    </div><div class="modal-foot"><button class="btn-ghost" data-action="cancel-hdf5">Cancel</button><button class="btn-primary" data-action="confirm-hdf5" ${d.busy || invalid ? "disabled" : ""}>Open selection</button></div>
  </div></div>`;
}
