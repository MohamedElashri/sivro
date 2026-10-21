# Releasing Sivro

Versions follow SemVer: PATCH for compatible fixes, MINOR for additive
features, MAJOR for breaking changes to stable behaviour. Before 1.0, additive
betas use `0.x`. `v0.1.0` is the first release; `v1.0.0` should follow only
after all seven input families, the four required targets and the export tests
pass on installed packages.

## 1. Prepare the version

1. Set the same version in `package.json`, `package-lock.json` (both root
   entries), `src-tauri/Cargo.toml`, `src-tauri/Cargo.lock` (`sivro`
   package) and `src-tauri/tauri.conf.json`.
2. Move the `Unreleased` entries in `CHANGELOG.md` under the new version.
3. Update `docs/release-notes.md` (its text is prepended to the release
   notes).
4. Regenerate `THIRD_PARTY_NOTICES.md` with
   `node scripts/third-party-notices.mjs` whenever dependencies change, and
   review new licenses before release. It is bundled into every package.

## 2. Local gates (Linux x86_64)

```sh
npm ci && npm run check && npm run test:frontend
node scripts/with-duckdb.mjs cargo test --locked --manifest-path src-tauri/Cargo.toml
node scripts/with-duckdb.mjs cargo clippy --locked --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check
npm run tauri:build
npm run test:e2e && npm run test:e2e:offline
node scripts/perf-baseline.mjs            # compare with docs/testing.md
```

## 3. Tag and build

Pushing a `v*` tag (or running the workflow with an existing tag) creates a
**draft** release. `.github/workflows/release.yml` then:

- runs the backend reader tests on every target before bundling;
- builds the packages: Linux x86_64, Windows x86_64 and macOS x86_64/ARM64
  are required, while Linux and Windows ARM64 are experimental and cannot
  block publishing;
- runs the GUI end-to-end suite on the built Linux x86_64 app, online and
  offline;
- uploads the assets.

The `publish-release` job attaches `SHA256SUMS.txt` and then publishes. It
runs in the `release` environment. **Configure required reviewers for that
environment** in the repository settings; otherwise publishing proceeds
automatically once the builds succeed.

## 4. Manual checks before approving publication

The automated GUI suite covers Linux only. For each of the macOS x86_64, macOS
ARM64 and Windows x86_64 draft assets:

1. Install the package and launch it with networking disabled.
2. Open one file from each family: Parquet, CSV (confirm parsing), TSV, a
   JSON document, JSON Lines, an Excel workbook (pick a sheet) and a SQLite
   database (pick a table). `npm run test:e2e -- --keep` leaves a set of
   fixtures to use.
3. Check that the exact value `9007199254740993` appears, then sort, filter,
   inspect a row and export the view to CSV and Parquet.
4. Record the result in the release notes. Approve the `release` environment
   only when every required target passed.
