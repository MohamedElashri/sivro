#!/usr/bin/env node
// Regenerates THIRD_PARTY_NOTICES.md: every Rust crate compiled into the app
// for the release targets (normal dependencies only), the npm packages shipped
// in the frontend bundle, DuckDB's statically linked components and SQLite.
// License texts are copied from the packages themselves; DuckDB's
// third-party texts are fetched from its pinned release tag (build-time only).
//   node scripts/third-party-notices.mjs
import { execFileSync } from "node:child_process";
import { existsSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";

const root = resolve(new URL("..", import.meta.url).pathname);
const manifest = join(root, "src-tauri/Cargo.toml");
const targets = ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu", "x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc", "x86_64-apple-darwin", "aarch64-apple-darwin"];
const licenseFile = /^(licen[cs]e|copying|notice|unlicense|copyright)/i;

const texts = new Map(); // license text -> Set of component labels
function addText(text, label) {
  const key = text.replace(/\r\n/g, "\n").trim();
  if (!texts.has(key)) texts.set(key, new Set());
  texts.get(key).add(label);
}
function licenseTexts(dir) {
  return existsSync(dir) ? readdirSync(dir).filter((f) => licenseFile.test(f)).sort().map((f) => readFileSync(join(dir, f), "utf8")) : [];
}

// Rust: walk normal (non-dev, non-build) edges from the app for each target.
const crates = new Map();
for (const target of targets) {
  const meta = JSON.parse(execFileSync("cargo", ["metadata", "--format-version", "1", "--locked", "--manifest-path", manifest, "--filter-platform", target], { encoding: "utf8", maxBuffer: 256 << 20 }));
  const packages = new Map(meta.packages.map((p) => [p.id, p]));
  const nodes = new Map(meta.resolve.nodes.map((n) => [n.id, n]));
  const seen = new Set();
  const stack = [meta.resolve.root];
  while (stack.length) {
    const id = stack.pop();
    if (seen.has(id)) continue;
    seen.add(id);
    for (const dep of nodes.get(id).deps) {
      if (dep.dep_kinds.some((k) => k.kind === null)) stack.push(dep.pkg);
    }
  }
  for (const id of seen) {
    const p = packages.get(id);
    if (id === meta.resolve.root) continue;
    const key = `${p.name} ${p.version}`;
    const entry = crates.get(key) ?? { name: p.name, version: p.version, license: p.license ?? p.license_file ?? "UNKNOWN", dir: dirname(p.manifest_path), targets: new Set() };
    entry.targets.add(target);
    crates.set(key, entry);
  }
}
const rustRows = [...crates.values()].sort((a, b) => a.name.localeCompare(b.name) || a.version.localeCompare(b.version));
const missing = [];
for (const c of rustRows) {
  const found = licenseTexts(c.dir);
  if (!found.length) missing.push(`${c.name} ${c.version} (${c.license})`);
  for (const text of found) addText(text, `${c.name} ${c.version}`);
}

// npm: production dependencies only; the frontend bundle ships these.
const tree = JSON.parse(execFileSync("npm", ["ls", "--omit=dev", "--all", "--json"], { cwd: root, encoding: "utf8" }));
const npmRows = [];
(function walk(deps) {
  for (const [name, d] of Object.entries(deps ?? {})) {
    const dir = join(root, "node_modules", name);
    const pkg = JSON.parse(readFileSync(join(dir, "package.json"), "utf8"));
    if (!npmRows.some((r) => r.name === name && r.version === pkg.version)) {
      npmRows.push({ name, version: pkg.version, license: pkg.license ?? "UNKNOWN" });
      const found = licenseTexts(dir);
      if (!found.length) missing.push(`${name} ${pkg.version} (${pkg.license})`);
      for (const text of found) addText(text, `${name} ${pkg.version} (npm)`);
    }
    walk(d.dependencies);
  }
})(tree.dependencies);

// DuckDB: the engine and its statically linked third-party components.
const duckdbTag = "v1.5.6";
const components = ["brotli", "concurrentqueue", "fast_float", "fastpforlib", "fmt", "fsst", "httplib", "hyperloglog", "jaro_winkler", "libpg_query", "lz4", "mbedtls", "miniz", "parquet", "pcg", "pdqsort", "re2", "ska_sort", "skiplist", "snappy", "tdigest", "thrift", "utf8proc", "vergesort", "yyjson", "zstd"]
  .map((c) => `third_party/${c}`).concat(["third_party/jemalloc", "extension/icu/third_party/icu"]);
const api = (path) => fetch(`https://api.github.com/repos/duckdb/duckdb/contents/${path}?ref=${duckdbTag}`, { headers: { accept: "application/vnd.github+json", ...(process.env.GITHUB_TOKEN ? { authorization: `Bearer ${process.env.GITHUB_TOKEN}` } : {}) } }).then((r) => r.ok ? r.json() : Promise.reject(new Error(`${r.status} for ${path}`)));
const raw = (path) => fetch(`https://raw.githubusercontent.com/duckdb/duckdb/${duckdbTag}/${path}`).then((r) => r.ok ? r.text() : Promise.reject(new Error(`${r.status} for ${path}`)));
addText(await raw("LICENSE"), `DuckDB ${duckdbTag.slice(1)} (statically linked)`);
const duckdbRows = [];
for (const path of components) {
  const files = (await api(path)).filter((f) => f.type === "file" && licenseFile.test(f.name));
  duckdbRows.push({ path, files: files.map((f) => f.name) });
  if (!files.length) missing.push(`DuckDB ${path} (no license file in the component directory)`);
  for (const f of files) addText(await raw(`${path}/${f.name}`), `DuckDB component ${path.split("/").pop()}`);
}

// SQLite: bundled by libsqlite3-sys; public-domain dedication from its header.
const sqliteHeader = readFileSync(join(crates.get([...crates.keys()].find((k) => k.startsWith("libsqlite3-sys "))).dir, "sqlite3/sqlite3.h"), "utf8");
const sqliteVersion = sqliteHeader.match(/#define SQLITE_VERSION\s+"([^"]+)"/)[1];
addText(sqliteHeader.match(/\*\* The author disclaims[\s\S]*?\*{6,}/)[0].replace(/^\*\*\s?/gm, "").replace(/\*+$/, "").trim(), `SQLite ${sqliteVersion} (public domain, statically linked)`);

const out = [
  "# Third-party notices",
  "",
  "Sivro is MIT-licensed (see `LICENSE`). Release builds include the components",
  "below. This file is generated by `node scripts/third-party-notices.mjs` from",
  "the locked dependency graph, the shipped npm packages, DuckDB's",
  `${duckdbTag} sources and the bundled SQLite ${sqliteVersion}. System libraries`,
  "(for example WebKitGTK on Linux) are not distributed with the app.",
  "",
  `## Rust crates (${rustRows.length}; normal dependencies for the release targets)`,
  "",
  "| Crate | Version | License | Targets |",
  "|---|---|---|---|",
  ...rustRows.map((c) => `| ${c.name} | ${c.version} | ${c.license} | ${c.targets.size === targets.length ? "all" : [...c.targets].join(", ")} |`),
  "",
  `## Frontend packages (${npmRows.length}; production dependencies)`,
  "",
  "| Package | Version | License |",
  "|---|---|---|",
  ...npmRows.map((r) => `| ${r.name} | ${r.version} | ${r.license} |`),
  "",
  "## Statically linked native code",
  "",
  `- DuckDB ${duckdbTag.slice(1)} (MIT) with these bundled components: ${duckdbRows.map((r) => r.path.split("/").pop()).join(", ")}.`,
  `- SQLite ${sqliteVersion} (public domain).`,
  "",
  ...(missing.length ? ["## Components whose package carries no license file", "", "These packages declare the SPDX license shown in the tables above but ship no license text. The standard text of each license appears below under other components; their own copyright holders are the package authors.", "", ...missing.map((m) => `- ${m}`), ""] : []),
  "## License texts",
  "",
  ...[...texts.entries()].flatMap(([text, labels]) => [`### ${[...labels].sort().slice(0, 12).join(", ")}${labels.size > 12 ? `, and ${labels.size - 12} more` : ""}`, "", "```text", text.replace(/```/g, "'''"), "```", ""]),
];
writeFileSync(join(root, "THIRD_PARTY_NOTICES.md"), out.join("\n"));
console.log(`THIRD_PARTY_NOTICES.md: ${rustRows.length} crates, ${npmRows.length} npm packages, ${duckdbRows.length} DuckDB components, ${texts.size} distinct license texts, ${missing.length} without a packaged license file`);
