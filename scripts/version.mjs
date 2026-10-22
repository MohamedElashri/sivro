// package.json is the version source. Cargo and lockfile versions are mirrors.
import { readFileSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../", import.meta.url));

export function readVersion(directory = root) {
  const { version } = JSON.parse(readFileSync(join(directory, "package.json"), "utf8"));
  if (typeof version !== "string" || !/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(version)) {
    throw new Error("package.json must define a stable version in X.Y.Z form");
  }
  return version;
}

function cargoVersion(text, filename, version, header) {
  // Change only Sivro's own package block, preserving dependencies and comments.
  let packages = 0;
  const updated = text.split(/(?=^\[)/m).map((section) => {
    if (section.split(/\r?\n/, 1)[0] !== header || !/^name[ \t]*=[ \t]*"sivro"[ \t]*(?:#.*)?\r?$/m.test(section)) return section;
    packages++;
    const versions = [...section.matchAll(/^version[ \t]*=[ \t]*"[^"\r\n]*"/gm)];
    if (versions.length !== 1) throw new Error(`${filename}: expected one Sivro package version`);
    return section.replace(/^(version[ \t]*=[ \t]*)"[^"\r\n]*"/m, `$1"${version}"`);
  }).join("");
  if (packages !== 1) throw new Error(`${filename}: expected one Sivro package block`);
  return updated;
}

export function syncVersion(directory = root, { write = false, tag } = {}) {
  const version = readVersion(directory);
  if (tag !== undefined && tag !== `v${version}`) throw new Error(`Release tag must be v${version}, received ${tag}`);
  const changes = new Map();
  const update = (filename, transform) => {
    const original = readFileSync(join(directory, filename), "utf8");
    const updated = transform(original);
    if (updated !== original) changes.set(filename, updated);
  };
  update("package-lock.json", (text) => {
    const lock = JSON.parse(text);
    if (!lock.packages?.[""]) throw new Error("package-lock.json is missing the root package");
    if (lock.version === version && lock.packages[""].version === version) return text;
    lock.version = lock.packages[""].version = version;
    return `${JSON.stringify(lock, null, 2)}\n`;
  });
  update("src-tauri/Cargo.toml", (text) => cargoVersion(text, "src-tauri/Cargo.toml", version, "[package]"));
  update("src-tauri/Cargo.lock", (text) => cargoVersion(text, "src-tauri/Cargo.lock", version, "[[package]]"));
  update("src-tauri/tauri.conf.json", (text) => {
    const config = JSON.parse(text);
    if (config.version === "../package.json") return text;
    config.version = "../package.json";
    return `${JSON.stringify(config, null, 2)}\n`;
  });
  if (!write && changes.size) {
    throw new Error(`Version metadata differs from package.json (${version}): ${[...changes.keys()].join(", ")}. Run npm run version:sync and commit the results.`);
  }
  // Validate every input before writing any generated metadata.
  for (const [filename, content] of changes) writeFileSync(join(directory, filename), content);
  return { version, changedFiles: [...changes.keys()] };
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const args = process.argv.slice(2);
    if (args.length > 1 || args.length === 1 && args[0] !== "--write") {
      throw new Error("Usage: node scripts/version.mjs [--write]");
    }
    const { version, changedFiles } = syncVersion(root, { write: args[0] === "--write", tag: process.env.RELEASE_TAG });
    console.log(`Version ${version}: ${changedFiles.length ? `synchronized ${changedFiles.join(", ")}` : "all metadata agrees with package.json"}`);
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
