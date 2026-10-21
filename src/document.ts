import type { DatasetInfo, DocumentNode } from "./types";

// Each branch is fetched from the backend at most `jsonChildren` nodes at a
// time. Numbers stay exact source tokens and are never parsed in JavaScript.
export interface DocumentBranch { node: DocumentNode | null; children: DocumentNode[]; hasMore: boolean; loading: boolean; error: string | null }
export interface DocumentState { branches: Map<string, DocumentBranch>; expanded: Set<string> }

export function pathKey(path: number[]): string { return path.join("/"); }
export function parsePath(key: string): number[] { return key === "" ? [] : key.split("/").map(Number); }
export function childKey(parent: string, index: number): string { return parent === "" ? String(index) : `${parent}/${index}`; }

function html(value: string): string {
  return value.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;").replace(/'/g, "&#39;");
}

function label(node: DocumentNode): string {
  if (node.key === null) return `<span class="json-key doc-index">[${node.index}]</span>`;
  return `<span class="json-key">${html(JSON.stringify(node.key))}${node.keyTruncated ? "…" : ""}</span>`;
}

function scalar(node: DocumentNode): string {
  const more = node.truncated ? `<span class="doc-note">… preview of ${html(node.byteLength)} bytes; Copy JSON for the full value</span>` : "";
  if (node.kind === "null") return `<span class="json-null">null</span>`;
  if (node.kind === "boolean") return `<span class="json-bool">${html(node.value ?? "")}</span>`;
  if (node.kind === "number") return `<span class="json-number">${html(node.value ?? "")}</span>${more}`;
  return `<span class="json-string">“${html(node.value ?? "")}”</span>${more}`;
}

function container(node: DocumentNode): string {
  const count = node.childCount ?? "0";
  return node.kind === "array" ? `Array · ${count} items` : `Object · ${count} fields`;
}

function copyButton(key: string): string {
  return `<button class="mini-btn doc-copy" data-action="doc-copy" data-node="${html(key)}" title="Copy this node as JSON">Copy JSON</button>`;
}

function renderBranch(state: DocumentState, key: string): string {
  const branch = state.branches.get(key);
  if (!branch) return "";
  const rows = branch.children.map((node) => {
    const nodeKey = childKey(key, node.index);
    if (node.childCount === null) {
      return `<div class="doc-row">${label(node)}<span class="json-colon">: </span>${scalar(node)}${copyButton(nodeKey)}</div>`;
    }
    const open = state.expanded.has(nodeKey);
    return `<div class="doc-row doc-container">
      <button class="doc-toggle" data-action="doc-toggle" data-node="${html(nodeKey)}" aria-expanded="${open}">${open ? "▾" : "▸"} ${label(node)} <span class="json-container-kind">${container(node)}</span></button>${copyButton(nodeKey)}
      ${open ? `<div class="doc-children">${renderBranch(state, nodeKey)}</div>` : ""}
    </div>`;
  }).join("");
  const empty = !branch.loading && !branch.error && branch.node?.childCount === "0" ? `<span class="json-empty">Empty ${branch.node.kind}</span>` : "";
  const status = branch.loading ? `<div class="doc-status"><span class="spinner"></span> Loading…</div>`
    : branch.error ? `<div class="doc-status prof-error">${html(branch.error)}</div>`
    : branch.hasMore ? `<button class="btn-ghost btn-sm" data-action="doc-more" data-node="${html(key)}">Show more (${branch.children.length.toLocaleString("en-US")} of ${html(branch.node?.childCount ?? "?")} shown)</button>` : "";
  return rows + empty + status;
}

export function renderDocument(info: DatasetInfo, state: DocumentState | null, error: string | null, stale: boolean): string {
  const doc = info.document;
  if (error) {
    return `<div class="grid-empty"><h3>${stale ? "Source changed or session unavailable" : "Couldn't read this document"}</h3><p>${html(error)}</p>
      <button class="btn-primary" data-action="refresh-source">${stale ? "Refresh source" : "Reopen"}</button></div>`;
  }
  const root = state?.branches.get("");
  if (!root?.node) return `<div class="grid-empty"><span class="spinner"></span><p>Loading document…</p></div>`;
  const body = root.node.childCount === null
    ? `<div class="doc-row doc-scalar">${scalar(root.node)}</div>`
    : `<div class="doc-row doc-root"><span class="json-container-kind">${container(root.node)}</span></div>${renderBranch(state as DocumentState, "")}`;
  return `<div class="doc-view json-tree" role="tree" aria-label="JSON document">
    <div class="doc-head"><p class="modal-desc">${html(doc?.reason ?? "")} Numbers are shown as exact source tokens.</p>${copyButton("")}</div>
    ${body}
  </div>`;
}
