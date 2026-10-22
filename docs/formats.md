# Formats and behaviour

Sivro reads Parquet (`.parquet`, `.pq`), CSV (`.csv`), TSV (`.tsv`), JSON
(`.json`), JSON Lines (`.jsonl`, `.ndjson`), Excel (`.xlsx`, `.xlsm`, `.xlsb`,
`.xls`) and SQLite (`.sqlite`, `.sqlite3`, `.db`). It never modifies a source.
Release limitations are summarized in the [changelog](../CHANGELOG.md).

## Common workflow

Every format shares one workflow: multiple tabs, recent files, drag/drop,
schema and row-group details, a paged grid, search, filters, sorting, column
profiles, nested row inspection, copying and a 1,500-row sample generator.
Exports support Parquet, CSV, TSV, **JSON arrays** and **JSON Lines**. Queries
use backend-owned sessions and column IDs; exact numbers travel as tagged text. The first page uses the Parquet metadata
count for an unfiltered view. Filtered views offer **Count rows** on demand, while profiles
are computed when requested. **Refresh** explicitly reopens a changed source
and resets its view; **Cancel work** interrupts queued or active operations.

## Exports

Exports preserve the loaded projection, filters and sort. A row-cap warning
identifies omitted rows. Existing destinations, source paths and aliases are
never overwritten; choose a new filename. CSV/TSV use `\N` for NULL and reject a
literal value equal to that marker. Binary cells export as `hex:` text in CSV/TSV
and `{ "$binary_hex": "...", "byteCount": "..." }` in JSON/JSONL. JSON exports reject
non-finite numbers; Parquet retains native types. Row copying keeps large
numbers as JSON numbers and uses strings for NaN/Infinity with a visible note.
Grid previews are bounded; expansion/copy is capped at 1 MiB.

## CSV and TSV

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

## JSON and JSON Lines

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

## Excel

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

## SQLite

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
