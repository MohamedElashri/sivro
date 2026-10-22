# Development

## Setup

You need Node.js 24, GNU Make, a C compiler and the latest stable Rust with
`rustfmt` and `clippy` (CI lints with the latest stable), plus the
[Tauri prerequisites](https://v2.tauri.app/start/prerequisites/). On
Debian/Ubuntu:

```sh
sudo apt-get install build-essential file libayatana-appindicator3-dev \
  libgtk-3-dev libssl-dev libwebkit2gtk-4.1-dev libxdo-dev librsvg2-dev \
  patchelf unzip xdg-utils
rustup update stable && rustup component add rustfmt clippy
make dev
```

If Homebrew's `pkg-config` shadows the system one, set
`PKG_CONFIG=/usr/bin/pkg-config`. The first build downloads dependencies and
DuckDB archives; later builds work offline.

## Make targets

CI runs the same targets; `make help` lists them all.

| Target | Does |
|---|---|
| `make dev` | Run the app with hot reload |
| `make build` / `make app` | Release packages / release binary only |
| `make check` | Typecheck the frontend and Rust code |
| `make fmt` / `make fmt-check` | Format Rust code / check formatting |
| `make lint` | Clippy on all crates, warnings denied |
| `make test` | Frontend and backend tests |
| `make test-libs` | Library checks (`experiments/library-checks`) |
| `make test-e2e` / `make test-e2e-offline` | GUI suite against `make app` (Linux) |
| `make ci` | `fmt-check`, `lint`, `test` and `test-libs` |
| `make perf` | 1M-row performance baseline |
| `make clean` / `make distclean` | Remove build outputs / also dependencies and DuckDB archives |

## Architecture

A Tauri 2 app: a Rust backend in `src-tauri/src` and a vanilla
TypeScript/Vite frontend in `src`.

- **Sessions** (`sessions.rs`): each open file owns its fingerprint, frozen
  options and schema, a generation number, one serialized execution lane and
  a scratch directory. At most two expensive operations run at once; a 30 s
  budget interrupts native work, and refresh or close invalidates the
  generation so late replies are dropped.
- **Readers** (`adapters/`): Parquet, CSV/TSV, JSON/JSONL record tables and
  Excel run on DuckDB through the `TableSource` trait; JSON and Excel data is
  first staged into session scratch (`stage.rs`). JSON documents use a bounded
  in-memory tree. SQLite uses its own read-only rusqlite connection with an
  authorizer that allows only reads.
- **Queries** (`query/`): the frontend sends typed filters, column IDs, sorts
  and paging, never SQL. Rust compiles them with allow-listed identifiers and
  bound values, separately for DuckDB and SQLite.
- **Exact values:** cells are tagged, and integers and decimals cross IPC as
  text. JSON keeps raw serde_json tokens (`arbitrary_precision`).
- **Exports** (`export.rs`) are staged beside the destination and published
  with a no-clobber hard link; sources and their aliases are refused.
- **Limits** live in `policy.rs`: page sizes, 1 GiB scratch per session,
  512 MiB DuckDB memory, 200,000 Excel cells, 32 MiB JSON documents and a
  10,000,000-row export cap.
- **Offline:** fonts are bundled, the CSP allows only the app and Tauri IPC,
  DuckDB auto-install is off, and on Linux bundled SQLite/DuckDB symbols stay
  private (`-Wl,--exclude-libs,ALL`). Only the native updater makes network
  requests, after an explicit user action; opening About reads local metadata.
- **Updates** (`src/updates.ts`): a state machine separates checking,
  confirmation, download/signature verification, installation and restart.
  Check and download timeouts are 20 seconds and 5 minutes respectively.
  Tauri's updater verifies signatures and requires the signed version to
  match the manifest; downgrades are disabled. Release notes are rendered as
  escaped text. No network access is added to the WebView CSP.

DuckDB is engine 1.5.6 (`libduckdb-sys` 1.10506.0). `scripts/with-duckdb.mjs`
stages the official static archives on Linux and macOS
(`DUCKDB_TRIPLE=universal-apple-darwin` merges both macOS architectures).
Windows MSVC compiles DuckDB from source with `--features source-bundled`,
which the Makefile adds automatically.

## Tests

- **Appearance** (`tests/appearance.test.mjs`): persistence, live system-theme
  changes, storage failure recovery and WCAG AA text contrast in both themes.
  The GUI suite also checks theme restoration, dropdown keyboard highlights,
  About focus and pending-view tab indicators.
- **Frontend** (`tests/frontend.test.mjs`): mocked IPC, stale replies,
  dialogs, filters and exports.
- **Backend** (`src-tauri/src/tests`): every reader end to end, five-format
  export round trips, precision fixtures, hostile inputs, resource limits,
  cancellation, refresh and read-only guarantees.
- **Library checks:** isolated checks of the DuckDB, Calamine and rusqlite
  behaviour the readers rely on.
- **GUI** (`tests/e2e/run.mjs`): the built app through `tauri-driver` and
  `WebKitWebDriver` (`webkit2gtk-driver`), one file per format, online and
  inside a loopback-only network namespace. Use `xvfb-run -a` when headless;
  Ubuntu 24.04 needs `kernel.apparmor_restrict_unprivileged_userns=0` for the
  offline run.

Fixtures are synthetic. On a Ryzen 7 7735H, 1M-row sources open in 14 ms
(Parquet), ~200 ms (CSV/TSV) and ~5–6 s (JSON/JSONL, which are staged), with
peak memory under 350 MiB.

## Releasing

1. `package.json` is the only version source. Run
   `npm version X.Y.Z --no-git-tag-version` to set it; the version lifecycle
   hook runs `npm run version:sync` to update Cargo and lockfile metadata.
   If editing `package.json` directly, run `npm run version:sync` afterward.
   Tauri reads `../package.json` directly using its
   [version-file support](https://v2.tauri.app/reference/config/#version).
   Commit the generated metadata;
   do not edit its versions by hand. Update `CHANGELOG.md` under a `## X.Y.Z`
   heading; that section supplies the GitHub release notes.
   `npm run version:check` checks metadata without changing files. Frontend
   builds run it automatically, and release CI additionally requires the tag
   to equal `vX.Y.Z` before creating a draft or building packages.
2. Run `make ci build app test-e2e test-e2e-offline`.
3. Commit the version and changelog as `chore: Release vX.Y.Z`, tag it, and
   push both together:

   ```sh
   git commit -am "chore: Release vX.Y.Z"
   git tag vX.Y.Z
   git push --atomic origin main vX.Y.Z
   ```

   `ci.yml` skips pushes whose head commit starts with `chore: Release v`,
   since the release workflow runs every check on the tagged commit. Use that
   prefix only for release commits, and push other unpushed commits first so
   CI still checks them. Do not use `[skip ci]`; it can also skip the tag's
   release workflow.

   `release.yml` creates a draft release, runs the backend tests on every target, builds Linux x86_64, Windows x86_64 and a
   universal macOS app (ARM64 Linux/Windows are experimental), runs the GUI
   suite on Linux, assembles and validates `latest.json`, attaches
   `SHA256SUMS.txt` and publishes from the `release`
   environment. Add required reviewers to that environment to hold
   publication for manual macOS and Windows checks.

### Updater signing

The 0.1.1 signing key was generated locally in the ignored
`.secrets/sivro-updater.key`; its public key is committed in
`src-tauri/tauri.conf.json`. The private key is configured as the repository
Actions secret `TAURI_SIGNING_PRIVATE_KEY`. Back up the private key securely:
existing installations trust this public key, so losing its private key
prevents signing updates they will accept. Never commit the private key.
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD` is optional (the initial key has no
password). GitHub stores the secret encrypted; keep a separate secure backup.

For local signed packages:

```sh
export TAURI_SIGNING_PRIVATE_KEY="$PWD/.secrets/sivro-updater.key"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD=''
make build
```

`make app` builds the unbundled executable for tests without needing a signing
key. `make build` requires the private key to generate updater artifacts.
Use the Tauri CLI from the locked npm dependencies (2.12.1 or newer) so
signatures include the app version. If signing artifacts by hand, pass
`tauri signer sign --app-version X.Y.Z`.

The release workflow checks the secret and version before building. Each
platform uploads bundles and `.sig` files. A single final job creates the
manifest, checks the signature metadata/key ID/version, requires Linux
x86_64, Windows x86_64 and both macOS architectures, and includes experimental
ARM64 targets only if their signed bundles are complete. Windows entries
preserve MSI/NSIS installer selection; both macOS architectures use the
universal bundle. Runtime Tauri verification authenticates the downloaded
bytes and signed version before installation. Publication fails on missing
assets or inconsistent signing metadata.
Published releases cannot be rebuilt in place; bump the version so existing
manifests keep pointing to the signed bytes they were published with.

The manifest is hosted at
`https://github.com/MohamedElashri/sivro/releases/latest/download/latest.json`.
It points to immutable version-tag URLs. Keep releases stable and complete;
drafts and prereleases do not form the normal stable update channel. Before
the first manifest is published, a check reports an error with release
download links. 0.1.0 users must install 0.1.1 manually once.

The implementation follows the [Tauri updater documentation](https://v2.tauri.app/plugin/updater/).

### Appearance and menus

`src/themes.css` defines light and dark semantic tokens based on
[Catppuccin Latte and Mocha](https://github.com/catppuccin/catppuccin), with
text accents adjusted to meet 4.5:1 contrast on their backgrounds.
`src/theme.ts` stores `sivro-theme` (`system`, `light` or `dark`) locally and
listens for OS changes. Theme switches do not recreate datasets or issue queries.

`src/selects.ts` enhances native selects with accessible combobox/listbox
popups while preserving their values, change events, labels and validation.
Popups are positioned within the viewport outside scrolling containers,
support pointer hover, keyboard navigation, typeahead, selection checkmarks,
outside-click dismissal and Escape. Native file dialogs remain OS dialogs.

Startup is idempotent and uses mutually exclusive immediate/DOMContentLoaded
paths. Picker requests are guarded until selection, cancellation or failure
completes. Pending-view dots compare staged search/filters to the last loaded
query; they clear after Apply succeeds or changes are reverted. They do not
imply that a source file has been edited or needs saving.

### Update smoke checks

- Verify About's version, links, keyboard focus, Escape and focus restoration.
- Verify opening About and normal file operations work offline. An explicit
  offline check must show a recoverable error; retry online afterward.
- On each supported package, check a newer signed test release, cancel the
  confirmation, then confirm installation. Verify progress, restart behavior
  and the version after relaunch. Try a corrupt artifact/signature and check
  that installation is refused. Use an isolated test build and manifest;
  never publish a test update to the stable endpoint.
- On Linux deb/rpm and development builds, check that installation is not
  offered and release download links are available.
- On macOS and Windows, perform the manual installer/restart checks before
  approving the `release` environment. Automated GUI tests run on Linux;
  mocked tests cover update consent, failures, retries and signature rejection.
