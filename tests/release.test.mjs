import assert from "node:assert/strict";
import test from "node:test";
import { execFileSync } from "node:child_process";
import { createManifest } from "../scripts/create-update-manifest.mjs";

const version = "0.1.1";
const key = Buffer.alloc(42, 7);
const publicKey = Buffer.from(`untrusted comment: test\n${key.toString("base64")}\n`).toString("base64");
function signature(v = version, id = key.subarray(2, 10)) {
  const raw = Buffer.alloc(74); id.copy(raw, 2);
  return Buffer.from(`untrusted comment: test\n${raw.toString("base64")}\ntrusted comment: timestamp:0\tfile:test\tversion:${v}\n${Buffer.alloc(64).toString("base64")}\n`).toString("base64");
}
function fixture() {
  const files = {};
  for (const target of ["darwin-universal-app.app.tar.gz", "linux-amd64-appimage.AppImage", "windows-x64-nsis-setup.exe", "windows-x64-msi.msi"]) {
    const name = `Sivro-v${version}-${target}`; files[name] = null; files[`${name}.sig`] = signature();
  }
  return { version, tag: `v${version}`, repository: "MohamedElashri/sivro", publicKey, files, notes: "Release notes" };
}
test("release versions are consistent", () => execFileSync(process.execPath, ["scripts/check-release-version.mjs"]));
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
