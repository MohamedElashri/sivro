import assert from "node:assert/strict";
import test from "node:test";
import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createManifest } from "../scripts/create-update-manifest.mjs";
import { readVersion, syncVersion } from "../scripts/version.mjs";

const version = "0.1.1";
const key = Buffer.alloc(42, 7);
const publicKey = Buffer.from(`untrusted comment: test\n${key.toString("base64")}\n`).toString("base64");
function signature(v = version, id = key.subarray(2, 10)) {
  const raw = Buffer.alloc(74); id.copy(raw, 2);
  return Buffer.from(`untrusted comment: test\n${raw.toString("base64")}\ntrusted comment: timestamp:0\tfile:test\tversion:${v}\n${Buffer.alloc(64).toString("base64")}\n`).toString("base64");
}
function fixture(v = version) {
  const files = {};
  for (const target of ["darwin-universal-app.app.tar.gz", "linux-amd64-appimage.AppImage", "windows-x64-nsis-setup.exe", "windows-x64-msi.msi"]) {
    const name = `Sivro-v${v}-${target}`; files[name] = null; files[`${name}.sig`] = signature(v);
  }
  return { version: v, tag: `v${v}`, repository: "MohamedElashri/sivro", publicKey, files, notes: "Release notes" };
}
test("release versions are consistent", () => execFileSync(process.execPath, ["scripts/version.mjs"]));

const versionFiles = ["package.json", "package-lock.json", "src-tauri/Cargo.toml", "src-tauri/Cargo.lock", "src-tauri/tauri.conf.json"];
function versionFixture(t) {
  const directory = mkdtempSync(join(tmpdir(), "sivro-version-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  mkdirSync(join(directory, "src-tauri"));
  for (const filename of versionFiles) copyFileSync(new URL(`../${filename}`, import.meta.url), join(directory, filename));
  const read = (filename) => readFileSync(join(directory, filename), "utf8");
  const write = (filename, text) => writeFileSync(join(directory, filename), text);
  const setVersion = (value) => {
    const pkg = JSON.parse(read("package.json")); pkg.version = value;
    write("package.json", `${JSON.stringify(pkg, null, 2)}\n`);
  };
  const snapshot = () => versionFiles.map(read);
  return { directory, read, write, setVersion, snapshot };
}

test("one version edit synchronizes metadata without touching dependencies", (t) => {
  const fixture = versionFixture(t);
  const originalLock = JSON.parse(fixture.read("package-lock.json"));
  const originalCargoLock = fixture.read("src-tauri/Cargo.lock");
  const originalCargo = fixture.read("src-tauri/Cargo.toml");
  fixture.setVersion("12.34.56");
  const unsynchronized = fixture.snapshot();
  assert.throws(() => syncVersion(fixture.directory), /Version metadata differs.*version:sync/);
  assert.deepEqual(fixture.snapshot(), unsynchronized, "check mode must not modify files");
  const result = syncVersion(fixture.directory, { write: true });
  assert.equal(result.version, "12.34.56");
  const lock = JSON.parse(fixture.read("package-lock.json"));
  assert.equal(lock.version, result.version);
  assert.equal(lock.packages[""].version, result.version);
  lock.version = originalLock.version;
  lock.packages[""].version = originalLock.packages[""].version;
  assert.deepEqual(lock, originalLock, "npm dependency metadata must be preserved");
  const previous = readVersion();
  assert.equal(fixture.read("src-tauri/Cargo.toml"), originalCargo.replace(`version = "${previous}"`, 'version = "12.34.56"'));
  assert.equal(fixture.read("src-tauri/Cargo.lock"), originalCargoLock.replace(`name = "sivro"\nversion = "${previous}"`, 'name = "sivro"\nversion = "12.34.56"'));
  assert.equal(JSON.parse(fixture.read("src-tauri/tauri.conf.json")).version, "../package.json");
  const synchronized = fixture.snapshot();
  assert.deepEqual(syncVersion(fixture.directory, { write: true }).changedFiles, []);
  assert.deepEqual(fixture.snapshot(), synchronized, "synchronization must be idempotent");
  assert.equal(syncVersion(fixture.directory, { tag: "v12.34.56" }).version, "12.34.56");
});

test("release validation rejects drift in each mirror and Tauri's version source", (t) => {
  const fixture = versionFixture(t);
  for (const filename of versionFiles.slice(1)) {
    const original = fixture.read(filename);
    if (filename.endsWith(".json")) {
      const json = JSON.parse(original); json.version = "9.9.9";
      fixture.write(filename, JSON.stringify(json));
    } else if (filename.endsWith("Cargo.lock")) {
      fixture.write(filename, original.replace(`name = "sivro"\nversion = "${readVersion()}"`, 'name = "sivro"\nversion = "9.9.9"'));
    } else {
      fixture.write(filename, original.replace(`version = "${readVersion()}"`, 'version = "9.9.9"'));
    }
    assert.throws(() => syncVersion(fixture.directory), (error) => error.message.includes(filename));
    fixture.write(filename, original);
  }
  const lock = JSON.parse(fixture.read("package-lock.json")); lock.packages[""].version = "9.9.9";
  fixture.write("package-lock.json", JSON.stringify(lock));
  assert.throws(() => syncVersion(fixture.directory), /package-lock.json/);
});

test("incorrect tags and invalid versions fail before any metadata is written", (t) => {
  const fixture = versionFixture(t);
  fixture.setVersion("12.34.56");
  const before = fixture.snapshot();
  for (const tag of ["v0.0.0", "12.34.56", ""]) {
    assert.throws(() => syncVersion(fixture.directory, { write: true, tag }), /Release tag must be v12.34.56/);
    assert.deepEqual(fixture.snapshot(), before);
  }
  for (const value of ["v1.2.3", "01.2.3", "1.2", "1.2.3-beta", null]) {
    fixture.setVersion(value);
    assert.throws(() => syncVersion(fixture.directory, { write: true }), /stable version/);
  }
});

test("malformed Cargo metadata cannot leave partially synchronized files", (t) => {
  const fixture = versionFixture(t);
  fixture.setVersion("12.34.56");
  fixture.write("src-tauri/Cargo.lock", fixture.read("src-tauri/Cargo.lock").replace('name = "sivro"', 'name = "missing-app"'));
  const before = fixture.snapshot();
  assert.throws(() => syncVersion(fixture.directory, { write: true }), /expected one Sivro package block/);
  assert.deepEqual(fixture.snapshot(), before);
});

test("npm version updates the canonical version and invokes metadata synchronization", (t) => {
  const fixture = versionFixture(t);
  mkdirSync(join(fixture.directory, "scripts"));
  copyFileSync(new URL("../scripts/version.mjs", import.meta.url), join(fixture.directory, "scripts/version.mjs"));
  const env = { ...process.env };
  delete env.RELEASE_TAG;
  execFileSync("npm", ["version", "12.34.56", "--no-git-tag-version", "--ignore-scripts=false"], {
    cwd: fixture.directory, env, shell: process.platform === "win32",
  });
  assert.equal(syncVersion(fixture.directory, { tag: "v12.34.56" }).version, "12.34.56");
});

test("manifest CLI resolves the canonical version when Tauri references package.json", (t) => {
  const directory = mkdtempSync(join(tmpdir(), "sivro-manifest-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const current = readVersion();
  const data = fixture(current);
  const config = JSON.parse(readFileSync(new URL("../src-tauri/tauri.conf.json", import.meta.url), "utf8"));
  const rawKey = Buffer.from(Buffer.from(config.plugins.updater.pubkey, "base64").toString("utf8").trim().split(/\r?\n/)[1], "base64");
  mkdirSync(join(directory, "assets"));
  for (const name of Object.keys(data.files)) {
    writeFileSync(join(directory, "assets", name), name.endsWith(".sig") ? signature(current, rawKey.subarray(2, 10)) : "");
  }
  const release = join(directory, "release.json");
  const output = join(directory, "latest.json");
  writeFileSync(release, JSON.stringify({ tag_name: data.tag, body: "Current changelog section" }));
  execFileSync(process.execPath, ["scripts/create-update-manifest.mjs", join(directory, "assets"), release, output], {
    env: { ...process.env, GITHUB_REPOSITORY: data.repository },
  });
  const manifest = JSON.parse(readFileSync(output, "utf8"));
  assert.equal(manifest.version, current);
  assert.equal(manifest.notes, "Current changelog section");
  assert.ok(manifest.platforms["linux-x86_64"]);
});
test("one manifest maps universal macOS and selects NSIS on Windows", () => {
  const manifest = createManifest(fixture());
  assert.equal(manifest.version, version);
  assert.equal(manifest.platforms["darwin-x86_64"].url, manifest.platforms["darwin-aarch64"].url);
  assert.match(manifest.platforms["windows-x86_64"].url, /nsis-setup\.exe$/);
  assert.match(manifest.platforms["windows-x86_64-msi"].url, /msi\.msi$/);
  assert.match(manifest.platforms["windows-x86_64-nsis"].url, /nsis-setup\.exe$/);
  assert.match(manifest.platforms["linux-x86_64"].url, /releases\/download\/v0\.1\.1\/.*\.AppImage$/);
});
test("a partial release cannot be published to the updater", () => {
  const data = fixture(); delete data.files["Sivro-v0.1.1-linux-amd64-appimage.AppImage.sig"];
  assert.throws(() => createManifest(data), /Missing required signed updater for linux-x86_64/);
});
test("missing artifacts, malformed signatures and wrong keys fail closed", () => {
  const name = "Sivro-v0.1.1-linux-amd64-appimage.AppImage";
  const data = fixture(); delete data.files[name]; assert.throws(() => createManifest(data), /Missing signed update asset/);
  data.files[name] = null; data.files[`${name}.sig`] = "junk"; assert.throws(() => createManifest(data), /Invalid signature/);
  data.files[`${name}.sig`] = signature(version, Buffer.alloc(8, 9)); assert.throws(() => createManifest(data), /wrong signing key/);
});
test("wrong signed version, prereleases and mismatched tags are refused", () => {
  const data = fixture(); data.files["Sivro-v0.1.1-linux-amd64-appimage.AppImage.sig"] = signature("0.1.0");
  assert.throws(() => createManifest(data), /not bound to version/);
  assert.throws(() => createManifest({ ...fixture(), tag: "v0.1.2" }), /must match/);
  assert.throws(() => createManifest({ ...fixture(), version: "0.1.1-beta", tag: "v0.1.1-beta" }), /stable release/);
});
test("experimental targets are optional but must have complete artifacts", () => {
  const data = fixture(); const name = "Sivro-v0.1.1-linux-aarch64-appimage.AppImage";
  data.files[name] = null; data.files[`${name}.sig`] = signature();
  assert.ok(createManifest(data).platforms["linux-aarch64"]);
});
