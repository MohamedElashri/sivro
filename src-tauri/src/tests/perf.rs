//! Performance baseline (ignored in normal runs). `scripts/perf-baseline.mjs`
//! writes fixtures once, then measures each format in a fresh process so the
//! peak-RSS reading belongs to that format alone.
use super::hardening::open_any;
use super::*;

const ROWS: u64 = 1_000_000;

fn dir() -> PathBuf {
    PathBuf::from(std::env::var("SIVRO_PERF_DIR").expect("SIVRO_PERF_DIR"))
}

#[test]
#[ignore]
fn perf_write_fixtures() {
    let dir = dir();
    std::fs::create_dir_all(&dir).unwrap();
    let rows = format!(
        "SELECT i AS id, 'group-' || (i % 20) AS grp, (i % 100000) / 100.0 AS amount, 9007199254740993 + i AS big, 'label ' || md5(i::VARCHAR)[1:12] AS label FROM range({ROWS}) t(i)"
    );
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("SET threads=4; SET preserve_insertion_order=true;")
        .unwrap();
    for (name, options) in [
        ("rows.parquet", "FORMAT PARQUET"),
        ("rows.csv", "FORMAT CSV, HEADER true"),
        ("rows.tsv", "FORMAT CSV, HEADER true, DELIMITER '\t'"),
        ("rows.jsonl", "FORMAT JSON, ARRAY false"),
        ("rows.json", "FORMAT JSON, ARRAY true"),
    ] {
        conn.execute_batch(&format!(
            "COPY ({rows}) TO {} ({options})",
            sql::quote_literal(dir.join(name).to_str().unwrap())
        ))
        .unwrap();
    }
    let mut db = rusqlite::Connection::open(dir.join("rows.sqlite")).unwrap();
    db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE rows(id INTEGER PRIMARY KEY, grp TEXT, amount REAL, big INTEGER, label TEXT);").unwrap();
    let tx = db.transaction().unwrap();
    {
        let mut insert = tx
            .prepare("INSERT INTO rows VALUES (?, ?, ?, ?, ?)")
            .unwrap();
        for i in 0..ROWS as i64 {
            insert
                .execute(rusqlite::params![
                    i,
                    format!("group-{}", i % 20),
                    (i % 100_000) as f64 / 100.0,
                    9_007_199_254_740_993i64 + i,
                    format!("label {i:012x}")
                ])
                .unwrap();
        }
    }
    tx.commit().unwrap();
    db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap();
    // Excel's per-sheet budget is 200,000 cells: 20,000 rows x 10 columns.
    let mut sheet = String::with_capacity(16 << 20);
    for r in 1..=20_000u32 {
        sheet.push_str(&format!("<row r=\"{r}\">"));
        for c in 0..10u32 {
            let col = (b'A' + c as u8) as char;
            sheet.push_str(&format!(
                "<c r=\"{col}{r}\" t=\"n\"><v>{}.5</v></c>",
                r * 10 + c
            ));
        }
        sheet.push_str("</row>");
    }
    let ns = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
    let rel = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    let mut zip = zip::ZipWriter::new(std::fs::File::create(dir.join("rows.xlsx")).unwrap());
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, body) in [
        (
            "[Content_Types].xml".to_string(),
            r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"/>"#
                .to_string(),
        ),
        (
            "_rels/.rels".into(),
            format!(
                r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{rel}/officeDocument" Target="xl/workbook.xml"/></Relationships>"#
            ),
        ),
        (
            "xl/workbook.xml".into(),
            format!(
                r#"<workbook xmlns="{ns}" xmlns:r="{rel}"><sheets><sheet name="Rows" sheetId="1" r:id="rId1"/></sheets></workbook>"#
            ),
        ),
        (
            "xl/_rels/workbook.xml.rels".into(),
            format!(
                r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{rel}/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#
            ),
        ),
        (
            "xl/worksheets/sheet1.xml".into(),
            format!(r#"<worksheet xmlns="{ns}"><sheetData>{sheet}</sheetData></worksheet>"#),
        ),
    ] {
        use std::io::Write;
        zip.start_file(name, opts).unwrap();
        zip.write_all(body.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
}

fn peak_rss_kib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM:"))
                .and_then(|l| l.split_whitespace().nth(1).map(|v| v.parse().unwrap_or(0)))
        })
        .unwrap_or(0)
}
fn tree_bytes(path: &Path) -> u64 {
    std::fs::read_dir(path).map_or(0, |entries| {
        entries
            .flatten()
            .map(|e| match e.metadata() {
                Ok(m) if m.is_dir() => tree_bytes(&e.path()),
                Ok(m) => m.len(),
                Err(_) => 0,
            })
            .sum()
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn perf_measure() {
    let file = std::env::var("SIVRO_PERF_FILE").expect("SIVRO_PERF_FILE");
    let source = dir().join(&file);
    let work = Directory::new();
    let state = AppState::new(work.0.join("scratch"), ResourcePolicy::default()).unwrap();
    let rss_start = peak_rss_kib();
    let mut scratch_peak = 0u64;
    let mut steps: Vec<(String, u128)> = Vec::new();
    let mut step = |name: &str, started: std::time::Instant, work_dir: &Path| {
        steps.push((name.into(), started.elapsed().as_millis()));
        scratch_peak = scratch_peak.max(tree_bytes(&work_dir.join("scratch")));
    };
    let t = std::time::Instant::now();
    let info = open_any(&state, &source).await.unwrap();
    step("open", t, &work.0);
    let run_page = |q: Query| {
        let (s, info) = (state.clone(), info.clone());
        async move {
            s.run(
                request(&s, &info, OperationKind::Page, q),
                OperationKind::Page,
                |a, g, q, p, _| a.fetch_page(g, &q, p),
            )
            .await
            .unwrap()
        }
    };
    let t = std::time::Instant::now();
    assert_eq!(run_page(query(&info)).await.rows.len(), 100);
    step("first page", t, &work.0);
    let mut filtered = query(&info);
    filtered.filter = predicate(
        "col-0",
        FilterOp::Gte,
        FilterValue::Number {
            value: "500000".into(),
        },
    );
    let t = std::time::Instant::now();
    run_page(filtered.clone()).await;
    step("filtered page", t, &work.0);
    let t = std::time::Instant::now();
    let count = state
        .run(
            request(&state, &info, OperationKind::Count, filtered.clone()),
            OperationKind::Count,
            |a, g, q, p, _| a.count(g, &q, p),
        )
        .await
        .unwrap();
    step("filtered count", t, &work.0);
    let mut sorted = query(&info);
    sorted.sort = Some(SortSpec {
        column_id: "col-0".into(),
        descending: true,
    });
    let t = std::time::Instant::now();
    run_page(sorted).await;
    step("global sort page", t, &work.0);
    let t = std::time::Instant::now();
    let all = state
        .run(
            request(&state, &info, OperationKind::Count, query(&info)),
            OperationKind::Count,
            |a, g, q, p, _| a.count(g, &q, p),
        )
        .await
        .unwrap();
    step("exact count", t, &work.0);
    let total: u64 = match &all.count {
        CountState::Known { value } => value.parse().unwrap(),
        _ => ROWS,
    };
    let mut deep = query(&info);
    deep.offset = total.saturating_sub(100);
    let t = std::time::Instant::now();
    run_page(deep).await;
    step("high offset page", t, &work.0);
    let t = std::time::Instant::now();
    state
        .run(
            request(
                &state,
                &info,
                OperationKind::Profile,
                ProfileQuery {
                    column_id: "col-1".into(),
                    context: query(&info),
                },
            ),
            OperationKind::Profile,
            |a, g, q, p, _| a.profile(g, &q, p),
        )
        .await
        .unwrap();
    step("profile", t, &work.0);
    for (format, ext) in [
        (ExportFormat::Csv, "csv"),
        (ExportFormat::Parquet, "parquet"),
    ] {
        let view = query(&info);
        let destination = work.0.join(format!("out.{ext}"));
        let options = ExportOptions {
            destination: destination.to_str().unwrap().into(),
            format,
            max_rows: ROWS,
            view,
        };
        let t = std::time::Instant::now();
        let result = state
            .run(
                request(&state, &info, OperationKind::Export, options),
                OperationKind::Export,
                |a, g, q, p, c| a.export(g, q, p, c),
            )
            .await
            .unwrap();
        step(
            &format!("export {ext} ({} rows)", result.exported_rows),
            t,
            &work.0,
        );
    }
    state.close(info.session_id).await.unwrap();
    let rss_end = peak_rss_kib();
    let report = serde_json::json!({
        "file": file,
        "bytes": std::fs::metadata(&source).unwrap().len(),
        "rows": info.object.row_count,
        "filtered_count": count.count,
        "steps": steps,
        "peak_rss_kib": rss_end,
        "rss_before_open_kib": rss_start,
        "scratch_peak_bytes": scratch_peak,
    });
    println!("SIVRO_PERF {report}");
}
