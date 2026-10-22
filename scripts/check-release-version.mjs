import { readFileSync } from "node:fs";

const read = (name) => readFileSync(new URL(`../${name}`, import.meta.url), "utf8");
const version = JSON.parse(read("package.json")).version;
const lock = JSON.parse(read("package-lock.json"));
const cargo = /^version = "([^"]+)"/m.exec(read("src-tauri/Cargo.toml"))?.[1];
const cargoLock = /\[\[package\]\]\nname = "sivro"\nversion = "([^"]+)"/.exec(read("src-tauri/Cargo.lock"))?.[1];
const config = JSON.parse(read("src-tauri/tauri.conf.json"));
if (![lock.version, lock.packages[""].version, cargo, cargoLock, config.version].every((value) => value === version)) throw new Error("Release versions disagree across package, Cargo and Tauri manifests");
if (process.env.RELEASE_TAG && process.env.RELEASE_TAG !== `v${version}`) throw new Error(`Release tag must be v${version}`);
console.log(`Release versions agree: ${version}`);
