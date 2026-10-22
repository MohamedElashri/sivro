export type ThemePreference = "system" | "light" | "dark";
export type Theme = "light" | "dark";
const STORAGE_KEY = "sivro-theme";
export class ThemeController {
  preference: ThemePreference = "system";
  constructor(
    private storage: Pick<Storage, "getItem" | "setItem">,
    private media: Pick<MediaQueryList, "matches" | "addEventListener"> | null,
    private apply: (theme: Theme) => void,
  ) {
    try {
      const saved = storage.getItem(STORAGE_KEY);
      if (saved === "light" || saved === "dark") this.preference = saved;
    } catch { /* Appearance remains usable when storage is unavailable. */ }
    this.update();
    media?.addEventListener("change", () => this.update());
  }
  get resolved(): Theme { return this.preference === "system" ? this.media?.matches ? "dark" : "light" : this.preference; }
  setPreference(value: string): void {
    if (value !== "system" && value !== "light" && value !== "dark") return;
    this.preference = value;
    try { this.storage.setItem(STORAGE_KEY, value); } catch { /* Session-only choice. */ }
    this.update();
  }
  private update(): void { this.apply(this.resolved); }
}
