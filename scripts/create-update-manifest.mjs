// Assemble once after all release jobs finish: concurrent platform jobs must
// never read/overwrite a shared latest.json. No dependencies or network I/O.
import { readFileSync, readdirSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export function createManifest({ version, tag, repository, notes, publicKey, files, now = new Date().toISOString() }) {
  if (tag !== `v${version}` || !/^\d+\.\d+\.\d+$/.test(version)) throw new Error("A stable release tag must match the app version");
  if (!/^[\w.-]+\/[\w.-]+$/.test(repository)) throw new Error("Invalid GitHub repository");
  const key = Buffer.from(Buffer.from(publicKey, "base64").toString("utf8").trim().split(/\r?\n/)[1] ?? "", "base64");
  if (key.length !== 42) throw new Error("Invalid updater public key");
  const candidates = new Map();
  const prefix = `Sivro-v${version}-`;
  for (const [name, content] of Object.entries(files)) {
    if (!name.startsWith(prefix) || !name.endsWith(".sig")) continue;
    const match = /^(darwin|linux|windows)-(universal|x86_64|x64|amd64|aarch64|arm64)-(app\.app\.tar\.gz|appimage\.AppImage|nsis-setup\.exe|msi\.msi)\.sig$/.exec(name.slice(prefix.length));
    if (!match) continue;
    const [, os, architecture, bundle] = match;
    if (os === "darwin" && bundle !== "app.app.tar.gz" || os === "linux" && bundle !== "appimage.AppImage" || os === "windows" && !["nsis-setup.exe", "msi.msi"].includes(bundle)) throw new Error(`Mismatched platform and bundle: ${name}`);
    const asset = name.slice(0, -4);
    if (!Object.hasOwn(files, asset)) throw new Error(`Missing signed update asset: ${asset}`);
    const signature = String(content).trim();
    const lines = Buffer.from(signature, "base64").toString("utf8").trim().split(/\r?\n/);
    const raw = Buffer.from(lines[1] ?? "", "base64");
    if (lines.length !== 4 || raw.length !== 74 || Buffer.from(lines[3], "base64").length !== 64 || !raw.subarray(2, 10).equals(key.subarray(2, 10))) throw new Error(`Invalid signature or wrong signing key: ${name}`);
    if (!lines[2].startsWith("trusted comment: ") || !lines[2].split("\t").includes(`version:${version}`)) throw new Error(`Signature is not bound to version ${version}: ${name}`);
    // Runtime Tauri verification authenticates bytes and the signed version.
    // Here we fail publication on incomplete assets or mismatched signing keys.
    const arch = ["x86_64", "x64", "amd64"].includes(architecture) ? "x86_64" : architecture === "universal" ? "universal" : "aarch64";
    if (arch === "universal" && os !== "darwin") throw new Error(`Invalid universal asset: ${name}`);
    const arches = arch === "universal" ? ["x86_64", "aarch64"] : [arch];
    for (const a of arches) {
      const target = `${os}-${a}`;
      const priority = bundle === "nsis-setup.exe" ? 2 : 1;
      const installer = bundle.startsWith("nsis") ? "nsis" : bundle.startsWith("msi") ? "msi" : os === "darwin" ? "app" : "appimage";
      const specific = `${target}-${installer}`;
      if (candidates.has(specific)) throw new Error(`Duplicate update asset for ${specific}`);
      candidates.set(specific, { priority, signature, url: `https://github.com/${repository}/releases/download/${tag}/${encodeURIComponent(asset)}` });
      const previous = candidates.get(target);
      if (previous?.priority === priority) throw new Error(`Duplicate update asset for ${target}`);
      if (!previous || priority > previous.priority) candidates.set(target, { priority, signature, url: `https://github.com/${repository}/releases/download/${tag}/${encodeURIComponent(asset)}` });
    }
  }
  for (const target of ["darwin-x86_64", "darwin-aarch64", "linux-x86_64", "windows-x86_64"]) {
    if (!candidates.has(target)) throw new Error(`Missing required signed updater for ${target}`);
  }
  return { version, notes, pub_date: now, platforms: Object.fromEntries([...candidates].sort(([a], [b]) => a.localeCompare(b)).map(([target, { signature, url }]) => [target, { signature, url }])) };
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const [directory, releaseFile, output] = process.argv.slice(2);
  if (!directory || !releaseFile || !output) throw new Error("Usage: node scripts/create-update-manifest.mjs ASSETS RELEASE_JSON OUTPUT");
  const config = JSON.parse(readFileSync(new URL("../src-tauri/tauri.conf.json", import.meta.url), "utf8"));
  const release = JSON.parse(readFileSync(releaseFile, "utf8"));
  // Read signatures only; installer files may be hundreds of megabytes.
  const files = Object.fromEntries(readdirSync(directory).map((name) => [name, name.endsWith(".sig") ? readFileSync(join(directory, name), "utf8") : null]));
  const manifest = createManifest({ version: config.version, tag: release.tag_name, repository: process.env.GITHUB_REPOSITORY, notes: release.body ?? "", publicKey: config.plugins.updater.pubkey, files });
  writeFileSync(output, `${JSON.stringify(manifest, null, 2)}\n`);
  console.log(`Validated signed updater assets: ${Object.keys(manifest.platforms).join(", ")}`);
}
