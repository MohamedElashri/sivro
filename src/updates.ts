import type { CheckOptions, DownloadEvent, DownloadOptions } from "@tauri-apps/plugin-updater";

export interface UpdatePackage {
  version: string;
  body?: string;
  date?: string;
  download(onEvent: (event: DownloadEvent) => void, options: DownloadOptions): Promise<void>;
  install(): Promise<void>;
  close(): Promise<void>;
}
export type UpdatePhase = "idle" | "checking" | "current" | "available" | "confirm" | "downloading" | "verifying" | "installing" | "installed" | "restarting" | "error";
export interface UpdateState {
  phase: UpdatePhase;
  version?: string;
  notes?: string;
  date?: string;
  checkedAt?: Date;
  downloaded: number;
  total?: number;
  error?: string;
}

// Construction and opening About never perform network I/O. Only checkNow()
// contacts GitHub; only installConfirmed() downloads or changes the app.
export class UpdateController {
  state: UpdateState = { phase: "idle", downloaded: 0 };
  private pending: UpdatePackage | null = null;
  constructor(
    private check: (options: CheckOptions) => Promise<UpdatePackage | null>,
    private restart: () => Promise<void>,
    private changed: () => void,
  ) {}

  get busy(): boolean {
    return ["checking", "downloading", "verifying", "installing", "restarting"].includes(this.state.phase);
  }
  get changingApp(): boolean {
    return ["downloading", "verifying", "installing", "restarting"].includes(this.state.phase);
  }
  private set(next: Partial<UpdateState>): void {
    this.state = { ...this.state, ...next };
    this.changed();
  }
  private async release(): Promise<void> {
    const previous = this.pending;
    this.pending = null;
    await previous?.close().catch(() => undefined);
  }
  async checkNow(): Promise<void> {
    if (this.busy || this.state.phase === "installed") return;
    this.set({ phase: "checking", error: undefined, version: undefined, notes: undefined, date: undefined, downloaded: 0, total: undefined });
    await this.release();
    try {
      this.pending = await this.check({ timeout: 20_000 });
      this.set({ phase: this.pending ? "available" : "current", version: this.pending?.version, notes: this.pending?.body, date: this.pending?.date, checkedAt: new Date() });
    } catch (error) {
      this.set({ phase: "error", error: `Could not check for updates. Check your connection and try again. GitHub may be temporarily unavailable. Details: ${String(error)}` });
    }
  }
  confirm(): void {
    if (this.state.phase === "available" && this.pending) this.set({ phase: "confirm" });
  }
  cancelConfirm(): void {
    if (this.state.phase === "confirm") this.set({ phase: "available" });
  }
  async installConfirmed(): Promise<void> {
    if (this.state.phase !== "confirm" || !this.pending) return;
    const update = this.pending;
    this.set({ phase: "downloading", downloaded: 0, total: undefined, error: undefined });
    let lastProgress = 0;
    try {
      await update.download((event) => {
        if (event.event === "Started") this.set({ total: event.data.contentLength });
        else if (event.event === "Progress") {
          this.state.downloaded += event.data.chunkLength;
          // Avoid rebuilding the entire data explorer for every network chunk.
          if (Date.now() - lastProgress >= 100) { lastProgress = Date.now(); this.changed(); }
        }
        else this.set({ phase: "verifying" });
      }, { timeout: 300_000 });
      // download() verifies the signature before resolving. Never install on
      // the Finished event: signature verification can still reject afterward.
      this.set({ phase: "installing" });
      await update.install();
      this.set({ phase: "installed" });
    } catch (error) {
      this.set({ phase: "error", error: `The update could not be completed. Check again to retry, or use the release downloads. If installation started, restart Sivro to verify its version. Details: ${String(error)}` });
    } finally {
      await this.release();
    }
  }
  async restartNow(): Promise<void> {
    if (this.state.phase !== "installed") return;
    this.set({ phase: "restarting", error: undefined });
    try { await this.restart(); }
    catch (error) { this.set({ phase: "installed", error: `Could not restart Sivro. Quit and reopen it to finish updating. Details: ${String(error)}` }); }
  }
}
