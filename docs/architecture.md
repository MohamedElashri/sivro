# Sivro architecture

Sivro is a Tauri 2 desktop app with a Rust backend and a vanilla
TypeScript/Vite frontend. It starts from MIT-licensed ParquetPeek commit
`63e0f53745d5606c484f775bd2b74d6b0fdca9bc` and keeps its visual design. Every
input family uses one session API. Parquet, CSV, TSV, JSON/JSONL record
tables and Excel tables run on DuckDB; JSON documents use a bounded in-memory
tree; SQLite runs on its own read-only rusqlite connection. Release gates are
in [releasing.md](releasing.md) and test coverage is in [testing.md](testing.md).

## Inputs and outputs

| Family | Extensions | Reader and execution |
|---|---|---|
| Parquet | `.parquet`, `.pq` | Parquet metadata crate; DuckDB query engine |
| CSV | `.csv` | DuckDB with frozen, user-confirmed parsing options and schema |
| TSV | `.tsv` | Same adapter, tab separator by default |
| JSON | `.json` | serde_json tree (document) or streamed record array staged as text |
| JSON Lines | `.jsonl`, `.ndjson` | Validated line stream staged as text |
| Excel | `.xlsx`, `.xls`, `.xlsm`, `.xlsb` | Container preflight; Calamine streams one worksheet (XLS: whole workbook); staged as text |
| SQLite | `.sqlite`, `.sqlite3`, `.db` | Valid SQLite header; rusqlite read-only connection and SQLite-specific compiler |

Extensions are hints, not permission to parse arbitrary content. Signatures
are checked (Parquet magic and footer, SQLite header, ZIP or CFB workbooks,
UTF-8 text). JSON and JSON Lines are validated as a whole. A mismatch between
them produces `format_mismatch` and an explicit reopen choice; nothing is
reinterpreted silently.

Outputs are Parquet, CSV, TSV, a JSON array (`COPY ... ARRAY true`, or a
streaming writer) and JSON Lines. There are no Excel or SQLite writers.
Non-tabular JSON supports inspection and copy, not table export.

## Exact types and the wire contract

Columns have backend-owned stable IDs, and cells are tagged. Exact integers
and decimals travel as text with their type, never through a JavaScript
`number`. Database NULL, JSON null, Excel blank, empty text, false and zero
stay distinct wherever the source distinguishes them. BLOB cells carry their
byte length and a 128-byte hex preview. Structured cells carry JSON text.
Counts are known, unknown, pending or failed; a sample is never reported as
an exact total.

Previews are bounded to 4 KiB per cell. Full-value expansion and copy are
capped at 1 MiB, and larger values need export. The frontend keeps page data,
its column mapping, the selected object and the request generation together,
so a late response cannot install rows under another schema or tab.

## Sessions, execution and cancellation

The frontend submits typed filters, validated column IDs, sorting and paging.
It never submits SQL, source expressions or object names. Rust owns schema
discovery, identifier allow-lists, dialect-specific quoting and bound values.

A session owns:
- the canonical source identity and fingerprint;
- the frozen options and schema, and the selected object;
- a generation number;
- the execution connection and its interrupt handle;
- a scratch directory and caches.

Operations are reserved first (`begin_operation`), so a cancel during
admission is safe. Each session has one serialized execution lane, and at most
two expensive operations run globally. A soft time budget of 30 seconds
interrupts native work through DuckDB's or SQLite's interrupt handle, and
generations stop a late cancel from hitting a newer operation.

Close removes a session before draining its work. Refresh invalidates the
generation before replacing the connection, schema and caches, and a failed
refresh leaves no usable old catalog. Native work drains at exit before
scratch is cleaned. Table work goes through the `TableSource` trait, which
`DuckDbAdapter` and `SqliteAdapter` implement.

The first page doesn't wait for an exact filtered count; counts and profiles
are lazy and cached per generation and view. An unsorted DuckDB view keeps
its insertion-order stream. A streaming `row_number() OVER ()` provides a
source ordinal for row expansion and sort tie-breaks.

Source changes are checked at operation boundaries using file identity, size
and mtime, and become sticky until an explicit refresh. SQLite tracks
identity only, plus `schema_version`; committed data changes (`data_version`)
are live and only clear its caches.

## Libraries

- **DuckDB:** `duckdb`/`libduckdb-sys` 1.10506.0, which is engine 1.5.6.
  Linux and macOS link the official prebuilt static archives; Windows MSVC
  compiles from source with `source-bundled`, which enables `bundled`, `json`
  and `parquet`. A bundled build compiles only the requested extensions plus
  `core_functions`.
- **Calamine 0.37.0** with its own pinned `zip` 9.0.5, default features off.
  No VBA, external links or formula evaluation are ever requested.
- **rusqlite 0.40.2** with bundled SQLite 3.53.2, features `bundled` and
  `hooks` only.
- **serde_json** with `arbitrary_precision` for exact JSON number tokens.
- **Tauri 2.12** requires Rust 1.90.

## CSV and TSV

`preview_delimited` validates a 64 KiB UTF-8 prefix (BOM allowed). It then
runs DuckDB's bounded dialect sniff and a raw VARCHAR sample. The defaults are
a comma for CSV and a tab for TSV; `auto` is available. Header detection can
be overridden, and quote and escape default to `"`. Strict parsing reports
malformed rows; it never skips them.

Rust infers types from at most 20,480 raw records without f64: exact integer
widths, DECIMAL up to precision 38 (always keeping one integer digit),
booleans, and VARCHAR otherwise. Dates and exponents stay text unless
overridden. Force-text and per-column overrides are available; DOUBLE warns
about rounding. Queries read a generated VARCHAR schema with guarded casts,
so a later value that would round or truncate fails instead.

A five-minute backend preview token binds the path, fingerprint, options and
schema. Opening consumes it once, refresh reuses the confirmed schema, and a
changed header requires a new preview. Duplicate and blank headers keep
distinct column IDs; ambiguous labels make exports and copies use IDs. The
NULL marker is an unquoted `\N`: quoted matches stay text, and exports refuse
a literal `\N`.

## JSON and JSON Lines

DuckDB's `json_extract` rounds long decimals, so record fields are not
projected with DuckDB JSON functions. Instead, `adapters/json.rs` parses into
an ordered node type that keeps raw number tokens, key order and repeated
keys, with nesting limited to 64. A value budget of source bytes ÷ 32 bounds
tree memory. serde_json writes exponents as `e+`/`e-`; that is the only
change to a token's spelling.

A `.json` root array is streamed element by element, each within 16 MiB. If
every element is an object with fields, the array is a table. Any other root
is a document of at most 32 MiB. `read_document` returns up to 100 children
per request and copies one node as formatted JSON up to 1 MiB. JSON Lines
skips blank lines, requires exactly one value per line and reports 1-based
line numbers.

Tables take two passes:
1. Every record is read to infer each field's type, up to 4,096 columns.
   Extra keys stay only in the source record.
2. Text values and the source record are appended to `records.duckdb` in
   session scratch.

A fixed view applies exact casts to `BIGINT`, `HUGEINT`, `DECIMAL(p,s)`,
`BOOLEAN` or `VARCHAR`. Wider or exponent numbers stay exact text as
`JSON_NUMBER`, and nested or mixed fields as `JSON_VALUE`. Both are
unsortable and support only exact-text comparisons. Staging needs twice the
source size within the scratch budget, and the open fails if the source
changes between passes.

JSON/JSONL exports from these tables use a streaming token writer. Parquet
refuses `JSON_NUMBER`. For every reader, `HUGEINT`/`UHUGEINT` are written to
Parquet as `DECIMAL(38,0)`, because DuckDB's Parquet writer would otherwise
store them as `DOUBLE`.

## Excel

The registry requires a ZIP signature for `.xlsx`/`.xlsm`/`.xlsb` and a CFB
signature for `.xls`. `inspect_workbook` lists sheets with their kind and
visibility. `OpenOptions.excel` (a sheet plus a 1-based header row, or none)
is frozen per session.

Preflight runs before Calamine. ZIP central directories are limited to 10,000
entries and no encryption, 50 MiB compressed, 128 MiB per part and 256 MiB in
total. Parts over 1 MiB that compress more than 1,000:1 are refused, and zip 9
enforces declared sizes while reading.

Calamine's `Xls::new` parses every sheet and reserves each sheet's declared
DIMENSIONS before reading cells, so `preflight::xls` repeats Calamine 0.37's
own CFB traversal read-only:
- DIFAT, FAT and directory chains, and the mini stream when the mini FAT is
  non-empty;
- Workbook/Book/WORKBOOK/BOOK name precedence, NUL-truncated names and 64-bit
  lengths;
- BoundSheet8 offsets, CONTINUE merging and nested substreams.

It refuses cyclic or overlong chains, which Calamine would follow without
bound. The total of each sheet's DIMENSIONS reservation or cell bounding box,
whichever is larger, must fit the 200,000-cell budget. Encrypted workbooks are
refused.

XLSX/XLSB sheets stream through Calamine's cell readers; no `Range` is built.
Positions outside Excel's grid are damaged-file errors, and the used bounding
box is checked on every cell, so A1 plus XFD1048576 fails after two cells.
Calamine runs inside `catch_unwind`. Values are converted explicitly:
- whole numbers up to 2^53 become integers; other numbers stay binary
  doubles;
- dates follow the workbook's 1900 or 1904 epoch at millisecond resolution;
- the fictional 1900-02-29, out-of-range serials, errors, durations and
  formulas without a cached value are tagged JSON;
- blank cells are NULL, and empty text stays text.

Each column takes one native type; anything mixed stays `JSON_VALUE` with a
`mixed_types` warning. The table is staged through `adapters/stage.rs`.

## SQLite

The connection is opened `SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_NO_MUTEX`, with
`query_only=ON`, `trusted_schema=OFF` and a busy timeout of at most 2 s. An
authorizer allows only SELECT, reads of `main`, recursive CTEs, functions
other than `load_extension`/`fts3_tokenizer`, and five read-only pragmas.

Objects come from `PRAGMA table_list`: internal and shadow tables are hidden,
and virtual tables are listed but refused. Columns come from
`pragma_table_xinfo(?, 'main')`. Queries read a CTE `sivro_src(c0..cn)` whose
column list renames positionally. The stable key is the first unshadowed
rowid alias, the primary key for WITHOUT ROWID tables (tagged exact row IDs),
or otherwise a `row_number()` scan position with a warning. Sorts break ties
by that key.

`query/sqlite.rs` derives kinds from SQLite affinity and binds every value.
Text matching uses escaped `LIKE`, which folds ASCII only, and BLOBs never
match. Cells keep their runtime storage class. Invalid UTF-8 text is carried
as bytes, and empty BLOBs stay BLOBs.

A read-only connection may create empty `-wal`/`-shm` files beside a WAL
database, and Sivro reports when that happens. In a read-only folder without
them, the database cannot be read; `immutable=1` is never used.

Exports stream rows: CSV/TSV with `\N` NULLs and `hex:` BLOBs; JSON/JSONL
with exact integers and tagged BLOBs; Parquet after a storage-class pass that
refuses mixed columns.

## Exports and resources

Exports cover the selected view: visible columns in order, filters, quick
search and sort, with a validated row cap and a truncation warning. Sources
and their aliases (symlinks, hardlinks) are refused. Output is staged in a
private directory beside the destination, size and free space are watched,
and the finished file is published with a no-clobber hard link; existing
files are never replaced. Cancellation and failure clean up. CSV/TSV NULL
collisions and non-standard JSON (NaN) are errors.

`policy.rs` holds the `ResourcePolicy`, which can only be reduced:
- pages of 100 rows by default, 1,000 maximum;
- a 20,480-record parsing sample;
- JSON documents of 32 MiB and depth 64, and records of 16 MiB;
- 1 GiB of scratch per session, and 512 MiB of DuckDB memory;
- 200,000 Excel cells per sheet;
- an export cap of 100,000 rows by default and 10,000,000 maximum;
- 32 sessions, 4,096 columns, and 8 MiB page payloads.

These are product budgets, not a cap on process memory. Scratch is cleaned
on close, cancellation, startup recovery and shutdown.

## Offline operation and isolation

The app needs no network:
- **Assets:** fonts are bundled (`@fontsource`, OFL-1.1), and the CSP allows
  only `'self'`, `data:` images and fonts, inline styles and Tauri IPC.
- **DuckDB:** it loads only its statically linked extensions (`json`,
  `parquet`, `icu`, `core_functions`, `autocomplete`), with auto-install and
  auto-load off. A remote URL fails for lack of `httpfs` before any download.
- **Linux linking:** the executable is linked with `--exclude-libs,ALL`, so
  bundled SQLite and DuckDB symbols stay private, and WebKitGTK keeps using
  the system SQLite.

`tests/e2e/run.mjs` verifies all seven families in the real app with and
without network.

## Known limits

- Parser allocations are bounded by the limits above, not by process
  isolation. Calamine still parses shared strings and styles in full.
- JSON/JSONL open time is linear (about 18 MiB/s on a 2022 laptop CPU) and
  ends at the 30 s budget.
- Views in SQLite, and DuckDB sorts on equal keys without a stable key, can
  reorder between pages when the source changes.
- Interrupted exports are not journaled; scratch recovery uses filesystem
  leases.
