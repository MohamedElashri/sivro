# Formats and behaviour

Sivro opens local files read-only. Tables support search, filters, sorting,
profiles, copying and export. **Refresh** reopens changed files and retains
format selections. **Cancel work** stops operations. Filtered counts and
profiles are computed on demand.

| Format | Extensions | Opening |
| --- | --- | --- |
| Parquet | `.parquet`, `.pq` | Table with schema and row-group metadata |
| Arrow IPC / Feather V2 | `.arrow`, `.ipc`, `.arrows`, `.feather` | File or stream table |
| HDF5 | `.h5`, `.hdf5`, `.hdf` | Group/dataset browser |
| CSV / TSV | `.csv`, `.tsv` | Parsing preview |
| JSON | `.json` | Record table or document tree |
| JSON Lines | `.jsonl`, `.ndjson` | Record table |
| Excel | `.xlsx`, `.xlsm`, `.xlsb`, `.xls` | Worksheet/header picker |
| SQLite | `.sqlite`, `.sqlite3`, `.db` | Table/view picker |

## Exports

Exports retain the loaded columns, filters and sort, with warnings for capped
rows. Sources, aliases and existing files are never overwritten. Duplicate or
blank labels use column IDs as keys.

| Output | Value handling |
| --- | --- |
| CSV / TSV | NULL is `\N`; colliding literal values are rejected. Binary uses `hex:` text. |
| JSON / JSONL | Exact numeric tokens and nested JSON are retained; binary uses tagged hex objects. NaN/Infinity are rejected. |
| Parquet / Arrow | Native types and binary; wide integers must fit `DECIMAL(38,0)`. Exact JSON-number columns are rejected; nested JSON becomes text. |
| HDF5 | Native values plus annotations and null masks; see below. |

Copying preserves large numbers; NaN/Infinity copy as strings with a note.
Expanded cells and copied values are limited to 1 MiB.

## Arrow IPC and Feather V2

IPC files/streams support multiple batches, dictionaries, LZ4 and ZSTD.
Feather V1, Flight and tensor messages are unsupported. File details expose
the original schema and metadata.

Scalars and supported nested values use native query types. Other types,
including Decimal256, negative-scale decimals and certain temporal types, use
labelled text fallbacks. Dictionaries decode to values; extension types use
storage types. Unsupported nested children make the whole value text.

Exports use uncompressed IPC and current query types. Original dictionary
encoding, metadata, extension annotations and timezone labels are not retained.

## HDF5

Choose a dataset, axes, fixed indices and row window. Compound fields and
equal-length vectors can become columns. **Dataset:** changes the selection.
Scalars have one row; empty/null dataspaces have none.

Supports layout v3/v4/v5, modern chunk indexes, user blocks, deflate, shuffle,
Fletcher32, N-bit, scale-offset and LZF. Integers through 128 bits stay exact;
nested values use JSON, with non-finite floats as strings. Opaque, reference
and unsupported numeric values remain bytes. Strings require UTF-8.

Soft/external links, external/virtual storage and external filter plugins are
unsupported. pandas, PyTables and MATLAB object conventions are not reconstructed.

Exports use `/table/col-NNNN`, `/_sivro_validity` masks (0 NULL, 1 present) and
`sivro.*` attributes for names, types and encodings. Reopen `/table` as columns;
other tools must apply the annotations. Original hierarchy and metadata are
not preserved.

## CSV and TSV

Confirm delimiter, header, quoting, NULL marker and types in the preview.
UTF-8 supports an optional BOM. Inference samples 20,480 records; **Force text**
preserves numeric spelling. Floating point conversion may round values.
Malformed records and later type mismatches fail.

The default NULL marker is an unquoted `\N`; quoted `"\N"` and empty text remain
text. An empty marker makes unquoted empty fields NULL. Blank lines are skipped.
**Parsing settings** changes the configuration; Refresh retains it. Exports
preserve values, not original formatting.

## JSON and JSON Lines

A `.json` array of objects opens as a table; other roots use a document tree.
JSON Lines requires one value per nonblank line; object and scalar/array records
cannot be mixed. Documents retain duplicate keys, order and exact numbers;
exponents are normalized.

Tables validate every record. Exponents and oversized numbers use `JSON_NUMBER`:
equality compares text (`1.0` differs from `1`); sorting, ranges and min/max are
disabled. Nested/mixed values use JSON text. Missing fields and JSON `null` both
appear as NULL; **Show source record** exposes the source record.

## Excel

Choose a worksheet/header, or column letters without a header. **Sheet:**
changes the selection; Refresh retains it. Values use the saved cache;
formulas are not recalculated, macros do not run, and encryption is unsupported.

Numbers retain Excel's stored double precision. Dates use the workbook's
1900/1904 system at millisecond resolution; serial 60 and out-of-range dates
remain tagged values. Errors, uncached formulas and mixed-type columns use
tagged JSON text. Blank cells are NULL; empty text remains empty text.

## SQLite

Select a table or view; virtual/internal tables are unavailable. Reads reflect
other writers' commits; schema changes require Refresh. Cells retain storage
classes and exact 64-bit integers; invalid UTF-8 becomes bytes. Views have no
stable row order. Case-insensitive search applies to ASCII only.

Parquet, Arrow and HDF5 exports reject mixed storage classes or combinations
that would lose numeric precision. WAL databases require their `-wal`/`-shm`
sidecars; opening a writable directory may create them without changing the
database content.

## Default limits

| Input | Limits |
| --- | --- |
| Arrow | 256 MiB file; 128 MiB message/batch; 8 MiB metadata; 1 GiB cumulative decoded buffers |
| HDF5 | 4 GiB file; 64 MiB read/chunk/leading row; 8 MiB metadata; rank 32; 2 million selected cells; 1 million B-tree chunks |
| HDF5 export | 64 MiB each for collected cells and encoded values |
| JSON document | 32 MiB; nesting depth 64 |
| JSON record table | 512 MiB input; 16 MiB per JSON Lines record |
| Excel | 50 MiB workbook (`.xls`: 16 MiB); 200,000 cells; 128 MiB per expanded part and 256 MiB total |

Shared limits: 30 seconds per operation, 4,096 columns, 1 GiB scratch,
512 MiB DuckDB memory and 10 million exported rows. Large HDF5 planes or Excel
used ranges may exceed limits despite a small selection. These are operation
budgets, not a hard process-memory cap.
