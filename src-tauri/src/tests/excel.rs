//! Excel coverage. Committed .xlsx/.xls/.xlsm fixtures come from
//! tests/fixtures/excel/generate.py (LibreOffice); .xlsb and edge-case
//! workbooks are built here because no available tool writes them.
use super::*;
use crate::adapters::excel::{column_letters, datetime_for_test};
use std::io::Write;

fn fixture_path(ext: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../tests/fixtures/excel/workbook.{ext}"))
}
fn copy_fixture(dir: &Directory, ext: &str) -> PathBuf {
    let path = dir.0.join(format!("book.{ext}"));
    std::fs::copy(fixture_path(ext), &path).unwrap();
    path
}
async fn open_sheet(
    state: &Arc<AppState>,
    path: &Path,
    sheet: &str,
    header_row: Option<u32>,
) -> Result<DatasetDescriptor, AppError> {
    state
        .open(
            path.to_str().unwrap().into(),
            OpenOptions {
                excel: Some(ExcelOptions {
                    sheet: sheet.into(),
                    header_row,
                }),
                ..Default::default()
            },
        )
        .await
}
async fn inspect(state: &Arc<AppState>, path: &Path) -> Result<WorkbookInfo, AppError> {
    state.inspect_workbook(path.to_str().unwrap().into()).await
}
fn values(page: &PageResult) -> Vec<Vec<String>> {
    page.rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|c| match c {
                    Cell::Null => "NULL".into(),
                    Cell::Boolean { value } => value.to_string(),
                    other => text(other).unwrap().to_string(),
                })
                .collect()
        })
        .collect()
}
fn types(info: &DatasetDescriptor) -> Vec<(String, String)> {
    info.object
        .columns
        .iter()
        .map(|c| (c.name.clone(), c.data_type.clone()))
        .collect()
}
async fn export_to(
    state: &Arc<AppState>,
    info: &DatasetDescriptor,
    path: &Path,
    format: ExportFormat,
    view: Query,
) -> Result<ExportResult, AppError> {
    state
        .run(
            request(
                state,
                info,
                OperationKind::Export,
                ExportOptions {
                    destination: path.to_str().unwrap().into(),
                    format,
                    max_rows: 1_000,
                    view,
                },
            ),
            OperationKind::Export,
            |a, g, q, p, c| a.export(g, q, p, c),
        )
        .await
}

// ---------------------------------------------------------------------------
// Workbook builders
// ---------------------------------------------------------------------------

const NS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

fn zip_bytes(entries: &[(&str, Vec<u8>)], deflate: bool) -> Vec<u8> {
    let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let method = if deflate {
        zip::CompressionMethod::Deflated
    } else {
        zip::CompressionMethod::Stored
    };
    for (name, data) in entries {
        archive
            .start_file(
                *name,
                zip::write::SimpleFileOptions::default()
                    .compression_method(method)
                    .large_file(data.len() > u32::MAX as usize),
            )
            .unwrap();
        archive.write_all(data).unwrap();
    }
    archive.finish().unwrap().into_inner()
}

/// Sheets are (name, state, sheetData XML, is_chartsheet).
fn xlsx(
    sheets: &[(&str, &str, String, bool)],
    date1904: bool,
    extra: Vec<(&str, Vec<u8>)>,
) -> Vec<u8> {
    let mut entries = vec![
        ("[Content_Types].xml", r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/></Types>"#.as_bytes().to_vec()),
        ("_rels/.rels", format!(r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{REL}/officeDocument" Target="xl/workbook.xml"/></Relationships>"#).into_bytes()),
        // 0 general, 1 date, 2 date-time, 3 time, 4 elapsed duration.
        ("xl/styles.xml", format!(r#"<styleSheet xmlns="{NS}"><numFmts count="1"><numFmt numFmtId="164" formatCode="yyyy-mm-dd hh:mm:ss"/></numFmts><cellXfs count="5"><xf numFmtId="0"/><xf numFmtId="14"/><xf numFmtId="164"/><xf numFmtId="21"/><xf numFmtId="46"/></cellXfs></styleSheet>"#).into_bytes()),
    ];
    let mut book = format!(r#"<workbook xmlns="{NS}" xmlns:r="{REL}">"#);
    if date1904 {
        book.push_str(r#"<workbookPr date1904="1"/>"#);
    }
    book.push_str("<sheets>");
    let mut rels = String::from(
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
    );
    let mut parts = Vec::new();
    for (i, (name, state, data, chart)) in sheets.iter().enumerate() {
        let n = i + 1;
        book.push_str(&format!(
            r#"<sheet name="{name}" sheetId="{n}" state="{state}" r:id="rId{n}"/>"#
        ));
        let (kind, folder) = if *chart {
            ("chartsheet", "chartsheets")
        } else {
            ("worksheet", "worksheets")
        };
        rels.push_str(&format!(
            r#"<Relationship Id="rId{n}" Type="{REL}/{kind}" Target="{folder}/sheet{n}.xml"/>"#
        ));
        parts.push((
            format!("xl/{folder}/sheet{n}.xml"),
            format!(r#"<{kind} xmlns="{NS}"><sheetData>{data}</sheetData></{kind}>"#),
        ));
    }
    book.push_str("</sheets></workbook>");
    rels.push_str(&format!(
        r#"<Relationship Id="rIdS" Type="{REL}/styles" Target="styles.xml"/></Relationships>"#
    ));
    entries.push(("xl/workbook.xml", book.into_bytes()));
    entries.push(("xl/_rels/workbook.xml.rels", rels.into_bytes()));
    let parts: Vec<(String, Vec<u8>)> = parts
        .into_iter()
        .map(|(p, d)| (p, d.into_bytes()))
        .collect();
    let mut all: Vec<(&str, Vec<u8>)> = entries;
    for (p, d) in &parts {
        all.push((p.as_str(), d.clone()));
    }
    all.extend(extra);
    zip_bytes(&all, false)
}

/// MS-XLSB record: 7-bit variable-length type and size.
fn record(out: &mut Vec<u8>, typ: u16, data: &[u8]) {
    if typ < 0x80 {
        out.push(typ as u8);
    } else {
        out.push((typ & 0x7F) as u8 | 0x80);
        out.push((typ >> 7) as u8);
    }
    let mut len = data.len();
    loop {
        let byte = (len & 0x7F) as u8;
        len >>= 7;
        out.push(if len > 0 { byte | 0x80 } else { byte });
        if len == 0 {
            break;
        }
    }
    out.extend_from_slice(data);
}
fn wide(text: &str) -> Vec<u8> {
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut out = (units.len() as u32).to_le_bytes().to_vec();
    units.iter().for_each(|u| out.extend(u.to_le_bytes()));
    out
}
enum B {
    Num(f64),
    /// A number at column 0xFFFFFFFF; Calamine's XLSB reader does not bound columns.
    Far(f64),
    Str(&'static str),
    Bool(bool),
    Err(u8),
}
/// Sheets are (name, hsState, rows, is_chartsheet).
fn xlsb(sheets: &[(&str, u32, Vec<Vec<B>>, bool)]) -> Vec<u8> {
    let mut book = Vec::new();
    record(&mut book, 0x0083, &[]);
    record(&mut book, 0x008F, &[]);
    let mut rels = String::from(
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
    );
    let mut parts = Vec::new();
    for (i, (name, state, rows, chart)) in sheets.iter().enumerate() {
        let id = format!("rId{}", i + 1);
        let mut data = state.to_le_bytes().to_vec();
        data.extend((i as u32 + 1).to_le_bytes());
        data.extend(wide(&id));
        data.extend(wide(name));
        record(&mut book, 0x009C, &data);
        let folder = if *chart { "chartsheets" } else { "worksheets" };
        rels.push_str(&format!(
            r#"<Relationship Id="{id}" Type="{REL}/worksheet" Target="{folder}/sheet{}.bin"/>"#,
            i + 1
        ));
        let mut sheet = Vec::new();
        record(&mut sheet, 0x0081, &[]);
        let width = rows.iter().map(Vec::len).max().unwrap_or(1) as u32;
        let dim: Vec<u8> = [0u32, rows.len() as u32 - 1, 0, width - 1]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        record(&mut sheet, 0x0094, &dim);
        record(&mut sheet, 0x0091, &[]);
        for (r, row) in rows.iter().enumerate() {
            let mut header = (r as u32).to_le_bytes().to_vec();
            header.extend([0u8; 13]);
            record(&mut sheet, 0x0000, &header);
            for (c, value) in row.iter().enumerate() {
                let col = if matches!(value, B::Far(_)) {
                    u32::MAX
                } else {
                    c as u32
                };
                let mut cell = col.to_le_bytes().to_vec();
                cell.extend([0u8; 4]);
                let typ = match value {
                    B::Num(v) | B::Far(v) => {
                        cell.extend(v.to_le_bytes());
                        0x0005
                    }
                    B::Str(s) => {
                        cell.extend(wide(s));
                        0x0006
                    }
                    B::Bool(b) => {
                        cell.push(u8::from(*b));
                        0x0004
                    }
                    B::Err(e) => {
                        cell.push(*e);
                        0x0003
                    }
                };
                record(&mut sheet, typ, &cell);
            }
        }
        record(&mut sheet, 0x0092, &[]);
        record(&mut sheet, 0x0082, &[]);
        parts.push((format!("xl/{folder}/sheet{}.bin", i + 1), sheet));
    }
    record(&mut book, 0x0090, &[]);
    record(&mut book, 0x0084, &[]);
    rels.push_str("</Relationships>");
    let mut entries: Vec<(&str, Vec<u8>)> = vec![
        ("[Content_Types].xml", br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"/>"#.to_vec()),
        ("_rels/.rels", format!(r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{REL}/officeDocument" Target="xl/workbook.bin"/></Relationships>"#).into_bytes()),
        ("xl/workbook.bin", book),
        ("xl/_rels/workbook.bin.rels", rels.into_bytes()),
    ];
    for (p, d) in &parts {
        entries.push((p.as_str(), d.clone()));
    }
    zip_bytes(&entries, false)
}

fn c(r: &str, attrs: &str, body: &str) -> String {
    format!(r#"<c r="{r}" {attrs}>{body}</c>"#)
}
fn row(n: u32, cells: &[String]) -> String {
    format!(r#"<row r="{n}">{}</row>"#, cells.concat())
}
fn s(r: &str, v: &str) -> String {
    c(r, r#"t="inlineStr""#, &format!("<is><t>{v}</t></is>"))
}
fn n(r: &str, v: &str) -> String {
    c(r, r#"t="n""#, &format!("<v>{v}</v>"))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn excel_serials_follow_both_epochs_and_report_the_1900_leap_day() {
    for (serial, expected) in [
        (1.0, "1900-01-01"),
        (59.0, "1900-02-28"),
        (61.0, "1900-03-01"),
        (61.25, "1900-03-01 06:00:00.000"),
        (45351.0, "2024-02-29"),
        (2958465.0, "9999-12-31"),
        (0.5, "12:00:00.000"),
        (0.99999999, "23:59:59.999"),
        (0.9999999999, "1900-01-01"),
    ] {
        assert_eq!(datetime_for_test(serial, false), expected, "{serial}");
    }
    assert!(datetime_for_test(60.0, false).contains("1900-02-29"));
    assert!(datetime_for_test(-1.0, false).contains("$invalid_date"));
    assert!(datetime_for_test(2958466.0, false).contains("$invalid_date"));
    assert_eq!(datetime_for_test(1.0, true), "1904-01-02");
    assert_eq!(datetime_for_test(60.0, true), "1904-03-01");
    assert_eq!(datetime_for_test(43830.0, true), "2024-01-01");
    assert_eq!(datetime_for_test(0.0, true), "00:00:00.000");
    assert_eq!(column_letters(0), "A");
    assert_eq!(column_letters(25), "Z");
    assert_eq!(column_letters(26), "AA");
    assert_eq!(column_letters(16_383), "XFD");
}

#[tokio::test]
async fn committed_fixtures_and_xlsb_open_with_exact_types_and_complete_workflows() {
    let dir = Directory::new();
    let state = state(&dir);
    let rows = vec![
        vec![
            B::Str("id"),
            B::Str("name"),
            B::Str("ratio"),
            B::Str("ok"),
            B::Str("check"),
        ],
        vec![
            B::Num(1.0),
            B::Str("Alice"),
            B::Num(0.1),
            B::Bool(true),
            B::Err(0x07),
        ],
        vec![
            B::Num(2.0),
            B::Str("Bob"),
            B::Num(0.30000000000000004),
            B::Bool(false),
            B::Str("ok"),
        ],
        vec![
            B::Num(3.0),
            B::Str("Émile ✓"),
            B::Num(-7.25),
            B::Bool(true),
            B::Num(42.0),
        ],
    ];
    std::fs::write(
        dir.0.join("book.xlsb"),
        xlsb(&[
            ("Data", 0, rows, false),
            ("Hidden", 1, vec![vec![B::Num(1.0)]], false),
            ("Chart", 0, vec![vec![B::Num(1.0)]], true),
        ]),
    )
    .unwrap();
    for ext in ["xlsx", "xls", "xlsm", "xlsb"] {
        let path = if ext == "xlsb" {
            dir.0.join("book.xlsb")
        } else {
            copy_fixture(&dir, ext)
        };
        let original = std::fs::read(&path).unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&path, permissions).unwrap();
        let book = inspect(&state, &path).await.unwrap();
        assert_eq!(book.variant, ext);
        assert!(!book.date1904);
        let hidden = book.sheets.iter().find(|s| s.name == "Hidden").unwrap();
        assert_eq!(hidden.visibility, SheetVisibility::Hidden);
        let info = open_sheet(&state, &path, "Data", Some(1)).await.unwrap();
        assert_eq!(info.format, FormatKind::Excel);
        assert_eq!(info.object.label, "Data");
        let selection = info.excel.as_ref().unwrap();
        assert_eq!(
            (selection.sheet.as_str(), selection.header_row),
            ("Data", Some(1))
        );
        let page_all = page(&state, &info, query(&info)).await;
        if ext == "xlsb" {
            assert_eq!(
                types(&info),
                [
                    ("id", "BIGINT"),
                    ("name", "VARCHAR"),
                    ("ratio", "DOUBLE"),
                    ("ok", "BOOLEAN"),
                    ("check", "JSON_VALUE")
                ]
                .map(|(a, b)| (a.to_string(), b.to_string()))
            );
            assert_eq!(
                values(&page_all)[1],
                ["2", "Bob", "0.30000000000000004", "false", "\"ok\""]
            );
            assert_eq!(values(&page_all)[0][4], r##"{"$excel_error":"#DIV/0!"}"##);
            assert!(
                !book
                    .sheets
                    .iter()
                    .find(|s| s.name == "Chart")
                    .unwrap()
                    .selectable
            );
            let error = open_sheet(&state, &path, "Chart", Some(1))
                .await
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidObject);
        } else {
            assert_eq!(
                book.sheets
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>(),
                ["Data", "Offset", "Hidden", "Empty", "Dupes"]
            );
            // LibreOffice stores TRUE/FALSE as formatted numbers, so `active` is numeric.
            assert_eq!(
                types(&info),
                [
                    ("id", "BIGINT"),
                    ("name", "VARCHAR"),
                    ("amount", "DOUBLE"),
                    ("ratio", "DOUBLE"),
                    ("when", "DATE"),
                    ("stamp", "TIMESTAMP"),
                    ("active", "BIGINT"),
                    ("code", "VARCHAR"),
                    ("twice", "DOUBLE"),
                    ("check", "JSON_VALUE")
                ]
                .map(|(a, b)| (a.to_string(), b.to_string()))
            );
            let v = values(&page_all);
            assert_eq!(
                v[0],
                [
                    "1",
                    "Alice",
                    "12.5",
                    "0.1",
                    "2024-02-29",
                    "2024-02-29 13:45:30",
                    "1",
                    "00123",
                    "25.0",
                    r##"{"$excel_error":"#DIV/0!"}"##
                ]
            );
            // .xlsx XML stores LibreOffice's 15-digit text; .xls keeps the binary double.
            assert_eq!(
                v[1][3],
                if ext == "xls" {
                    "0.30000000000000004"
                } else {
                    "0.3"
                }
            );
            assert_eq!(v[1][5], "1900-03-01 06:00:00");
            assert_eq!(v[2][1], "NULL");
            assert_eq!(v[2][3], "1e-07");
            assert_eq!(v[2][5], "2000-01-01 00:00:00.5");
            assert_eq!(v[3][1], "Émile ✓ <b>");
            // LibreOffice writes serial 2 for 1900-01-01 (it has no 1900-02-29);
            // Excel, and Sivro, read serial 2 as 1900-01-02.
            assert_eq!(v[3][4], "1900-01-02");
            assert!(info
                .object
                .warnings
                .iter()
                .any(|w| w.starts_with("mixed_types")));
        }
        // Filter, sort, count, profile and expand through the normal session API.
        let mut q = query(&info);
        q.filter = predicate(
            "col-0",
            FilterOp::Gte,
            FilterValue::Number { value: "2".into() },
        );
        q.sort = Some(SortSpec {
            column_id: "col-2".into(),
            descending: true,
        });
        let shown = page(&state, &info, q.clone()).await;
        assert_eq!(shown.rows.len(), if ext == "xlsb" { 2 } else { 3 });
        let count = state
            .run(
                request(&state, &info, OperationKind::Count, q.clone()),
                OperationKind::Count,
                |a, g, q, p, _| a.count(g, &q, p),
            )
            .await
            .unwrap();
        assert_eq!(
            count.count,
            CountState::known(if ext == "xlsb" { 2 } else { 3 })
        );
        let profile = state
            .run(
                request(
                    &state,
                    &info,
                    OperationKind::Profile,
                    ProfileQuery {
                        column_id: "col-0".into(),
                        context: query(&info),
                    },
                ),
                OperationKind::Profile,
                |a, g, q, p, _| a.profile(g, &q, p),
            )
            .await
            .unwrap();
        assert_eq!(text(&profile.min), Some("1"));
        let raw = state
            .run(
                request(
                    &state,
                    &info,
                    OperationKind::ReadRow,
                    RowQuery {
                        row_id: "0".into(),
                        column_ids: vec!["col-1".into()],
                        raw_record: true,
                    },
                ),
                OperationKind::ReadRow,
                |a, g, q, p, _| a.read_row(g, &q, p),
            )
            .await
            .unwrap_err();
        assert_eq!(raw.code, ErrorCode::InvalidInput);
        let mut unsortable = query(&info);
        let last = info.object.columns.last().unwrap().id.clone();
        unsortable.sort = Some(SortSpec {
            column_id: last.clone(),
            descending: false,
        });
        assert_eq!(
            state
                .run(
                    request(&state, &info, OperationKind::Page, unsortable),
                    OperationKind::Page,
                    |a, g, q, p, _| a.fetch_page(g, &q, p)
                )
                .await
                .unwrap_err()
                .code,
            ErrorCode::InvalidInput
        );
        // All five encodings; reopen through the app where Sivro reads them.
        for (format, out) in [
            (ExportFormat::Json, "json"),
            (ExportFormat::Jsonl, "jsonl"),
            (ExportFormat::Csv, "csv"),
            (ExportFormat::Tsv, "tsv"),
            (ExportFormat::Parquet, "parquet"),
        ] {
            let dest = dir.0.join(format!("{ext}-out.{out}"));
            let result = export_to(&state, &info, &dest, format, query(&info))
                .await
                .unwrap();
            assert_eq!(result.exported_rows, if ext == "xlsb" { "3" } else { "4" });
            let written = std::fs::read(&dest).unwrap();
            if matches!(format, ExportFormat::Json | ExportFormat::Jsonl) {
                let body = String::from_utf8(written).unwrap();
                assert!(
                    body.contains(r##""check":{"$excel_error":"#DIV/0!"}"##),
                    "{body}"
                );
                assert!(body.contains(r#""check":"ok""#));
                let reopened = open(&state, &dest).await;
                assert_eq!(
                    reopened.object.row_count,
                    CountState::known(if ext == "xlsb" { 3 } else { 4 })
                );
                state.close(reopened.session_id).await.unwrap();
            } else if format == ExportFormat::Parquet {
                let reopened = open(&state, &dest).await;
                let back = page(&state, &reopened, query(&reopened)).await;
                assert_eq!(values(&back)[0][1], values(&page_all)[0][1]);
                assert_eq!(values(&back)[0][0], "1");
                state.close(reopened.session_id).await.unwrap();
            }
        }
        state.close(info.session_id).await.unwrap();
        if ext != "xlsb" {
            let offset = open_sheet(&state, &path, "Offset", Some(3)).await.unwrap();
            assert_eq!(
                types(&offset),
                [("region", "VARCHAR"), ("q1", "BIGINT")]
                    .map(|(a, b)| (a.to_string(), b.to_string()))
            );
            assert!(offset
                .object
                .warnings
                .iter()
                .any(|w| w.contains("1 non-empty cells above header row 3")));
            state.close(offset.session_id).await.unwrap();
            let headerless = open_sheet(&state, &path, "Offset", None).await.unwrap();
            assert_eq!(
                headerless
                    .object
                    .columns
                    .iter()
                    .map(|c| c.name.as_str())
                    .collect::<Vec<_>>(),
                ["A", "B"]
            );
            assert_eq!(headerless.object.row_count, CountState::known(5));
            state.close(headerless.session_id).await.unwrap();
            let dupes = open_sheet(&state, &path, "Dupes", Some(1)).await.unwrap();
            assert_eq!(
                dupes
                    .object
                    .columns
                    .iter()
                    .map(|c| (c.id.as_str(), c.name.as_str()))
                    .collect::<Vec<_>>(),
                [
                    ("col-0", "x"),
                    ("col-1", "x"),
                    ("col-2", ""),
                    ("col-3", "y")
                ]
            );
            let dest = dir.0.join(format!("{ext}-dupes.jsonl"));
            export_to(&state, &dupes, &dest, ExportFormat::Jsonl, query(&dupes))
                .await
                .unwrap();
            assert!(std::fs::read_to_string(&dest)
                .unwrap()
                .starts_with(r#"{"col-0":1,"col-1":2,"col-2":3,"col-3":4}"#));
            state.close(dupes.session_id).await.unwrap();
            let hidden = open_sheet(&state, &path, "Hidden", None).await.unwrap();
            assert!(hidden.object.warnings.iter().any(|w| w.contains("hidden")));
            state.close(hidden.session_id).await.unwrap();
            for (sheet, header, code) in [
                ("Empty", Some(1), ErrorCode::InvalidInput),
                ("Missing", Some(1), ErrorCode::InvalidObject),
                ("Data", Some(0), ErrorCode::InvalidInput),
                ("Offset", Some(9), ErrorCode::InvalidInput),
            ] {
                assert_eq!(
                    open_sheet(&state, &path, sheet, header)
                        .await
                        .unwrap_err()
                        .code,
                    code,
                    "{sheet}"
                );
            }
            let required = state
                .open(path.to_str().unwrap().into(), OpenOptions::default())
                .await
                .unwrap_err();
            assert_eq!(required.code, ErrorCode::InvalidInput);
        }
        assert_eq!(
            std::fs::read(&path).unwrap(),
            original,
            "{ext} source bytes changed"
        );
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        std::fs::set_permissions(&path, permissions).unwrap();
    }
}

#[tokio::test]
async fn crafted_cells_keep_blank_empty_text_booleans_errors_1904_dates_and_uncached_formulas() {
    let dir = Directory::new();
    let state = state(&dir);
    let data = [
        row(
            1,
            &[
                s("A1", "flag"),
                s("B1", "label"),
                s("C1", "calc"),
                s("D1", "when"),
                s("E1", "time"),
                s("F1", "elapsed"),
            ],
        ),
        row(
            2,
            &[
                c("A2", r#"t="b""#, "<v>1</v>"),
                s("B2", ""),
                c("C2", "", "<f>A2*2</f>"),
                c("D2", r#"s="1""#, "<v>1</v>"),
                c("E2", r#"s="3""#, "<v>0.5</v>"),
                c("F2", r#"s="4""#, "<v>1.5</v>"),
            ],
        ),
        row(
            3,
            &[
                c("A3", r#"t="b""#, "<v>0</v>"),
                c("C3", "", "<f>A3*2</f><v>7</v>"),
                c("D3", r#"s="2""#, "<v>43830.75</v>"),
                c("E3", r#"s="3""#, "<v>0.25</v>"),
                c("F3", r#"t="e""#, "<v>#N/A</v>"),
            ],
        ),
    ]
    .concat();
    let bytes = xlsx(
        &[
            ("Cells", "visible", data, false),
            ("Plot", "visible", String::new(), true),
            ("Secret", "veryHidden", row(1, &[n("A1", "1")]), false),
        ],
        true,
        vec![("xl/vbaProject.bin", b"not executed".to_vec())],
    );
    let path = dir.0.join("crafted.xlsm");
    std::fs::write(&path, &bytes).unwrap();
    let book = inspect(&state, &path).await.unwrap();
    assert!(book.date1904);
    assert_eq!(book.sheets[1].kind, SheetKind::Chartsheet);
    assert!(!book.sheets[1].selectable);
    assert_eq!(book.sheets[2].visibility, SheetVisibility::VeryHidden);
    let info = open_sheet(&state, &path, "Cells", Some(1)).await.unwrap();
    assert_eq!(
        types(&info),
        [
            ("flag", "BOOLEAN"),
            ("label", "VARCHAR"),
            ("calc", "JSON_VALUE"),
            ("when", "TIMESTAMP"),
            ("time", "TIME"),
            ("elapsed", "JSON_VALUE")
        ]
        .map(|(a, b)| (a.to_string(), b.to_string()))
    );
    let v = values(&page(&state, &info, query(&info)).await);
    assert_eq!(v[0][0], "true");
    // An empty text cell stays empty text; a missing cell is NULL.
    assert_eq!(v[0][1], "");
    assert_eq!(v[1][1], "NULL");
    assert_eq!(
        v[0][2],
        r#"{"$excel_formula":"A2*2","$no_cached_value":true}"#
    );
    assert_eq!(v[1][2], "7");
    assert_eq!(v[0][3], "1904-01-02 00:00:00");
    assert_eq!(v[1][3], "2024-01-01 18:00:00");
    assert_eq!(v[0][4], "12:00:00");
    assert!(v[0][5].contains("$excel_duration_days"));
    assert_eq!(v[1][5], r##"{"$excel_error":"#N/A"}"##);
    let warnings = info.object.warnings.join("\n");
    assert!(warnings.contains("1904 date system"));
    assert!(warnings.contains("no_cached_value: 1 formula cells"));
    assert!(warnings.contains("mixed_types: calc, elapsed"));
    let mut q = query(&info);
    q.filter = predicate(
        "col-1",
        FilterOp::Eq,
        FilterValue::Text { value: "".into() },
    );
    assert_eq!(page(&state, &info, q).await.row_ids, ["0"]);
    let secret = open_sheet(&state, &path, "Secret", None).await.unwrap();
    assert_eq!(secret.object.row_count, CountState::known(1));
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[tokio::test]
async fn oversized_sparse_bomb_encrypted_and_damaged_workbooks_fail_before_allocation() {
    let dir = Directory::new();
    let write = |name: &str, bytes: &[u8]| {
        let path = dir.0.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    };
    let state = state(&dir);
    // A1 plus the last cell of the sheet: rejected while streaming, never built.
    let sparse = xlsx(
        &[(
            "S",
            "visible",
            [
                row(1, &[n("A1", "1")]),
                row(1_048_576, &[n("XFD1048576", "2")]),
            ]
            .concat(),
            false,
        )],
        false,
        vec![],
    );
    let error = open_sheet(&state, &write("sparse.xlsx", &sparse), "S", None)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ResourceLimit);
    assert!(error.message.contains("XFD1048576"), "{}", error.message);
    // About 1,000:1 deflate ratio on an 8 MiB part.
    let zeros = format!(
        "<worksheet xmlns=\"{NS}\"><sheetData/></worksheet>{}",
        " ".repeat(8 << 20)
    );
    let bomb = zip_bytes(&[("xl/worksheets/sheet1.xml", zeros.into_bytes())], true);
    let error = inspect(&state, &write("bomb.xlsx", &bomb))
        .await
        .unwrap_err();
    assert!(
        error.message.contains("compression ratio"),
        "{}",
        error.message
    );
    for (name, bytes, code) in [
        (
            "truncated.xlsx",
            sparse[..sparse.len() / 2].to_vec(),
            ErrorCode::FormatMismatch,
        ),
        (
            "encrypted.xlsx",
            std::fs::read(fixture_path("xls")).unwrap(),
            ErrorCode::FormatMismatch,
        ),
        (
            "renamed.xls",
            std::fs::read(fixture_path("xlsx")).unwrap(),
            ErrorCode::FormatMismatch,
        ),
        (
            "text.xlsb",
            b"not a workbook".to_vec(),
            ErrorCode::FormatMismatch,
        ),
    ] {
        assert_eq!(
            inspect(&state, &write(name, &bytes))
                .await
                .unwrap_err()
                .code,
            code,
            "{name}"
        );
    }

    // Reduced budgets exercise each limit with small synthetic files.
    let small =
        |policy: ResourcePolicy| AppState::new(dir.0.join("scratch-small"), policy).unwrap();
    let wide: String = (1..=12)
        .map(|r| {
            row(
                r,
                &(0..12)
                    .map(|c| n(&format!("{}{r}", column_letters(c)), "1"))
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    let grid = write(
        "grid.xlsx",
        &xlsx(&[("G", "visible", wide, false)], false, vec![]),
    );
    let limited = small(ResourcePolicy {
        excel_cells: 100,
        ..ResourcePolicy::default()
    });
    assert_eq!(
        open_sheet(&limited, &grid, "G", Some(1))
            .await
            .unwrap_err()
            .code,
        ErrorCode::ResourceLimit
    );
    // Every .xls sheet is parsed at open, so its cells count against one budget.
    let error = inspect(
        &small(ResourcePolicy {
            excel_cells: 20,
            ..ResourcePolicy::default()
        }),
        &copy_fixture(&dir, "xls"),
    )
    .await
    .unwrap_err();
    assert!(
        error.message.contains("all") || error.message.contains("whole workbook"),
        "{}",
        error.message
    );
    let big_part = write(
        "part.xlsx",
        &zip_bytes(&[("a.xml", vec![b' '; 2 << 20])], false),
    );
    let error = inspect(
        &small(ResourcePolicy {
            excel_entry_bytes: 1 << 20,
            excel_payload_bytes: 1 << 20,
            ..ResourcePolicy::default()
        }),
        &big_part,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::ResourceLimit);
    let error = inspect(
        &small(ResourcePolicy {
            xls_workbook_bytes: 4096,
            ..ResourcePolicy::default()
        }),
        &copy_fixture(&dir, "xls"),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::ResourceLimit);
    let error = inspect(
        &small(ResourcePolicy {
            excel_workbook_bytes: 1024,
            ..ResourcePolicy::default()
        }),
        &copy_fixture(&dir, "xlsx"),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::ResourceLimit);

    // .xls attacks found in Calamine's open path: a lying DIMENSIONS record
    // (it reserves rows x columns before reading cells) and a sheet offset
    // outside the stream (it would slice out of bounds).
    let xls = std::fs::read(fixture_path("xls")).unwrap();
    let find = |pattern: &[u8]| {
        xls.windows(pattern.len())
            .position(|w| w == pattern)
            .unwrap()
    };
    let mut dims = xls.clone();
    let at = find(&[0x00, 0x02, 0x0E, 0x00]) + 4;
    dims[at + 4..at + 8].copy_from_slice(&u32::MAX.to_le_bytes());
    let error = inspect(&state, &write("dims.xls", &dims))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ResourceLimit, "{}", error.message);
    let mut offset = xls.clone();
    // The BoundSheet8 record for "Data": type, length, offset, state, type, name.
    let at = (0..xls.len() - 16)
        .find(|&i| xls[i..i + 2] == [0x85, 0x00] && &xls[i + 10..i + 16] == b"\x04\x00Data")
        .unwrap()
        + 4;
    offset[at..at + 4].copy_from_slice(&0x7FFF_FFFFu32.to_le_bytes());
    let error = inspect(&state, &write("offset.xls", &offset))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::FormatMismatch, "{}", error.message);
    let error = inspect(&state, &write("cut.xls", &xls[..1536]))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::FormatMismatch, "{}", error.message);
}

#[tokio::test]
async fn sheet_selection_is_frozen_across_refresh_and_large_sheets_stay_bounded() {
    let dir = Directory::new();
    let state = state(&dir);
    let path = copy_fixture(&dir, "xlsx");
    let info = open_sheet(&state, &path, "Offset", Some(3)).await.unwrap();
    std::fs::copy(fixture_path("xlsm"), &path).unwrap();
    let stale = state
        .run(
            request(&state, &info, OperationKind::Page, query(&info)),
            OperationKind::Page,
            |a, g, q, p, _| a.fetch_page(g, &q, p),
        )
        .await
        .unwrap_err();
    assert_eq!(stale.code, ErrorCode::StaleDataset);
    let fresh = state.refresh(info.session_id.clone()).await.unwrap();
    assert_eq!(fresh.object.label, "Offset");
    assert_eq!(fresh.excel.as_ref().unwrap().header_row, Some(3));
    assert_eq!(fresh.object.columns[0].name, "region");
    // A refresh that no longer finds the sheet fails without a usable catalog.
    std::fs::write(
        &path,
        xlsx(
            &[("Other", "visible", row(1, &[n("A1", "1")]), false)],
            false,
            vec![],
        ),
    )
    .unwrap();
    assert_eq!(
        state
            .refresh(info.session_id.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidObject
    );
    state.close(info.session_id).await.unwrap();

    // 20,000 x 10 = 200,000 cells, the default per-sheet budget.
    let mut data = String::with_capacity(16 << 20);
    for r in 1..=20_000u32 {
        let cells: Vec<String> = (0..10)
            .map(|c| {
                n(
                    &format!("{}{r}", column_letters(c)),
                    &format!("{}.5", r * 10 + c),
                )
            })
            .collect();
        data.push_str(&row(r, &cells));
    }
    let large = dir.0.join("large.xlsx");
    std::fs::write(
        &large,
        xlsx(&[("Big", "visible", data, false)], false, vec![]),
    )
    .unwrap();
    let rss = || {
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| {
                s.lines().find(|l| l.starts_with("VmHWM:")).map(|l| {
                    l.split_whitespace()
                        .nth(1)
                        .unwrap_or("0")
                        .parse::<u64>()
                        .unwrap_or(0)
                })
            })
    };
    let before = rss();
    let started = std::time::Instant::now();
    let info = open_sheet(&state, &large, "Big", None).await.unwrap();
    let open_ms = started.elapsed().as_millis();
    assert_eq!(info.object.row_count, CountState::known(20_000));
    assert_eq!(info.object.columns.len(), 10);
    let mut q = query(&info);
    q.sort = Some(SortSpec {
        column_id: "col-9".into(),
        descending: true,
    });
    let top = page(&state, &info, q).await;
    assert_eq!(text(&top.rows[0][9]), Some("200009.5"));
    eprintln!(
        "200,000-cell xlsx ({} MiB): open {open_ms} ms; peak RSS before {:?} KiB, after {:?} KiB (VmHWM is process-wide)",
        std::fs::metadata(&large).unwrap().len() >> 20,
        before,
        rss()
    );
    let over = dir.0.join("over.xlsx");
    // Only two cells, but K1 and A20001 span 20,001 x 11 > 200,000.
    let corners = row(1, &[n("K1", "1")]) + &row(20_001, &[n("A20001", "1")]);
    std::fs::write(
        &over,
        xlsx(&[("Big", "visible", corners, false)], false, vec![]),
    )
    .unwrap();
    let error = open_sheet(&state, &over, "Big", None).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::ResourceLimit, "{}", error.message);
    state.close(info.session_id).await.unwrap();
}

#[tokio::test]
async fn review_regressions_far_columns_cyclic_cfb_and_decoy_streams_fail_cleanly() {
    let dir = Directory::new();
    let state = state(&dir);
    let write = |name: &str, bytes: &[u8]| {
        let path = dir.0.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    };
    // Two cells in columns 0 and 0xFFFFFFFF used to wrap the range width to 0.
    let far = write(
        "far.xlsb",
        &xlsb(&[("S", 0, vec![vec![B::Num(1.0), B::Far(2.0)]], false)]),
    );
    let error = open_sheet(&state, &far, "S", None).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::FormatMismatch, "{}", error.message);
    assert!(
        error.message.contains("outside the Excel grid"),
        "{}",
        error.message
    );

    let xls = std::fs::read(fixture_path("xls")).unwrap();
    let u32_at = |b: &[u8], at: usize| u32::from_le_bytes(b[at..at + 4].try_into().unwrap());
    // A directory chain pointing at itself: Calamine would follow it forever.
    let mut cyclic = xls.clone();
    let (fat_sector, dir_start) = (u32_at(&xls, 0x4C) as usize, u32_at(&xls, 0x30) as usize);
    let entry = (fat_sector + 1) * 512 + dir_start * 4;
    cyclic[entry..entry + 4].copy_from_slice(&(dir_start as u32).to_le_bytes());
    let error = inspect(&state, &write("cyclic.xls", &cyclic))
        .await
        .unwrap_err();
    assert!(error.message.contains("cyclic"), "{}", error.message);

    // Directory names: Calamine prefers an exact "Workbook" over "Book".
    let name = |text: &str| -> Vec<u8> { text.encode_utf16().flat_map(u16::to_le_bytes).collect() };
    let find =
        |b: &[u8], pattern: &[u8]| b.windows(pattern.len()).position(|w| w == pattern).unwrap();
    let rename = |b: &mut Vec<u8>, from: &str, to: &str| {
        let at = find(b, &name(from));
        let mut field = name(to);
        field.resize(64, 0);
        b[at..at + 64].copy_from_slice(&field);
        b[at + 64..at + 66].copy_from_slice(&((to.len() as u16 + 1) * 2).to_le_bytes());
    };
    // A harmless decoy "Book" must not hide a hostile "Workbook".
    let mut decoy = xls.clone();
    rename(&mut decoy, "\u{5}SummaryInformation", "Book");
    let at = find(&decoy, &[0x00, 0x02, 0x0E, 0x00]) + 4;
    decoy[at + 4..at + 8].copy_from_slice(&u32::MAX.to_le_bytes());
    let error = inspect(&state, &write("decoy.xls", &decoy))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ResourceLimit, "{}", error.message);
    // With the names swapped, both readers pick the non-BIFF stream and fail.
    let mut swapped = xls.clone();
    rename(&mut swapped, "Workbook", "Book");
    rename(&mut swapped, "\u{5}SummaryInformation", "Workbook");
    assert!(inspect(&state, &write("swapped.xls", &swapped))
        .await
        .is_err());
    // Calamine ignores the header's mini-stream cutoff; so does the preflight.
    let mut cutoff = xls.clone();
    cutoff[0x38..0x3C].copy_from_slice(&0u32.to_le_bytes());
    assert_eq!(
        inspect(&state, &write("cutoff.xls", &cutoff))
            .await
            .unwrap()
            .sheets
            .len(),
        5
    );
}
