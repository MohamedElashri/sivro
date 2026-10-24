# Development

## Setup

Use Node.js 24, GNU Make and stable Rust with `rustfmt` and `clippy`.
Install the [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/).
On Debian/Ubuntu:

```sh
sudo apt-get install build-essential file libayatana-appindicator3-dev \
  libgtk-3-dev libssl-dev libwebkit2gtk-4.1-dev libxdo-dev librsvg2-dev \
  patchelf unzip xdg-utils
rustup update stable
rustup component add rustfmt clippy
make dev
```

The first build downloads dependencies and DuckDB archives. Linux and macOS
use prebuilt DuckDB libraries; Windows builds them with `source-bundled`,
which Make enables automatically. On Linux, use
`PKG_CONFIG=/usr/bin/pkg-config` if Homebrew masks the system libraries.

## Commands

| Command | Purpose |
| --- | --- |
| `make dev` | Start development mode |
| `make check` | Typecheck frontend and backend |
| `make fmt` / `make lint` | Format Rust / run Clippy |
| `make test` / `make test-libs` | Run application / library tests |
| `make ci` | Run formatting, lint and all non-GUI tests |
| `make build` | Build signed release packages |
| `make app` / `make app-debug` | Build an unbundled release / debug app |
| `make perf` | Measure the performance baseline |

Use `make help` for all targets. To run the Linux desktop suite:

```sh
make app-debug
SIVRO_BIN="$PWD/src-tauri/target/debug/sivro" make test-e2e test-e2e-offline
```

GUI tests require `tauri-driver` and `webkit2gtk-driver`; headless runs also
need `xvfb` and `xauth`. Use `xvfb-run -a` when headless. Ubuntu 24.04's offline
test may require `kernel.apparmor_restrict_unprivileged_userns=0`.

## Code layout

The app uses Tauri 2, a Rust backend and a TypeScript/Vite frontend.

| Location | Responsibility |
| --- | --- |
| `src-tauri/src/adapters/` | Readers and table adapters |
| `src-tauri/src/sessions.rs` | Sessions, cancellation and source-change detection |
| `src-tauri/src/query/` | Typed filters and parameterized queries |
| `src-tauri/src/export.rs` | Staged exports with no-overwrite publication |
| `src-tauri/src/policy.rs` | Resource limits |
| `src/` | Grid, dialogs, appearance and updater UI |

Numbers cross IPC as tagged text to preserve precision. File operations are
local; update checks require user action. HDF5 uses a patched pure Rust reader;
see its [vendor notes](../src-tauri/vendor/hdf5-pure/PATCHES.md).

## Tests and CI

CI checks formatting and the frontend first, then runs Clippy, backend tests
and online/offline GUI tests on one Linux runner. Cargo, DuckDB and WebDriver
are cached. Release builds test the packaged version on supported targets.

Frontend tests cover IPC and UI behaviour; backend tests cover readers,
precision, exports, cancellation and resource limits. Desktop tests exercise
the real app. Synthetic interoperability fixtures live in `tests/fixtures/`.

## Releasing

1. From current `main`, run `npm version X.Y.Z --no-git-tag-version`.
   This synchronizes Cargo and npm metadata from `package.json`.
2. Add `## X.Y.Z - YYYY-MM-DD` to `CHANGELOG.md` with concise release notes.
   Validate with `RELEASE_TAG=vX.Y.Z npm run version:check` and run
   `make ci build app test-e2e test-e2e-offline`.
3. Commit the release files, then tag and push together:

   ```sh
   git commit -am "chore: Release vX.Y.Z"
   git tag vX.Y.Z
   git push --atomic origin main vX.Y.Z
   ```

The release commit prefix skips duplicate main-branch CI; the tag starts the
release workflow. Use it only for release commits and do not use `[skip ci]`.
The workflow creates a draft, tests and builds platform packages, validates
signatures and `latest.json`, adds `SHA256SUMS.txt`, then publishes through the
`release` environment. Required reviewers can hold publication for manual checks.
Published versions are immutable; corrections require a new version.

### Updater signing

Configure `TAURI_SIGNING_PRIVATE_KEY` and, if needed,
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD` as repository secrets. The matching public
key is in `src-tauri/tauri.conf.json`. Keep a secure private-key backup;
existing installations trust that key. For local signed builds:

```sh
export TAURI_SIGNING_PRIVATE_KEY="$PWD/.secrets/sivro-updater.key"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD=''
make build
```

Use the locked Tauri CLI so signatures include the app version. Manual signing
requires `tauri signer sign --app-version X.Y.Z`. See the
[Tauri updater guide](https://v2.tauri.app/plugin/updater/) for details.

### Release smoke checks

- Check About's version, file opening and basic navigation online and offline.
- On macOS and Windows, verify installation, launch and restart before approval.
- With an isolated update endpoint, verify consent, cancellation, signed updates
  and rejection of invalid signatures; confirm the version after restart.
- Linux deb/rpm and development builds should offer release links rather than
  in-app installation. Version 0.1.0 requires a manual upgrade.
