import { icon } from "./icons";
import type { UpdateController } from "./updates";

export const PROJECT_URL = "https://github.com/MohamedElashri/sivro";
export const ABOUT_LINKS = {
  source: PROJECT_URL,
  releases: `${PROJECT_URL}/releases`,
  issues: `${PROJECT_URL}/issues`,
  author: "https://elashri.com",
  license: `${PROJECT_URL}/blob/main/LICENSE`,
};
export interface AppMetadata {
  version: string;
  author: string;
  platform: string;
  arch: string;
  updateSupported: boolean;
  updateHint: string;
}
const escape = (text: string) => text.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!);
const button = (action: string, text: string, primary = false) => `<button id="${action}" class="${primary ? "btn-primary" : "btn-ghost"}" data-action="${action}">${text}</button>`;

export function renderAbout(meta: AppMetadata | null, metadataError: string | null, updates: UpdateController): string {
  const s = updates.state;
  const messages = {
    idle: "Check GitHub Releases for a newer stable version.",
    checking: "Checking GitHub Releases…",
    current: "You’re up to date. No newer stable release is available.",
    available: `Sivro ${s.version ?? ""} is available.`,
    confirm: `Install Sivro ${s.version ?? ""}?`,
    downloading: "Downloading update…",
    verifying: "Verifying the update signature…",
    installing: "Installing update… Keep Sivro open.",
    installed: "Update installed. Restart Sivro to use the new version.",
    restarting: "Restarting Sivro…",
    error: "Update unavailable",
  };
  const progress = s.total && s.total > 0 ? Math.min(100, Math.round(s.downloaded / s.total * 100)) : null;
  return `<div class="overlay about-overlay" data-action="close-about">
    <section class="modal about-modal" role="dialog" aria-modal="true" aria-labelledby="about-title" data-modal-card>
      <div class="modal-head"><div class="modal-title" id="about-title">About Sivro</div><button id="about-close" class="icon-btn" data-action="close-about" aria-label="Close About">${icon("close")}</button></div>
      <div class="modal-body">
        <div class="about-brand"><div class="brand-mark">${icon("table")}</div><div><h1>Sivro</h1><p>A local, read-only data explorer</p></div></div>
        <p class="modal-desc">Explore Parquet, CSV, TSV, JSON, JSON Lines, Excel and SQLite files on your computer.</p>
        <dl class="about-details"><div><dt>Version</dt><dd id="about-version">${meta ? escape(meta.version) : "Loading…"}</dd></div><div><dt>Author</dt><dd>${escape(meta?.author ?? "Mohamed Elashri")}</dd></div><div><dt>Platform</dt><dd>${meta ? `${escape(meta.platform)} · ${escape(meta.arch)}` : "—"}</dd></div><div><dt>License</dt><dd>MIT · © 2026 Mohamed Elashri</dd></div></dl>
        ${metadataError ? `<p class="about-error" role="alert">Could not load application details. ${escape(metadataError)}</p>${button("reload-about", "Retry app details")}` : ""}
        <nav class="about-links" aria-label="Project links">${Object.entries({ source: "Source code", releases: "Releases", issues: "Report an issue", author: "Author website", license: "MIT license" }).map(([key, label]) => `<button class="link-btn" data-action="about-link" data-link="${key}">${label} ↗</button>`).join("")}</nav>
        <section class="about-updates" aria-labelledby="updates-title"><h2 id="updates-title">Updates</h2>
          <p class="modal-desc">Checks and installs run only when you request them. No automatic checks, downloads or updates.</p>
          <p id="update-status" role="status" aria-live="polite" aria-atomic="true">${escape(messages[s.phase])}</p>
          ${s.checkedAt ? `<p class="modal-desc muted">Last checked: ${escape(s.checkedAt.toLocaleString())}</p>` : ""}
          ${meta && !meta.updateSupported ? `<p class="modal-desc">${escape(meta.updateHint)} Download and install a release manually.</p>` : ""}
          ${s.notes ? `<details class="update-notes"><summary>Release notes${s.date && !Number.isNaN(Date.parse(s.date)) ? ` · ${escape(new Date(s.date).toLocaleDateString())}` : ""}</summary><pre>${escape(s.notes)}</pre></details>` : ""}
          ${s.phase === "confirm" ? `<p class="about-confirm">Installation may close Sivro and will end your current session. Finish exports and other work before continuing. Open files will need to be reopened.</p>` : ""}
          ${s.phase === "downloading" || s.phase === "verifying" ? `<progress aria-label="Update download" max="100" ${progress === null ? "" : `value="${progress}"`}></progress><p class="modal-desc muted">${(s.downloaded / 1048576).toFixed(1)} MB downloaded${progress === null ? "" : ` · ${progress}%`}</p>` : ""}
          ${s.error ? `<p class="about-error" role="alert">${escape(s.error)}</p>` : ""}
        </section>
      </div>
      <div class="modal-foot">
            ${meta && !updates.busy && !["installed", "confirm"].includes(s.phase) ? button("check-updates", s.phase === "error" ? "Try checking again" : "Check for updates") : ""}
            ${s.phase === "available" && meta?.updateSupported ? button("confirm-update", "Download and install…", true) : ""}
            ${s.phase === "confirm" ? button("cancel-update", "Not now") + button("install-update", "Install update", true) : ""}
            ${s.phase === "installed" ? button("restart-update", "Restart Sivro", true) : ""}
            ${s.phase === "error" || meta && !meta.updateSupported ? `<button class="btn-ghost" data-action="about-link" data-link="releases">Release downloads ↗</button>` : ""}
      </div>
    </section>
  </div>`;
}
