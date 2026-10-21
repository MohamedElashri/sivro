# Library checks

This crate is separate from Sivro's production dependencies and command API.
It pins down library behaviour that the readers depend on. All input
is deterministic and synthetic; temporary files are removed by `tempfile`.
Only the SQLite fixture writer creates/updates a source database.

From the repository root:

```sh
node scripts/with-duckdb.mjs cargo test --locked --manifest-path experiments/library-checks/Cargo.toml -- --nocapture
cargo fmt --manifest-path experiments/library-checks/Cargo.toml --all -- --check
node scripts/with-duckdb.mjs cargo clippy --locked --manifest-path experiments/library-checks/Cargo.toml --all-targets -- -D warnings
```

On Windows MSVC, add `--features source-bundled` before `-- --nocapture`
(and before `-- -D warnings` for clippy). Linux/macOS use the app's staged
static archives. The small `build.rs` mirrors their link ordering without
pulling Tauri into the checks.

| Test | What it establishes |
|---|---|
| `raw_json_numbers_and_precision_preserving_fallback` | Compares raw record text and extracted integer/huge-integer/decimal tokens; Rust `arbitrary_precision` retains exact values when DuckDB conversion cannot |
| `json_array_and_jsonl_are_different_export_encodings` | Explicit `ARRAY true` yields an array; `ARRAY false` and upstream default yield JSONL, with runtime extension auto-install/load disabled |
| `duckdb_interrupt_handle_crosses_thread_boundary` | Interrupts actual expensive work from another thread and reuses the connection afterward |
| `calamine_reads_only_the_selected_xlsx_sheet_range` | Lists sheets, reads a 2×2 selected range and cached formula; defers a deliberately invalid unselected worksheet until requested |
| `sqlite_readonly_wal_observes_writer_and_rejects_writes` | Reads an exact integer in WAL mode, rejects writes, checks controlled main/WAL bytes, then sees a separate writer commit via `data_version` |

These are API and semantics checks. The application's own tests cover ZIP
preflight, resource policies, the readers, the UI and packaging. The Calamine
check does not bound allocations or cover all four Excel container variants,
and the SQLite check uses existing WAL/SHM sidecars without claiming SHM byte
immutability.
