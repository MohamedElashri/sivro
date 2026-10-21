# Development

Use Node.js 24 and npm, GNU Make, and the latest stable Rust with `rustfmt`
and `clippy` (the locked Tauri version needs Rust 1.90 or newer; CI lints with
the latest stable). Building also needs a C compiler, for the bundled SQLite.
On Debian/Ubuntu, install the native dependencies first:

```sh
sudo apt-get install build-essential file libayatana-appindicator3-dev \
  libgtk-3-dev libssl-dev libwebkit2gtk-4.1-dev libxdo-dev librsvg2-dev \
  patchelf unzip xdg-utils
rustup update stable && rustup component add rustfmt clippy
make dev
```

If Homebrew's `pkg-config` comes first on `PATH` and cannot find the system
GTK/WebKit libraries, set `PKG_CONFIG=/usr/bin/pkg-config`. The first build
downloads dependencies and DuckDB archives; later builds can run offline.

## Make targets

The Makefile wraps every build and check, and CI runs the same targets.
`make help` lists them.

| Target | Does |
|---|---|
| `make dev` | Run the app with hot reload |
| `make build` | Build release packages |
| `make app` | Build the release binary only (for the GUI tests) |
| `make check` | Typecheck the frontend and Rust code |
| `make fmt` / `make fmt-check` | Format Rust code / check formatting |
| `make lint` | Clippy on the app and library checks, warnings denied |
| `make test` | Frontend and backend tests |
| `make test-libs` | Library checks |
| `make test-e2e` / `make test-e2e-offline` | GUI suite against `make app` (Linux) |
| `make ci` | `fmt-check`, `lint`, `test` and `test-libs` |
| `make perf` | 1M-row performance baseline |
| `make notices` | Regenerate `THIRD_PARTY_NOTICES.md` |
| `make clean` / `make distclean` | Remove build outputs / also dependencies and DuckDB archives |

GUI end-to-end tests drive the real built app through WebDriver. They need
`tauri-driver` (`cargo install tauri-driver --locked`) and WebKitGTK's
`WebKitWebDriver` (Debian/Ubuntu: `webkit2gtk-driver`), and open windows on
the current display. [Testing](testing.md) describes every suite;
[releasing](releasing.md) describes the release gates. CI runs formatting,
clippy, the frontend, backend and library tests, and the Linux GUI suite on
every pull request, plus the macOS and Windows backend suites on `main`.

## DuckDB linking

Use the build wrapper: `scripts/with-duckdb.mjs` stages official
prebuilt static DuckDB archives on Linux/macOS via `scripts/fetch-duckdb.mjs`.
The version comes from `libduckdb-sys` in `src-tauri/Cargo.lock`. Native DuckDB
1.5.6 corresponds to Rust crate 1.10506.0. `src-tauri/build.rs` links the engine,
extension loader, bundled extension archives and platform libraries. Generated
archives, link configuration and build outputs are ignored by Git.

Windows MSVC must build from source because DuckDB's official prebuilt Windows
archives use MinGW. The Makefile adds `--features source-bundled` on Windows;
without Make, pass it to Tauri and Cargo yourself.

`source-bundled` enables DuckDB's `bundled`, `json` and `parquet` features:
bundled builds compile only the extensions requested (plus `core_functions`).
Do not add these features to the default build; they force source
compilation and change the static-archive link strategy.

## Library checks

The separate [library-checks crate](../experiments/library-checks/README.md) pins
down library behaviour the readers rely on: raw JSON numeric fidelity,
array/JSONL output, DuckDB interruption, selected-sheet Calamine access and
read-only rusqlite WAL semantics.

Run them with `make test-libs`.

See [architecture](architecture.md), [testing](testing.md) and the
[changelog](../CHANGELOG.md) for design decisions, test coverage and limitations.

## Third-party notices

Packages include `LICENSE` and [`THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md):
the license of every bundled Rust crate (for all release targets) and npm
package, the DuckDB 1.5.6 engine and its statically linked components
(including ICU and jemalloc), and SQLite 3.53.2 (public domain). The bundled
Inter and JetBrains Mono fonts are under the SIL Open Font License 1.1.
Regenerate the notices with `node scripts/third-party-notices.mjs` after any
dependency change. Excel and other test fixtures are synthetic.
