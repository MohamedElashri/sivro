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
  private (`-Wl,--exclude-libs,ALL`).

DuckDB is engine 1.5.6 (`libduckdb-sys` 1.10506.0). `scripts/with-duckdb.mjs`
stages the official static archives on Linux and macOS
(`DUCKDB_TRIPLE=universal-apple-darwin` merges both macOS architectures).
Windows MSVC compiles DuckDB from source with `--features source-bundled`,
which the Makefile adds automatically.

## Tests

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

1. Set the version in `package.json`, `package-lock.json`,
   `src-tauri/Cargo.toml`, `src-tauri/Cargo.lock` and
   `src-tauri/tauri.conf.json`; update `CHANGELOG.md` and
   `docs/release-notes.md`.
2. Run `make ci build app test-e2e test-e2e-offline`.
3. Push a `vX.Y.Z` tag. `release.yml` creates a draft release, runs the
   backend tests on every target, builds Linux x86_64, Windows x86_64 and a
   universal macOS app (ARM64 Linux/Windows are experimental), runs the GUI
   suite on Linux, attaches `SHA256SUMS.txt` and publishes from the `release`
   environment. Add required reviewers to that environment to hold
   publication for manual macOS and Windows checks.
