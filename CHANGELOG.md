# Changelog

## 0.1.1

- Replaced the warm-paper theme with contrast-adjusted Catppuccin Latte
  (light) and Mocha (dark), with saved Light/Dark/System appearance controls
  and live system-theme changes. Updated the grid, dialogs, sidebar, buttons,
  scrollbars, focus outlines, and semantic status colors in both modes.
- Rebuilt dropdown popups with visible pointer and keyboard highlights,
  selected-item checkmarks, arrow/Home/End navigation, typeahead and Escape
  dismissal. Sheet and database lists now have hover and selected states.
- Fixed duplicate Linux file pickers by initializing event handlers once,
  preventing overlapping picker requests and ignoring repeated open shortcuts.
- Added a Sublime-style filled tab dot for unapplied search/filter changes;
  a separate warning marks sources changed on disk. Sources remain read-only.
- Added About with the running app version, author, platform, MIT license,
  source code, releases, issue tracker and author website.
- Added manual GitHub update checks, release notes, explicit installation
  confirmation, download progress, signature and signed-version verification,
  error recovery and an explicit restart action on macOS/Linux. Windows
  launches its installer and restarts after the user confirms installation.
  No checks, downloads or installations run automatically.
- Built-in updates support macOS, Windows and Linux AppImage. Other Linux
  packages and development builds offer checks and release download links.
- Release builds now generate signed updater assets. Publication validates
  the required platform assets and creates one complete `latest.json` after
  all platform builds finish, with installer-specific Windows entries.
- **Upgrade from 0.1.0:** install 0.1.1 manually; 0.1.0 has no updater.

### Known limitations

- This is a pre-1.0 beta. Verify downloads against `SHA256SUMS.txt`.
  Updater signatures do not provide Apple notarization or Windows
  Authenticode signing; macOS and Windows may warn on first launch.
- Automated GUI coverage is Linux x86_64; macOS and Windows GUI checks
  are manual. Required targets run backend tests before packaging. macOS
  ships a universal app; ARM64 Linux and Windows builds are experimental.
- JSON/JSON Lines tables scan the whole input when opening; inputs taking
  longer than 30 seconds are refused. See [Formats and behaviour](docs/formats.md)
  for format-specific limits, including Excel cached values, SQLite WAL
  sidecars and exact export types.
- Windows may refuse to delete or replace an open source file. Close its
  tab before another program rewrites it.
- Resource budgets for pages, exports, scratch space and DuckDB are not
  a hard cap on the app's total memory.

## 0.1.0

First release of Sivro, a local, read-only desktop explorer for tabular data.

### Inputs

- **Parquet**, with versioned session commands, opaque object and operation
  handles, stable column IDs, tagged exact cell values and structured errors.
  Row totals come from metadata; filtered counts are lazy.
- **CSV and TSV**, with a required parsing preview, UTF-8/BOM validation,
  configurable dialect and NULL rules, force-text and per-column type
  overrides. Confirmed schemas are frozen, and lossy numeric or temporal
  conversions are rejected. Duplicate and blank headers keep independent
  column IDs.
- **JSON and JSON Lines.** Root arrays of objects and JSONL records open as
  tables typed from every record; other JSON roots open in a bounded, lazily
  paged document tree with exact-token copy. Numbers stay exact serde_json
  tokens, in key order with repeated keys kept. Inputs are validated in full,
  with line-numbered errors, depth/size/record/value limits and an explicit
  JSON ↔ JSON Lines reopen after a format mismatch.
- **Excel** (`.xlsx`, `.xlsm`, `.xlsb`, `.xls`) through Calamine 0.37.0. A
  workbook dialog lists sheets with visibility and type and takes a header row
  or no header. Only cached values are read: formulas are never recalculated
  and macros never run. ZIP containers are checked against size, part, total
  and compression-ratio limits, and `.xls` files go through a read-only
  CFB/BIFF preflight before parsing. Columns are typed natively, both date
  systems are handled, and mixed columns, errors and uncached formulas are
  kept as tagged JSON text.
- **SQLite** (`.sqlite`, `.sqlite3`, `.db`) through rusqlite 0.40.2 with
  bundled SQLite 3.53.2. A table/view picker hides internal and shadow tables
  and refuses virtual tables. Connections are read-only, with `query_only`,
  `trusted_schema=OFF`, a busy timeout and a read-only authorizer. Each cell
  keeps its storage class, and pages are read live, resetting cached counts
  when another writer commits.

### Exports

- Parquet, CSV, TSV, JSON array and JSON Lines from every table source.
  Exports are published atomically without replacing existing files, guard
  against writing over the source, mark row-cap truncation and preserve exact
  numbers. `HUGEINT` columns are written to Parquet as `DECIMAL(38,0)`, and
  lossy Parquet output of raw JSON numbers or mixed SQLite columns is refused.

### Safety and resources

- Backend resource budgets, serialized session lanes, two-operation
  admission, source fingerprints with explicit refresh, cancellation with
  interrupt deadlines, and owned scratch cleanup.
- Fully offline: bundled Inter and JetBrains Mono fonts (OFL-1.1), a content
  security policy limited to the app and Tauri IPC, and private bundled
  SQLite/DuckDB symbols on Linux.
- Packages include `LICENSE`.

### Testing and release

- Backend tests for every reader, including precision fixtures, malformed and
  hostile inputs, resource limits, cancellation and repeated open/close;
  frontend tests with mocked IPC; library checks for DuckDB, Calamine and
  rusqlite; and GUI end-to-end tests of the built app, including a run with no
  network. A 1M-row performance baseline is available as `make perf`.
- Releases run backend tests on every target before packaging and GUI tests
  on Linux x86_64, publish `SHA256SUMS.txt`, and require approval before
  publishing. ARM64 Linux and Windows builds are experimental.
- The minimum supported Rust version is 1.90, required by Tauri 2.12.1.
