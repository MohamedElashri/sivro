import type { DatabaseInfo } from "./types";

// Database first, then one table or view; the choice is frozen for the
// session and changed only by confirming this dialog again.
export interface SqliteDialog { path: string; database: DatabaseInfo | null; object: string; busy: boolean; error: string | null; epoch: number }

function html(value: string): string {
  return value.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;").replace(/'/g, "&#39;");
}

export function renderSqlite(dialog: SqliteDialog | null): string {
  if (!dialog) return "";
  const db = dialog.database;
  const objects = db ? db.objects.map((o, i) => `<label class="excel-sheet ${o.selectable ? "" : "disabled"}">
      <input type="radio" name="sqlite-object" id="sqlite-object-${i}" data-sqlite="object" value="${html(o.name)}" ${o.name === dialog.object ? "checked" : ""} ${o.selectable ? "" : "disabled"} />
      <span>${html(o.name)}</span><small>${o.kind}${o.withoutRowid ? " · without rowid" : ""} · ${o.columns} columns${o.reason ? ` — ${html(o.reason)}` : ""}</small></label>`).join("") : "";
  const ready = !!db && !!dialog.object && !dialog.busy;
  return `<div class="overlay excel-overlay" role="dialog" aria-modal="true" aria-labelledby="sqlite-title"><div class="modal delimited-modal" data-modal-card>
    <div class="modal-head"><div><h2 id="sqlite-title">SQLite database</h2><p class="modal-desc">${html(dialog.path)}</p></div><button class="btn-icon" data-action="cancel-sqlite" aria-label="Close database dialog">×</button></div>
    <div class="modal-body">
      ${dialog.busy && !db ? `<p class="modal-desc"><span class="spinner"></span> Reading the schema…</p>` : ""}
      ${dialog.error ? `<p class="delimited-error" role="alert">${html(dialog.error)}</p>` : ""}
      ${db ? `<p class="modal-desc">SQLite ${html(db.sqliteVersion)} · journal mode ${html(db.journalMode)} · ${db.objects.length} tables and views.</p>
        ${db.objects.length ? `<fieldset class="excel-sheets"><legend>Table or view</legend>${objects}</fieldset>` : `<p class="modal-desc">This database has no user tables or views.</p>`}
        ${db.warnings.map((w) => `<p class="modal-desc">${html(w)}</p>`).join("")}` : ""}
    </div><div class="modal-foot"><button class="btn-ghost" data-action="cancel-sqlite">Cancel</button><button class="btn-primary" data-action="confirm-sqlite" ${ready ? "" : "disabled"}>${dialog.busy && db ? "Opening…" : "Open"}</button></div>
  </div></div>`;
}
