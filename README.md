# Sivro

A local desktop data explorer built with Rust, Tauri 2, DuckDB and vanilla
TypeScript/Vite. Sivro starts from [ParquetPeek](https://github.com/goodappsstore/parquet-viewer),
retaining its MIT license and existing visual design.

**Supported inputs: Parquet (`.parquet`, `.pq`), CSV (`.csv`), TSV (`.tsv`),
JSON (`.json`), JSON Lines (`.jsonl`, `.ndjson`), Excel (`.xlsx`, `.xlsm`,
`.xlsb`, `.xls`) and SQLite (`.sqlite`, `.sqlite3`, `.db`).** Sivro is read-only
and works offline. The current release is 0.1.0, a pre-1.0 beta; see the
[release notes](docs/release-notes.md) for known limitations.

## Install

Download the package for your platform from the GitHub release, and check it
against `SHA256SUMS.txt`. Packages are not code-signed or notarized, so macOS
may ask you to allow the app under System Settings → Privacy & Security, and
Windows SmartScreen may warn on first launch.

## Features


Every format shares one workflow: multiple tabs, recent files, drag/drop,
schema and row-group details, a paged grid, search, filters, sorting, column
profiles, nested row inspection, copying and a 1,500-row sample generator.
Exports support Parquet, CSV, TSV, **JSON arrays** and **JSON Lines**. Queries
use backend-owned sessions and column IDs; exact numbers travel as tagged text. The first page uses the Parquet metadata count for an
unfiltered view. Filtered views offer **Count rows** on demand, while profiles
are computed when requested. **Refresh** explicitly reopens a changed source
and resets its view; **Cancel work** interrupts queued or active operations.

Exports preserve the loaded projection, filters and sort. A row-cap warning
identifies omitted rows. Existing destinations, source paths and aliases are
never overwritten; choose a new filename. CSV/TSV use `\N` for NULL and reject a
literal value equal to that marker. Binary cells export as `hex:` text in CSV/TSV
and `{ "$binary_hex": "...", "byteCount": "..." }` in JSON/JSONL. JSON exports reject
non-finite numbers; Parquet retains native types. Row copying keeps large
numbers as JSON numbers and uses strings for NaN/Infinity with a visible note.
Grid previews are bounded; expansion/copy is capped at 1 MiB.

CSV and TSV first open a **parsing preview**. Confirm the delimiter (comma/tab
by default, or `auto`), header, quote/escape, NULL marker and types. UTF-8 with
an optional BOM is supported; other encodings fail explicitly. Inference samples
at most 20,480 data records and selects exact integers/decimals, booleans or text.
Dates, exponent notation and numbers beyond exact supported numeric widths stay
text unless explicitly overridden. Preview cells show original text alongside
the proposed types. **Force text** preserves leading-zero IDs and numeric
spelling. Per-column overrides take precedence; explicitly choosing DOUBLE may
round values. Later rows incompatible with the frozen schema fail and require
reopening; malformed records are never intentionally skipped. Blank physical
lines are ignored by the scanner.

The default NULL marker is an unquoted `\N`: quoted `"\N"` and empty text stay
text. Choosing an empty NULL marker makes unquoted empty fields NULL. Use
**Parsing settings** to reopen with different options; Refresh retains the
confirmed schema/options, and a changed header requires a new preview. Duplicate
and blank labels retain separate column IDs. When labels are ambiguous, row
JSON copy and exports use column IDs as keys/headers and report that choice.
CSV/TSV row counts remain unknown until **Count rows** is requested. Delimited
exports preserve values rather than original quoting or numeric formatting;
reopen as text when lexical spelling matters.

JSON and JSON Lines open without a dialog; the whole file is validated when it
opens. A `.json` file whose root is an array of objects, and every JSON Lines
file, opens as a table. Any other `.json` root (an object, a single value, an
array of scalars or of mixed values, an empty array) opens in a **document
view**: a lazily expanded tree, 100 children at a time, limited to 32 MiB and
nesting depth 64. **Copy JSON** copies any node as formatted JSON, up to 1 MiB.
Numbers are never converted to JavaScript numbers. Values are exact source
tokens, except that exponents are written as `e+`/`e-` (`1E5` shows as `1e+5`).
Key order and repeated keys are kept.

Record tables read every record once when opening. Field types therefore come
from all records, not a sample; a key first seen late still becomes a column.
Integers use `BIGINT`/`HUGEINT` and decimals use exact `DECIMAL(p,s)`. Numbers
with exponents or more than 38 digits become `JSON_NUMBER`: the grid shows the
exact token, equality compares exact token text (`1.0` and `1` differ), and
sorting, range filters and min/max are disabled. Nested or mixed-type fields
become `JSON_VALUE` text with text matching and NULL checks. JSON Lines skips
blank lines; every other line must hold exactly one JSON value, and errors name
the 1-based line. Records over 16 MiB are rejected. Objects and scalar/array
records cannot be mixed. Scalar or array lines form a single `value` column.
Repeated keys become separate columns with the same label. Typed columns show
missing keys and JSON `null` both as NULL; **Show source record** in the row
panel shows the record itself. JSON Lines keeps each exact line; `.json`
array elements are re-serialized with exact values. Records are staged in
session scratch, so a source may be at most half the 1 GiB scratch budget.
If a `.json` file holds JSON Lines (or the reverse), opening reports the mismatch
and offers an explicit **Reopen as…** choice.

JSON/JSONL exports from these tables write exact tokens and nested values
unquoted. Parquet export refuses `JSON_NUMBER` columns because no Parquet
numeric type holds them exactly, and writes `JSON_VALUE` as text. For every
reader, `HUGEINT` is written to Parquet as `DECIMAL(38,0)` because DuckDB's
Parquet writer would otherwise store it as `DOUBLE`. Values beyond 38 digits
make the export fail.

Excel workbooks open in a **workbook dialog**. Pick one worksheet (hidden sheets
are labelled; chart, dialog and macro sheets cannot be opened) and a header row,
or choose no header to name columns A, B, …. Rows above the header are not
shown, and the count of skipped cells is reported. **Sheet:** in the toolbar
reopens the dialog; the choice is frozen per session and kept by Refresh. Sivro
reads only the values Excel cached when the workbook was saved: formulas are
never recalculated, and macros (`.xlsm`) are never run or exposed. A formula
without a cached value is tagged `{"$excel_formula": …, "$no_cached_value": true}`.

Each column gets one native type: `BIGINT` for whole numbers up to 2^53,
`DOUBLE`, `BOOLEAN`, `VARCHAR`, `DATE`, `TIMESTAMP` or `TIME`. Excel stores
numbers as binary doubles; Sivro shows the shortest decimal that round-trips,
which can differ from Excel's 15-digit display (`0.30000000000000004`). Dates
follow the workbook's 1900 or 1904 system at millisecond resolution. Serial 60
in the 1900 system (Excel's fictional 1900-02-29) and out-of-range serials are
tagged rather than shifted. A column that mixes value kinds, or contains errors
or uncached formulas, keeps tagged JSON text such as
`{"$excel_error":"#DIV/0!"}` and reports `mixed_types`. A blank cell is NULL;
an empty text cell stays empty text.

Workbooks are checked before parsing:
- `.xlsx`/`.xlsm`/`.xlsb` up to 50 MiB, with decompressed parts limited to
  128 MiB each and 256 MiB in total; parts over 1 MiB that compress more than
  1,000:1 are refused;
- `.xls` up to 16 MiB;
- a sheet's used range up to 200,000 cells. The range is checked while cells
  stream in, so a sheet whose cells sit at A1 and XFD1048576 is refused at once.

`.xls` files are parsed in full when opened, so their limit covers every sheet
together, including the extent each sheet *declares*. Encrypted workbooks are
not supported.

SQLite databases open in a **table/view picker**. Virtual tables are listed
but not opened, because they run extension modules, and internal tables are
hidden. **Table:** in the toolbar picks another object. Sivro opens the file
with a read-only connection, `query_only`, `trusted_schema=OFF` and an
authorizer that allows only reads. It never writes, attaches a database or
accepts SQL. Pages are read live: commits from another writer appear on the
next page and reset cached counts, while a schema change asks for **Refresh**.

Each cell shows its own SQLite storage class (integer, real, text or blob),
whatever the column's declared type. 64-bit integers stay exact, an empty
BLOB is not NULL, and text that isn't valid UTF-8 is shown as bytes. Rows are
ordered by rowid or the primary key, which also breaks sort ties. Views have
no stable key, so their row order can shift when the database changes. Text
search uses SQLite `LIKE`, which ignores case only for ASCII letters. Parquet
export refuses a column that mixes numbers, text and BLOBs. CSV, TSV, JSON
and JSONL keep each cell's representation.

A WAL-mode database needs its `-wal`/`-shm` files. Reading it in a writable
folder may create them; Sivro reports this, and the database content does not
change. In a read-only folder without them, it cannot be read.

![Upstream ParquetPeek interface with filters](screenshots/with-filters.png)

The screenshot shows the inherited interface before the Sivro rename.

## Development

Use Node.js 24 and npm, plus Rust with `rustfmt` and `clippy`. The locked Tauri
version requires Rust 1.90 or newer; development uses Rust 1.92.0.
Building also needs a C compiler, for the bundled SQLite.
On Debian/Ubuntu, install the native dependencies first:

```sh
sudo apt-get install build-essential file libayatana-appindicator3-dev \
  libgtk-3-dev libssl-dev libwebkit2gtk-4.1-dev libxdo-dev librsvg2-dev \
  patchelf unzip xdg-utils
npm ci
rustup component add rustfmt clippy
npm run tauri:dev
```

On hosts with Homebrew ahead of system tools, use `PKG_CONFIG=/usr/bin/pkg-config`
if Homebrew's `pkg-config` cannot locate the system GTK/WebKit libraries.
Build dependencies require network access on first setup; cached dependencies
and staged DuckDB archives can subsequently be used offline. The app itself
needs no network: fonts are bundled, the content security policy allows only the
app and Tauri IPC, and DuckDB/SQLite capabilities are compiled in. On Linux the
full GUI workflow is tested with no network at all (`npm run test:e2e:offline`).

```sh
npm run check
npm run test:frontend
cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check
node scripts/with-duckdb.mjs cargo test --locked --manifest-path src-tauri/Cargo.toml
node scripts/with-duckdb.mjs cargo clippy --locked --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
npm run tauri:build
```

GUI end-to-end tests (Linux) drive the real built app through WebDriver. They
need `tauri-driver` (`cargo install tauri-driver --locked`) and WebKitGTK's
`WebKitWebDriver` (Debian/Ubuntu: `webkit2gtk-driver`). Build with `tauri build`
first; a plain `cargo build` binary expects the Vite dev server. The tests open
Sivro windows on the current display.

```sh
npm run tauri:build -- --no-bundle
npm run test:e2e            # all seven formats in the real window
npm run test:e2e:offline    # the same, in a network namespace with loopback only
node scripts/perf-baseline.mjs   # 1M-row timings, peak RSS and scratch, per format
```

`npm run check` builds/typechecks the frontend and runs Rust checking. `npm run
build` builds only frontend assets. [Testing](docs/testing.md) describes every
suite and the performance baseline; [releasing](docs/releasing.md) describes
the release gates. CI runs the frontend, Rust and library checks plus the Linux
GUI suite on every pull request, and the macOS and Windows backend suites on
`main`.

## DuckDB linking

Keep the upstream build wrapper: `scripts/with-duckdb.mjs` stages official
prebuilt static DuckDB archives on Linux/macOS via `scripts/fetch-duckdb.mjs`.
The version comes from `libduckdb-sys` in `src-tauri/Cargo.lock`. Native DuckDB
1.5.6 corresponds to Rust crate 1.10506.0. `src-tauri/build.rs` links the engine,
extension loader, bundled extension archives and platform libraries. Generated
archives, link configuration and build outputs are ignored by Git.

Windows MSVC must build from source because the upstream prebuilt Windows
archives use MinGW. Use:

```sh
npm run tauri:dev -- --features source-bundled
npm run tauri:build -- --features source-bundled
node scripts/with-duckdb.mjs cargo test --locked --manifest-path src-tauri/Cargo.toml --features source-bundled
```

`source-bundled` enables DuckDB's `bundled`, `json` and `parquet` features:
bundled builds compile only the extensions requested (plus `core_functions`).
Do not add these features to the default build; they force source
compilation and change the static-archive link strategy.

## Library checks

The separate [library-checks crate](experiments/library-checks/README.md) pins
down library behaviour the readers rely on: raw JSON numeric fidelity,
array/JSONL output, DuckDB interruption, selected-sheet Calamine access and
read-only rusqlite WAL semantics.

```sh
node scripts/with-duckdb.mjs cargo test --locked --manifest-path experiments/library-checks/Cargo.toml -- --nocapture
```

See [architecture](docs/architecture.md), [testing](docs/testing.md) and the
[changelog](CHANGELOG.md) for design decisions, test coverage and limitations.

## License and attribution

MIT — see [LICENSE](LICENSE). Copyright (c) 2026 GoodApps is preserved.
The imported source snapshot is upstream commit
`63e0f53745d5606c484f775bd2b74d6b0fdca9bc` (ParquetPeek 0.1.1).
Sivro's changes are summarized in `CHANGELOG.md`.

Packages include `LICENSE` and [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md):
the license of every bundled Rust crate (for all release targets) and npm
package, the DuckDB 1.5.6 engine and its statically linked components
(including ICU and jemalloc), and SQLite 3.53.2 (public domain). The bundled
Inter and JetBrains Mono fonts are under the SIL Open Font License 1.1.
Regenerate the notices with `node scripts/third-party-notices.mjs` after any
dependency change. Excel and other test fixtures are synthetic.
