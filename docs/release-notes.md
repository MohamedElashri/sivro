Sivro is a local, read-only desktop explorer for **Parquet, CSV, TSV, JSON,
JSON Lines, Excel (xlsx, xlsm, xlsb, xls) and SQLite** files. It browses, filters,
sorts and profiles tables, inspects JSON documents, and exports the current view
to Parquet, CSV, TSV, a JSON array or JSON Lines. It never modifies a source file,
runs no macros or formulas, accepts no SQL, and makes no network requests.

This is a pre-1.0 beta. Download the package for your platform, then verify it
against `SHA256SUMS.txt`. Packages are not code-signed or notarized: on macOS
you may need to allow the app under System Settings → Privacy & Security, and
Windows SmartScreen may warn on first launch.

### Known limitations

- **Testing coverage:** the full GUI workflow is tested automatically only on
  Linux x86_64. Every required target runs the backend reader suites before
  packaging; macOS and Windows GUI checks are manual. The macOS package is a
  single universal app for Intel and Apple Silicon. ARM64 Linux and Windows
  builds are experimental.
- **JSON / JSON Lines tables** read the whole file when it opens, at about
  18 MiB/s on a recent laptop; inputs that take longer than 30 s to open are
  refused. JSON documents that aren't tables (a single object, mixed arrays)
  open as a tree, limited to 32 MiB. Numbers stay exact, but exponents are
  written as `e+`/`e-`.
- **Excel** reads only the values a workbook cached when it was saved;
  formulas are never recalculated. A worksheet may use at most 200,000 cells.
  `.xls` files are parsed in full, so that limit covers the whole workbook.
  Encrypted workbooks are not supported.
- **SQLite** opens read-only and shows other writers' commits live; a schema
  change asks for Refresh. A WAL-mode database needs its `-wal`/`-shm` files
  beside it; reading it in a writable folder may create them, and it cannot be
  read from a read-only folder without them. Views have no stable row order.
  Virtual tables are not opened.
- **CSV/TSV** preserve values, not original quoting or number formatting;
  `\N` is the NULL marker in CSV/TSV exports. Parquet export refuses columns
  that cannot be typed exactly (for example a SQLite column mixing numbers and
  text, or JSON numbers beyond 38 digits).
- **Windows** may refuse to delete or replace a file while it is open in
  Sivro; close its tab before another program rewrites it.
- **Resource limits** (page size, export row cap, scratch space, DuckDB
  memory) are product budgets, not a hard cap on the app's total memory.
