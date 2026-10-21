#!/usr/bin/env node
// Performance baseline: writes 1M-row fixtures (Excel: its 200,000-cell
// limit) outside Git, then measures each format in a fresh optimized test
// process so peak RSS is per format. Prints a Markdown table.
//   node scripts/perf-baseline.mjs [--dir DIR]
import { execFileSync } from "node:child_process";
import { mkdtempSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const root = resolve(new URL("..", import.meta.url).pathname);
const i = process.argv.indexOf("--dir");
const dir = resolve(i > 0 ? process.argv[i + 1] : mkdtempSync(join(tmpdir(), "sivro-perf-")));
const test = (name, env) => execFileSync("node", ["scripts/with-duckdb.mjs", "cargo", "test", "--release", "--quiet", "--manifest-path", "src-tauri/Cargo.toml", "--lib", name, "--", "--ignored", "--exact", "--nocapture", "--test-threads=1"],
  { cwd: root, env: { ...process.env, SIVRO_PERF_DIR: dir, ...env }, encoding: "utf8", stdio: ["ignore", "pipe", "inherit"] });
if (!existsSync(join(dir, "rows.xlsx"))) test("tests::perf::perf_write_fixtures", {});
const files = ["rows.parquet", "rows.csv", "rows.tsv", "rows.jsonl", "rows.json", "rows.sqlite", "rows.xlsx"];
const reports = files.map((file) => {
  const out = test("tests::perf::perf_measure", { SIVRO_PERF_FILE: file });
  return JSON.parse(out.split("\n").find((l) => l.startsWith("SIVRO_PERF ")).slice(11));
});
const names = reports[0].steps.map(([n]) => n.replace(/ \(.*\)/, ""));
const mib = (b) => (b / 1048576).toFixed(1);
console.log(`| Step | ${reports.map((r) => r.file).join(" | ")} |`);
console.log(`|---|${reports.map(() => "---:").join("|")}|`);
console.log(`| Source size (MiB) | ${reports.map((r) => mib(r.bytes)).join(" | ")} |`);
console.log(`| Rows | ${reports.map((r) => r.rows.value ?? (r.steps.find(([n]) => n.startsWith("export csv"))?.[0].match(/\((\d+)/)?.[1] ?? "?")).join(" | ")} |`);
for (const [k, name] of names.entries()) console.log(`| ${name} (ms) | ${reports.map((r) => r.steps[k][1]).join(" | ")} |`);
console.log(`| Peak RSS (MiB) | ${reports.map((r) => mib(r.peak_rss_kib * 1024)).join(" | ")} |`);
console.log(`| Peak scratch (MiB) | ${reports.map((r) => mib(r.scratch_peak_bytes)).join(" | ")} |`);
console.log(`\nfixtures: ${dir}`);
