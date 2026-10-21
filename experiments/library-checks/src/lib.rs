//! Executable checks of library behaviour the readers rely on. No
//! application commands or adapters.

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};
    use std::time::{Duration, Instant};

    use calamine::{Data, Reader, Xlsx};
    use duckdb::Connection;
    use rusqlite::{Connection as Sqlite, ErrorCode, OpenFlags};

    fn duckdb() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "SET autoinstall_known_extensions = false;
             SET autoload_known_extensions = false;
             SET memory_limit = '512MB'; SET threads = 2;",
        )
        .unwrap();
        conn
    }

    #[test]
    fn raw_json_numbers_and_precision_preserving_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("numbers.json");
        let integer = "9007199254740993";
        let huge = "12345678901234567890123456789012345678901234567890";
        let decimal = "0.123456789012345678901234567890";
        let record = format!(r#"{{"integer":{integer},"huge":{huge},"decimal":{decimal}}}"#);
        std::fs::write(&path, format!("[{record}]")).unwrap();
        let conn = duckdb();
        let (raw, extracted_integer, extracted_huge, extracted_decimal): (
            String,
            String,
            String,
            String,
        ) = conn
            .query_row(
                "SELECT json::VARCHAR, json_extract(json, '$.integer')::VARCHAR,
                        json_extract(json, '$.huge')::VARCHAR,
                        json_extract(json, '$.decimal')::VARCHAR
                 FROM read_json_objects(?, format = 'array')",
                [path.to_str().unwrap()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(raw, record, "raw scanner must preserve the source record");
        assert_eq!(extracted_integer, integer);
        println!("DuckDB extracted huge={extracted_huge}; decimal={extracted_decimal}");
        println!(
            "Exact extraction: huge={}, decimal={}",
            extracted_huge == huge,
            extracted_decimal == decimal
        );
        // The application can use structural Rust reading of raw records even
        // if a DuckDB numeric conversion/extraction proves lossy.
        let exact: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(exact["integer"].to_string(), integer);
        assert_eq!(exact["huge"].to_string(), huge);
        assert_eq!(exact["decimal"].to_string(), decimal);
    }

    #[test]
    fn json_array_and_jsonl_are_different_export_encodings() {
        let dir = tempfile::tempdir().unwrap();
        let conn = duckdb();
        let version: String = conn
            .query_row("SELECT version()", [], |r| r.get(0))
            .unwrap();
        println!("DuckDB engine {version}; automatic extension install/load disabled");
        for (filename, options, is_array) in [
            ("array.json", "FORMAT JSON, ARRAY true", true),
            ("lines.jsonl", "FORMAT JSON, ARRAY false", false),
            ("upstream.json", "FORMAT JSON", false),
        ] {
            let path = dir.path().join(filename);
            // Generated temp path only; production needs the export guards in
            // docs/architecture.md. This is not a production query compiler.
            let quoted = path.to_str().unwrap().replace('\'', "''");
            conn.execute_batch(&format!(
                "COPY (SELECT range::BIGINT AS id FROM range(2)) TO '{quoted}' ({options})"
            ))
            .unwrap();
            let text = std::fs::read_to_string(&path).unwrap();
            if is_array {
                let values: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap();
                assert_eq!(values.len(), 2);
                assert_eq!(values[1]["id"], 1);
            } else {
                let values: Vec<serde_json::Value> = text
                    .lines()
                    .map(|line| serde_json::from_str(line).unwrap())
                    .collect();
                assert_eq!(values.len(), 2);
                assert_eq!(values[1]["id"], 1);
                assert!(serde_json::from_str::<serde_json::Value>(&text).is_err());
            }
        }
    }

    #[test]
    fn duckdb_interrupt_handle_crosses_thread_boundary() {
        let conn = duckdb();
        let interrupt = conn.interrupt_handle();
        let worker = std::thread::spawn(move || {
            let result = conn.query_row::<String, _, _>(
                "SELECT sum(a.range * b.range)::VARCHAR FROM range(1000000000) a, range(1000000000) b",
                [], |r| r.get(0),
            );
            (conn, result)
        });
        let start = Instant::now();
        // Repeated signals cover the race where the worker has not started
        // executing yet. One operation owns the connection throughout.
        while !worker.is_finished() && start.elapsed() < Duration::from_secs(5) {
            interrupt.interrupt();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            worker.is_finished(),
            "DuckDB did not acknowledge interruption within 5s"
        );
        let (conn, result) = worker.join().unwrap();
        assert!(result
            .unwrap_err()
            .to_string()
            .to_lowercase()
            .contains("interrupt"));
        assert_eq!(
            conn.query_row::<i64, _, _>("SELECT 42", [], |r| r.get(0))
                .unwrap(),
            42
        );
        println!(
            "DuckDB interrupted and connection reused after {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn calamine_reads_only_the_selected_xlsx_sheet_range() {
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let entries = [
            (
                "[Content_Types].xml",
                r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/></Types>"#,
            ),
            (
                "_rels/.rels",
                r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#,
            ),
            (
                "xl/workbook.xml",
                r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Wanted" sheetId="1" r:id="rId1"/><sheet name="Unread" sheetId="2" r:id="rId2"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1:B2"/><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>id</t></is></c><c r="B1" t="inlineStr"><is><t>cached</t></is></c></row><row r="2"><c r="A2"><v>7</v></c><c r="B2"><f>A2*2</f><v>14</v></c></row></sheetData></worksheet>"#,
            ),
            // Explicit numeric type matters: absent a type, Calamine falls
            // back to a string when numeric parsing fails.
            (
                "xl/worksheets/sheet2.xml",
                r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="n"><v>not-a-number</v></c></row></sheetData></worksheet>"#,
            ),
        ];
        for (name, text) in entries {
            archive
                .start_file(
                    name,
                    zip::write::SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Stored),
                )
                .unwrap();
            archive.write_all(text.as_bytes()).unwrap();
        }
        let bytes = archive.finish().unwrap().into_inner();
        let mut workbook: Xlsx<_> = Xlsx::new(Cursor::new(bytes)).unwrap();
        assert_eq!(workbook.sheet_names(), ["Wanted", "Unread"]);
        let range = workbook.worksheet_range("Wanted").unwrap();
        assert_eq!(range.get_size(), (2, 2));
        assert_eq!(range.get((1, 0)), Some(&Data::Float(7.0)));
        assert_eq!(range.get((1, 1)), Some(&Data::Float(14.0)));
        assert_eq!(
            workbook
                .worksheet_formula("Wanted")
                .unwrap()
                .get((0, 0))
                .unwrap(),
            "A2*2"
        );
        assert!(workbook.worksheet_range("Unread").is_err());
        println!("Calamine 0.37.0: selected 2x2 range, cached formula=14; unselected invalid sheet deferred");
    }

    #[test]
    fn sqlite_readonly_wal_observes_writer_and_rejects_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("live.sqlite");
        let writer = Sqlite::open(&path).unwrap();
        writer
            .execute_batch(
                "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
            CREATE TABLE records(value INTEGER); INSERT INTO records VALUES (9007199254740993);",
            )
            .unwrap();
        let reader = Sqlite::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .unwrap();
        assert!(reader.is_readonly("main").unwrap());
        reader.execute_batch("PRAGMA query_only=ON").unwrap();
        let bytes_before = std::fs::read(&path).unwrap();
        let wal_path = path.with_extension("sqlite-wal");
        let wal_before = std::fs::read(&wal_path).unwrap();
        assert_eq!(
            reader
                .query_row::<i64, _, _>("SELECT value FROM records", [], |r| r.get(0))
                .unwrap(),
            9007199254740993
        );
        let err = reader
            .execute("INSERT INTO records VALUES (1)", [])
            .unwrap_err();
        assert_eq!(err.sqlite_error_code(), Some(ErrorCode::ReadOnly));
        assert_eq!(std::fs::read(&path).unwrap(), bytes_before);
        assert_eq!(std::fs::read(&wal_path).unwrap(), wal_before);
        let version: i64 = reader
            .query_row("PRAGMA data_version", [], |r| r.get(0))
            .unwrap();
        writer
            .execute("INSERT INTO records VALUES (2)", [])
            .unwrap();
        let changed: i64 = reader
            .query_row("PRAGMA data_version", [], |r| r.get(0))
            .unwrap();
        assert_ne!(changed, version);
        assert_eq!(
            reader
                .query_row::<i64, _, _>("SELECT count(*) FROM records", [], |r| r.get(0))
                .unwrap(),
            2
        );
        let interrupt = reader.get_interrupt_handle();
        interrupt.interrupt(); // API check; DuckDB experiment tests active cancellation.
        println!("rusqlite 0.40.2 / SQLite {}: read-only WAL, write rejected, data_version {} -> {}; sidecars existed with writer open",
            rusqlite::version(), version, changed);
    }
}
