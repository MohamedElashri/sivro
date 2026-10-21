//! JSON document and JSON/JSONL record-table coverage.
use super::*;
use crate::adapters::json::{compact_for_test, parse_for_test, Node};

const BIG: &str = "9007199254740993";
const HUGE: &str = "123456789012345678901234567890";
const WIDE: &str = "12345678901234567890123456789012345678901234567890";
const PRECISE: &str = "0.123456789012345678901234567890";

fn json_state(dir: &Directory, policy: ResourcePolicy) -> Arc<AppState> {
    AppState::new(dir.0.join("scratch"), policy).unwrap()
}
async fn try_open(
    state: &Arc<AppState>,
    path: &Path,
    format: Option<FormatKind>,
) -> Result<DatasetDescriptor, AppError> {
    state
        .open(
            path.to_str().unwrap().into(),
            OpenOptions {
                format,
                ..Default::default()
            },
        )
        .await
}
async fn document(
    state: &Arc<AppState>,
    info: &DatasetDescriptor,
    path: Vec<u64>,
    offset: u64,
    limit: usize,
    mode: DocumentMode,
) -> Result<DocumentResult, AppError> {
    state
        .run_adapter(
            request(
                state,
                info,
                OperationKind::Document,
                DocumentQuery {
                    path,
                    offset,
                    limit,
                    mode,
                },
            ),
            OperationKind::Document,
            |a, g, q, p, _| a.document()?.read(g, &q, p),
        )
        .await
}
async fn export_view(
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
async fn try_page(
    state: &Arc<AppState>,
    info: &DatasetDescriptor,
    q: Query,
) -> Result<PageResult, AppError> {
    state
        .run(
            request(state, info, OperationKind::Page, q),
            OperationKind::Page,
            |a, g, q, p, _| a.fetch_page(g, &q, p),
        )
        .await
}
fn column<'a>(info: &'a DatasetDescriptor, name: &str) -> &'a ColumnDescriptor {
    info.object
        .columns
        .iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("missing column {name}"))
}
fn parse_exact(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap()
}

fn records() -> Vec<String> {
    vec![
        format!(r#"{{"id":1,"name":"Alice","big":{BIG},"huge":{HUGE},"precise":{PRECISE},"amount":12.5,"exp":1e400,"wide":{WIDE},"nested":{{"x":[1,{BIG}]}},"mixed":1,"flag":true}}"#),
        format!(r#"{{"id":2,"name":"Bob","big":1,"huge":2,"precise":0.5,"amount":3,"exp":-2.5E-3,"wide":1,"nested":null,"mixed":"a","flag":false}}"#),
        r#"{"id":3,"name":null,"big":2,"huge":3,"precise":0.25,"amount":0.75,"exp":7,"wide":2,"nested":[],"mixed":null,"flag":null,"late":"only here"}"#.into(),
    ]
}

#[test]
fn ordered_nodes_keep_exact_tokens_duplicates_and_depth_bounds() {
    let node = parse_for_test(&format!(
        r#"{{"b":{BIG},"a":{PRECISE},"b":[{WIDE},-0.0,1E+2],"s":"é\n"}}"#
    ))
    .unwrap();
    let Node::Object(pairs) = &node else {
        panic!("object expected")
    };
    let keys: Vec<_> = pairs.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, ["b", "a", "b", "s"]);
    assert_eq!(pairs[0].1, Node::Number(BIG.into()));
    assert_eq!(pairs[1].1, Node::Number(PRECISE.into()));
    assert_eq!(
        compact_for_test(&node),
        format!(r#"{{"b":{BIG},"a":{PRECISE},"b":[{WIDE},-0.0,1e+2],"s":"é\n"}}"#)
    );
    let deep = |n: usize| format!("{}{}", "[".repeat(n), "]".repeat(n));
    assert!(parse_for_test(&deep(64)).is_ok());
    // A number inside 64 arrays is a leaf, not a 65th level.
    let number = format!("{}{BIG}{}", "[".repeat(64), "]".repeat(64));
    assert!(parse_for_test(&number).is_ok());
    let error = parse_for_test(&deep(65)).unwrap_err().to_string();
    assert!(error.contains("depth limit of 64"), "{error}");
    // Only a number-shaped value can be mistaken for serde_json's private marker.
    let marker = parse_for_test(r#"{"$serde_json::private::Number":"text"}"#).unwrap();
    assert!(matches!(marker, Node::Object(_)));
}

#[tokio::test]
async fn json_documents_are_bounded_exact_trees_with_copy_and_paging() {
    let dir = Directory::new();
    let state = state(&dir);
    let source = dir.0.join("document.json");
    let original = format!(
        "\u{feff}{{\"z\":{BIG},\"a\":{{\"precise\":{PRECISE},\"wide\":{WIDE}}},\"z\":\"second\",\"list\":[{}],\"esc\":\"<b>&\\u0000\"}}",
        (0..250).map(|i| i.to_string()).collect::<Vec<_>>().join(",")
    );
    std::fs::write(&source, &original).unwrap();
    let info = open(&state, &source).await;
    assert_eq!(info.format, FormatKind::Json);
    assert_eq!(info.object.kind, ObjectKind::Document);
    assert!(info.object.columns.is_empty());
    assert!(info.object.capabilities.exports.is_empty());
    let doc = info.document.clone().unwrap();
    assert_eq!(doc.root_kind, NodeKind::Object);
    assert_eq!(doc.child_count, "5");
    assert!(doc.reason.contains("root is an object"));

    let root = document(&state, &info, vec![], 0, 100, DocumentMode::Children)
        .await
        .unwrap();
    let keys: Vec<_> = root
        .children
        .iter()
        .map(|c| c.key.clone().unwrap())
        .collect();
    assert_eq!(keys, ["z", "a", "z", "list", "esc"]);
    assert_eq!(root.children[0].kind, NodeKind::Number);
    assert_eq!(root.children[0].value.as_deref(), Some(BIG));
    assert_eq!(root.children[2].value.as_deref(), Some("second"));
    assert_eq!(root.children[4].value.as_deref(), Some("<b>&\0"));
    assert_eq!(root.children[3].child_count.as_deref(), Some("250"));

    let nested = document(&state, &info, vec![1], 0, 100, DocumentMode::Children)
        .await
        .unwrap();
    assert_eq!(nested.children[0].value.as_deref(), Some(PRECISE));
    assert_eq!(nested.children[1].value.as_deref(), Some(WIDE));
    let copy = document(&state, &info, vec![1], 0, 1, DocumentMode::Copy)
        .await
        .unwrap()
        .text
        .unwrap();
    assert_eq!(
        copy,
        format!("{{\n  \"precise\": {PRECISE},\n  \"wide\": {WIDE}\n}}")
    );

    let first = document(&state, &info, vec![3], 0, 100, DocumentMode::Children)
        .await
        .unwrap();
    assert!(first.has_more);
    assert_eq!(first.children.len(), 100);
    let last = document(&state, &info, vec![3], 200, 100, DocumentMode::Children)
        .await
        .unwrap();
    assert!(!last.has_more);
    assert_eq!(last.children.len(), 50);
    assert_eq!(last.children[49].index, 249);
    assert_eq!(last.children[49].value.as_deref(), Some("249"));

    for (path, limit) in [(vec![9], 10), (vec![0, 0], 10), (vec![], 101), (vec![], 0)] {
        let error = document(&state, &info, path, 0, limit, DocumentMode::Children)
            .await
            .unwrap_err();
        assert!(matches!(
            error.code,
            ErrorCode::InvalidInput | ErrorCode::ResourceLimit
        ));
    }
    let table = try_page(&state, &info, query(&info)).await.unwrap_err();
    assert_eq!(table.code, ErrorCode::InvalidOperation);
    assert_eq!(std::fs::read_to_string(&source).unwrap(), original);
    state.close(info.session_id).await.unwrap();

    for (name, text, kind, reason) in [
        (
            "scalar.json",
            format!(" {WIDE} "),
            ObjectKind::Scalar,
            "single value",
        ),
        (
            "string.json",
            "\"text\"".into(),
            ObjectKind::Scalar,
            "single value",
        ),
        (
            "scalars.json",
            "[1, 2, 3]".into(),
            ObjectKind::Document,
            "not objects",
        ),
        (
            "hetero.json",
            "[{\"a\":1}, 2]".into(),
            ObjectKind::Document,
            "not objects",
        ),
        ("empty.json", "[]".into(), ObjectKind::Document, "is empty"),
        (
            "fieldless.json",
            "[{}, {}]".into(),
            ObjectKind::Document,
            "no fields",
        ),
    ] {
        let path = dir.0.join(name);
        std::fs::write(&path, &text).unwrap();
        let info = open(&state, &path).await;
        assert_eq!(info.object.kind, kind, "{name}");
        assert!(
            info.document.as_ref().unwrap().reason.contains(reason),
            "{name}"
        );
        let root = document(&state, &info, vec![], 0, 100, DocumentMode::Children)
            .await
            .unwrap();
        if kind == ObjectKind::Scalar {
            assert!(root.children.is_empty());
            assert_eq!(
                root.node.value.as_deref(),
                Some(text.trim().trim_matches('"'))
            );
        }
        state.close(info.session_id).await.unwrap();
    }
}

#[tokio::test]
async fn json_documents_reject_malformed_deep_oversized_and_ambiguous_inputs() {
    let dir = Directory::new();
    let policy = ResourcePolicy {
        json_document_bytes: 4096,
        cell_preview_bytes: 1024,
        full_cell_bytes: 2048,
        ..ResourcePolicy::default()
    };
    let state = json_state(&dir, policy);
    let write = |name: &str, text: &str| {
        let path = dir.0.join(name);
        std::fs::write(&path, text).unwrap();
        path
    };
    let malformed = try_open(&state, &write("bad.json", "{\n  \"a\": [1, 2,\n}"), None)
        .await
        .unwrap_err();
    assert_eq!(malformed.code, ErrorCode::InvalidInput);
    assert!(
        malformed.message.contains("line 3"),
        "{}",
        malformed.message
    );
    let deep = format!("{}1{}", "[".repeat(65), "]".repeat(65));
    let error = try_open(&state, &write("deep.json", &deep), None)
        .await
        .unwrap_err();
    assert!(
        error.message.contains("depth limit of 64"),
        "{}",
        error.message
    );
    let big = format!("{{\"a\":\"{}\"}}", "x".repeat(5000));
    let error = try_open(&state, &write("big.json", &big), None)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ResourceLimit);
    assert!(error.message.contains("at most 0 MiB") || error.message.contains("document viewer"));
    // Root arrays of objects use streamed staging instead of the 4 KiB tree budget.
    let rows = (0..200)
        .map(|i| format!("{{\"i\":{i},\"pad\":\"{}\"}}", "y".repeat(40)))
        .collect::<Vec<_>>()
        .join(",");
    let table = try_open(&state, &write("records.json", &format!("[{rows}]")), None)
        .await
        .unwrap();
    assert_eq!(table.object.kind, ObjectKind::Table);
    assert_eq!(table.object.row_count, CountState::known(200));
    state.close(table.session_id).await.unwrap();

    let copyable = format!(
        "{{\"a\":\"{}\",\"b\":[\"{}\"]}}",
        "p".repeat(1200),
        "q".repeat(1500)
    );
    let info = try_open(&state, &write("copy.json", &copyable), None)
        .await
        .unwrap();
    let root = document(&state, &info, vec![], 0, 10, DocumentMode::Children)
        .await
        .unwrap();
    assert!(root.children[0].truncated);
    assert_eq!(root.children[0].value.as_ref().unwrap().len(), 1024);
    assert_eq!(root.children[0].byte_length, "1200");
    let ok = document(&state, &info, vec![0], 0, 1, DocumentMode::Copy)
        .await
        .unwrap();
    assert_eq!(ok.text.unwrap().len(), 1202);
    let too_big = document(&state, &info, vec![], 0, 1, DocumentMode::Copy)
        .await
        .unwrap_err();
    assert_eq!(too_big.code, ErrorCode::ResourceLimit);
    state.close(info.session_id).await.unwrap();

    // A .json file holding JSON Lines is reported, never silently reinterpreted.
    let lines = write("lines.json", "{\"a\":1}\n{\"a\":2}\n");
    let mismatch = try_open(&state, &lines, None).await.unwrap_err();
    assert_eq!(mismatch.code, ErrorCode::FormatMismatch);
    assert!(mismatch.message.contains("reopen it as JSON Lines"));
    let reopened = try_open(&state, &lines, Some(FormatKind::Jsonl))
        .await
        .unwrap();
    assert_eq!(reopened.format, FormatKind::Jsonl);
    assert_eq!(reopened.object.row_count, CountState::known(2));
    state.close(reopened.session_id).await.unwrap();
    let array_lines = write("arrays.json", "[{\"a\":1}]\n[{\"a\":2}]\n");
    assert_eq!(
        try_open(&state, &array_lines, None).await.unwrap_err().code,
        ErrorCode::FormatMismatch
    );
    let wrong = try_open(&state, &lines, Some(FormatKind::Csv))
        .await
        .unwrap_err();
    assert_eq!(wrong.code, ErrorCode::FormatMismatch);
    for (name, bytes) in [
        ("empty.json", b"".to_vec()),
        ("blank.json", b"  \n\t ".to_vec()),
        ("latin1.json", b"{\"a\":\"\xe9\"}".to_vec()),
    ] {
        let path = dir.0.join(name);
        std::fs::write(&path, bytes).unwrap();
        assert!(try_open(&state, &path, None).await.is_err(), "{name}");
    }
}

#[tokio::test]
async fn json_record_arrays_and_jsonl_complete_workflow_with_exact_exports() {
    let dir = Directory::new();
    let state = state(&dir);
    let lines = records();
    for ext in ["json", "jsonl", "ndjson"] {
        let source = dir.0.join(format!("records.{ext}"));
        let original = if ext == "json" {
            format!("[\n{}\n]\n", lines.join(",\n"))
        } else {
            format!("{}\n\n   \n{}\n{}", lines[0], lines[1], lines[2])
        };
        std::fs::write(&source, &original).unwrap();
        let info = open(&state, &source).await;
        let format = if ext == "json" {
            FormatKind::Json
        } else {
            FormatKind::Jsonl
        };
        assert_eq!(info.format, format);
        assert_eq!(info.object.kind, ObjectKind::Table);
        assert!(info.document.is_none());
        assert_eq!(info.object.row_count, CountState::known(3));
        let types: Vec<_> = info
            .object
            .columns
            .iter()
            .map(|c| (c.name.as_str(), c.data_type.as_str()))
            .collect();
        assert_eq!(
            types,
            [
                ("id", "BIGINT"),
                ("name", "VARCHAR"),
                ("big", "BIGINT"),
                ("huge", "HUGEINT"),
                ("precise", "DECIMAL(31,30)"),
                ("amount", "DECIMAL(4,2)"),
                ("exp", "JSON_NUMBER"),
                ("wide", "JSON_NUMBER"),
                ("nested", "JSON_VALUE"),
                ("mixed", "JSON_VALUE"),
                ("flag", "BOOLEAN"),
                ("late", "VARCHAR"),
            ]
        );
        assert!(!column(&info, "exp").sortable);
        assert!(!column(&info, "nested").sortable);
        assert!(!column(&info, "exp").operators.contains(&FilterOp::Gt));
        assert!(info
            .object
            .warnings
            .iter()
            .any(|w| w.contains("exact source tokens")));

        let all = page(&state, &info, query(&info)).await;
        assert_eq!(all.row_ids, ["0", "1", "2"]);
        let row = &all.rows[0];
        assert_eq!(text(&row[2]), Some(BIG));
        assert_eq!(text(&row[3]), Some(HUGE));
        assert_eq!(text(&row[4]), Some(PRECISE));
        assert_eq!(text(&row[5]), Some("12.50"));
        assert_eq!(text(&row[6]), Some("1e+400"));
        assert_eq!(text(&row[7]), Some(WIDE));
        assert!(
            matches!(&row[6], Cell::Number { logical_type, .. } if logical_type == "JSON_NUMBER")
        );
        assert!(
            matches!(&row[8], Cell::Json { value, .. } if value == &format!(r#"{{"x":[1,{BIG}]}}"#))
        );
        assert_eq!(text(&row[9]), Some("1"));
        assert!(matches!(row[10], Cell::Boolean { value: true }));
        assert_eq!(
            all.rows[1][9],
            Cell::Json {
                value: "\"a\"".into(),
                byte_length: "3".into(),
                truncated: false
            }
        );
        assert_eq!(text(&all.rows[1][8]), Some("null"));
        assert_eq!(all.rows[2][1], Cell::Null);
        assert_eq!(
            all.rows[2][9],
            Cell::Json {
                value: "null".into(),
                byte_length: "4".into(),
                truncated: false
            }
        );
        assert_eq!(text(&all.rows[2][11]), Some("only here"));
        assert_eq!(all.rows[0][11], Cell::Null);

        let mut q = query(&info);
        q.filter = FilterExpr::All {
            terms: vec![
                predicate(
                    "col-0",
                    FilterOp::Gte,
                    FilterValue::Number { value: "1".into() },
                ),
                predicate(
                    "col-6",
                    FilterOp::Ne,
                    FilterValue::Number { value: "7".into() },
                ),
            ],
        };
        q.sort = Some(SortSpec {
            column_id: "col-4".into(),
            descending: false,
        });
        let filtered = page(&state, &info, q.clone()).await;
        assert_eq!(filtered.row_ids, ["0", "1"]);
        let mut exact = query(&info);
        exact.filter = predicate(
            "col-6",
            FilterOp::Eq,
            FilterValue::Number {
                value: "-2.5E-3".into(),
            },
        );
        assert_eq!(page(&state, &info, exact.clone()).await.row_ids, ["1"]);
        exact.filter = predicate(
            "col-6",
            FilterOp::Eq,
            FilterValue::Number {
                value: "-0.0025".into(),
            },
        );
        assert!(page(&state, &info, exact.clone()).await.rows.is_empty());
        exact.filter = predicate(
            "col-6",
            FilterOp::Eq,
            FilterValue::Number { value: "01".into() },
        );
        assert_eq!(
            try_page(&state, &info, exact.clone())
                .await
                .unwrap_err()
                .code,
            ErrorCode::InvalidInput
        );
        exact.filter = predicate(
            "col-6",
            FilterOp::Gt,
            FilterValue::Number { value: "1".into() },
        );
        assert_eq!(
            try_page(&state, &info, exact.clone())
                .await
                .unwrap_err()
                .code,
            ErrorCode::InvalidInput
        );
        exact.filter = predicate(
            "col-8",
            FilterOp::Contains,
            FilterValue::Text { value: BIG.into() },
        );
        assert_eq!(page(&state, &info, exact.clone()).await.row_ids, ["0"]);
        exact.filter = FilterExpr::default();
        exact.quick_search = "bob".into();
        assert_eq!(page(&state, &info, exact.clone()).await.row_ids, ["1"]);
        let mut unsortable = query(&info);
        unsortable.sort = Some(SortSpec {
            column_id: "col-6".into(),
            descending: false,
        });
        assert_eq!(
            try_page(&state, &info, unsortable).await.unwrap_err().code,
            ErrorCode::InvalidInput
        );

        let count = state
            .run(
                request(&state, &info, OperationKind::Count, q.clone()),
                OperationKind::Count,
                |a, g, q, p, _| a.count(g, &q, p),
            )
            .await
            .unwrap();
        assert_eq!(count.count, CountState::known(2));
        let profile = |column: &str| ProfileQuery {
            column_id: column.into(),
            context: query(&info),
        };
        let raw = state
            .run(
                request(&state, &info, OperationKind::Profile, profile("col-6")),
                OperationKind::Profile,
                |a, g, q, p, _| a.profile(g, &q, p),
            )
            .await
            .unwrap();
        assert_eq!((raw.min.clone(), raw.max.clone()), (Cell::Null, Cell::Null));
        assert!(!raw.warnings.is_empty());
        assert_eq!(raw.distinct_count, "3");
        let exact_profile = state
            .run(
                request(&state, &info, OperationKind::Profile, profile("col-4")),
                OperationKind::Profile,
                |a, g, q, p, _| a.profile(g, &q, p),
            )
            .await
            .unwrap();
        assert_eq!(
            text(&exact_profile.max),
            Some("0.500000000000000000000000000000")
        );
        assert_eq!(text(&exact_profile.min), Some(PRECISE));
        assert!(exact_profile.warnings.is_empty());

        let row = state
            .run(
                request(
                    &state,
                    &info,
                    OperationKind::ReadRow,
                    RowQuery {
                        row_id: "0".into(),
                        column_ids: vec!["col-7".into()],
                        raw_record: true,
                    },
                ),
                OperationKind::ReadRow,
                |a, g, q, p, _| a.read_row(g, &q, p),
            )
            .await
            .unwrap();
        assert_eq!(text(&row.cells[0]), Some(WIDE));
        // JSONL keeps each exact source line; array elements are re-serialized
        // with exact values, where serde_json spells exponents as `e+`/`e-`.
        let expected = if ext == "json" {
            lines[0].replace("1e400", "1e+400")
        } else {
            lines[0].clone()
        };
        assert_eq!(row.raw_record.as_deref(), Some(expected.as_str()));

        // Exact JSON/JSONL output, then reopen each export through the app.
        let mut view = query(&info);
        view.column_ids = [
            "col-7", "col-4", "col-6", "col-8", "col-9", "col-1", "col-10",
        ]
        .map(String::from)
        .to_vec();
        view.sort = Some(SortSpec {
            column_id: "col-0".into(),
            descending: true,
        });
        for (format, out) in [(ExportFormat::Json, "json"), (ExportFormat::Jsonl, "jsonl")] {
            let dest = dir.0.join(format!("{ext}-export.{out}"));
            let result = export_view(&state, &info, &dest, format, view.clone())
                .await
                .unwrap();
            assert_eq!(result.exported_rows, "3");
            let written = std::fs::read_to_string(&dest).unwrap();
            assert!(written.contains(&format!("\"wide\":{WIDE}")), "{written}");
            assert!(written.contains(&format!("\"precise\":{PRECISE}")));
            assert!(written.contains("\"exp\":1e+400"));
            assert!(written.contains(&format!("\"nested\":{{\"x\":[1,{BIG}]}}")));
            assert!(written.contains("\"mixed\":\"a\"") && written.contains("\"mixed\":null"));
            let values: Vec<serde_json::Value> = if format == ExportFormat::Json {
                assert!(written.starts_with('['));
                serde_json::from_str(&written).unwrap()
            } else {
                assert!(!written.starts_with('['));
                written.lines().map(parse_exact).collect()
            };
            assert_eq!(values.len(), 3);
            assert_eq!(values[2]["wide"].to_string(), WIDE);
            let reopened = open(&state, &dest).await;
            assert_eq!(reopened.object.kind, ObjectKind::Table);
            let names: Vec<_> = reopened
                .object
                .columns
                .iter()
                .map(|c| c.name.as_str())
                .collect();
            assert_eq!(
                names,
                ["wide", "precise", "exp", "nested", "mixed", "name", "flag"]
            );
            let back = page(&state, &reopened, query(&reopened)).await;
            assert_eq!(text(&back.rows[2][0]), Some(WIDE));
            assert_eq!(text(&back.rows[2][1]), Some(PRECISE));
            assert_eq!(text(&back.rows[2][2]), Some("1e+400"));
            state.close(reopened.session_id).await.unwrap();
        }
        for (format, out) in [(ExportFormat::Csv, "csv"), (ExportFormat::Tsv, "tsv")] {
            let dest = dir.0.join(format!("{ext}-export.{out}"));
            let result = export_view(&state, &info, &dest, format, view.clone())
                .await
                .unwrap();
            assert!(result
                .warnings
                .iter()
                .any(|w| w.contains("JSON text strings")));
            let written = std::fs::read_to_string(&dest).unwrap();
            assert!(
                written.contains(WIDE) && written.contains("1e+400") && written.contains(PRECISE)
            );
        }
        let parquet = dir.0.join(format!("{ext}-export.parquet"));
        let refused = export_view(&state, &info, &parquet, ExportFormat::Parquet, view.clone())
            .await
            .unwrap_err();
        assert_eq!(refused.code, ErrorCode::Export);
        assert!(refused.message.contains("wide"));
        assert!(!parquet.exists());
        view.column_ids = ["col-3", "col-4", "col-5", "col-8", "col-10"]
            .map(String::from)
            .to_vec();
        export_view(&state, &info, &parquet, ExportFormat::Parquet, view.clone())
            .await
            .unwrap();
        let reopened = open(&state, &parquet).await;
        let back = page(&state, &reopened, query(&reopened)).await;
        assert_eq!(text(&back.rows[2][0]), Some(HUGE));
        assert_eq!(text(&back.rows[2][1]), Some(PRECISE));
        assert_eq!(text(&back.rows[2][2]), Some("12.50"));
        assert_eq!(reopened.object.columns[1].data_type, "DECIMAL(31,30)");
        state.close(reopened.session_id).await.unwrap();
        assert_eq!(std::fs::read_to_string(&source).unwrap(), original);
        state.close(info.session_id).await.unwrap();
    }
}

#[tokio::test]
async fn jsonl_lines_scalars_errors_duplicates_and_late_keys() {
    let dir = Directory::new();
    let policy = ResourcePolicy {
        jsonl_record_bytes: 1024,
        ..ResourcePolicy::default()
    };
    let state = json_state(&dir, policy);
    let write = |name: &str, text: &str| {
        let path = dir.0.join(name);
        std::fs::write(&path, text).unwrap();
        path
    };
    // Blank and whitespace-only lines are skipped, CRLF and a BOM are accepted.
    let scalars = write(
        "scalars.jsonl",
        &format!("\u{feff}1\r\n\r\n  \t\r\n{BIG}\r\n-3\r\nnull\r\n"),
    );
    let info = try_open(&state, &scalars, None).await.unwrap();
    assert_eq!(info.object.columns.len(), 1);
    assert_eq!(info.object.columns[0].name, "value");
    assert_eq!(info.object.columns[0].data_type, "BIGINT");
    let values = page(&state, &info, query(&info)).await;
    assert_eq!(
        values
            .rows
            .iter()
            .map(|r| text(&r[0]).map(String::from))
            .collect::<Vec<_>>(),
        [Some("1".into()), Some(BIG.into()), Some("-3".into()), None]
    );
    state.close(info.session_id).await.unwrap();
    let arrays = write("arrays.jsonl", "[1,2]\n[]\n\"x\"\n");
    let info = try_open(&state, &arrays, None).await.unwrap();
    assert_eq!(info.object.columns[0].data_type, "JSON_VALUE");
    state.close(info.session_id).await.unwrap();

    for (name, text, code, needle) in [
        (
            "bad-line.jsonl",
            "{\"a\":1}\n\n{\"a\":2}\n{\"a\":\n{\"a\":4}\n",
            ErrorCode::InvalidInput,
            "Line 4",
        ),
        (
            "two-values.jsonl",
            "{\"a\":1}\n{\"a\":2} {\"a\":3}\n",
            ErrorCode::InvalidInput,
            "Line 2 contains more than one",
        ),
        (
            "mixed.jsonl",
            "{\"a\":1}\n\n2\n",
            ErrorCode::InvalidInput,
            "Line 3 is a scalar/array value",
        ),
        (
            "mixed2.jsonl",
            "2\n{\"a\":1}\n",
            ErrorCode::InvalidInput,
            "Line 2 is an object",
        ),
        (
            "document.jsonl",
            "\n{\n  \"a\": 1\n}\n",
            ErrorCode::FormatMismatch,
            "reopen it as JSON",
        ),
        (
            "blank.jsonl",
            "\n \n",
            ErrorCode::InvalidInput,
            "no records",
        ),
        (
            "fieldless.jsonl",
            "{}\n{}\n",
            ErrorCode::InvalidInput,
            "no fields",
        ),
        (
            "nan.jsonl",
            "{\"a\":NaN}\n",
            ErrorCode::InvalidInput,
            "Line 1",
        ),
    ] {
        let error = try_open(&state, &write(name, text), None)
            .await
            .unwrap_err();
        assert_eq!(error.code, code, "{name}: {}", error.message);
        assert!(error.message.contains(needle), "{name}: {}", error.message);
    }
    let long = write(
        "long.jsonl",
        &format!("{{\"a\":1}}\n{{\"a\":\"{}\"}}\n", "z".repeat(2000)),
    );
    let error = try_open(&state, &long, None).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::ResourceLimit);
    assert!(
        error.message.contains("Line 2 exceeds"),
        "{}",
        error.message
    );
    let document = write("document.jsonl", "{\n  \"a\": [1, 2]\n}\n");
    let reopened = try_open(&state, &document, Some(FormatKind::Json))
        .await
        .unwrap();
    assert_eq!(reopened.format, FormatKind::Json);
    assert_eq!(reopened.object.kind, ObjectKind::Document);
    state.close(reopened.session_id).await.unwrap();

    // Repeated keys stay distinct columns; ambiguous labels export by column ID.
    let duplicate = write(
        "duplicate.jsonl",
        "{\"a\":1,\"a\":2,\"b\":null}\n{\"a\":3}\n",
    );
    let info = try_open(&state, &duplicate, None).await.unwrap();
    let names: Vec<_> = info
        .object
        .columns
        .iter()
        .map(|c| (c.id.as_str(), c.name.as_str()))
        .collect();
    assert_eq!(names, [("col-0", "a"), ("col-1", "a"), ("col-2", "b")]);
    assert!(info
        .object
        .warnings
        .iter()
        .any(|w| w.contains("repeat a key")));
    let values = page(&state, &info, query(&info)).await;
    assert_eq!(text(&values.rows[0][1]), Some("2"));
    assert_eq!(values.rows[1][1], Cell::Null);
    let dest = dir.0.join("duplicate-out.jsonl");
    let result = export_view(&state, &info, &dest, ExportFormat::Jsonl, query(&info))
        .await
        .unwrap();
    assert!(result.warnings.iter().any(|w| w.contains("column IDs")));
    assert_eq!(
        std::fs::read_to_string(&dest).unwrap(),
        "{\"col-0\":1,\"col-1\":2,\"col-2\":null}\n{\"col-0\":3,\"col-1\":null,\"col-2\":null}\n"
    );
    state.close(info.session_id).await.unwrap();

    // A key first seen after 20,480 records still becomes a typed column,
    // and JSON null vs a missing key is called out.
    let mut late = String::new();
    for i in 0..30_000 {
        late.push_str(&format!("{{\"i\":{i}}}\n"));
    }
    late.push_str("{\"i\":30000,\"late\":0.5,\"gone\":null}\n");
    let info = try_open(&state, &write("late.jsonl", &late), None)
        .await
        .unwrap();
    assert_eq!(info.object.row_count, CountState::known(30_001));
    assert_eq!(column(&info, "late").data_type, "DECIMAL(2,1)");
    assert!(info
        .object
        .warnings
        .iter()
        .any(|w| w.contains("Missing keys and JSON null")));
    let mut q = query(&info);
    q.filter = predicate("col-1", FilterOp::IsNotNull, FilterValue::None);
    let found = page(&state, &info, q).await;
    assert_eq!(found.row_ids, ["30000"]);
    assert_eq!(text(&found.rows[0][1]), Some("0.5"));
    state.close(info.session_id).await.unwrap();
}

#[tokio::test]
async fn json_record_tables_stream_large_inputs_refresh_and_reject_stale_sources() {
    let dir = Directory::new();
    let state = state(&dir);
    let source = dir.0.join("large.jsonl");
    let mut text = String::with_capacity(16 << 20);
    for i in 0..200_000u64 {
        text.push_str(&format!(
            "{{\"id\":{i},\"group\":\"g{}\",\"amount\":{}.{:02},\"big\":{}}}\n",
            i % 7,
            i % 1000,
            i % 100,
            9_007_199_254_740_993u64 + i
        ));
    }
    std::fs::write(&source, &text).unwrap();
    let started = std::time::Instant::now();
    let info = open(&state, &source).await;
    let open_ms = started.elapsed().as_millis();
    assert_eq!(info.object.row_count, CountState::known(200_000));
    let started = std::time::Instant::now();
    let first = page(&state, &info, query(&info)).await;
    let page_ms = started.elapsed().as_millis();
    assert_eq!(first.rows.len(), 100);
    let mut q = query(&info);
    q.sort = Some(SortSpec {
        column_id: "col-3".into(),
        descending: true,
    });
    q.offset = 150_000;
    let deep = page(&state, &info, q).await;
    assert_eq!(
        text_of(&deep.rows[0][3]),
        (9_007_199_254_740_993u64 + 49_999).to_string()
    );
    eprintln!(
        "200k JSONL ({} MiB): open {open_ms} ms, first page {page_ms} ms",
        text.len() >> 20
    );

    // Any change makes the session stale; refresh re-reads with new values.
    std::fs::write(&source, "{\"id\":1,\"note\":\"changed\"}\n").unwrap();
    let error = try_page(&state, &info, query(&info)).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::StaleDataset);
    let fresh = state.refresh(info.session_id.clone()).await.unwrap();
    assert_eq!(fresh.generation, 2);
    assert_eq!(fresh.object.row_count, CountState::known(1));
    assert_eq!(column(&fresh, "note").data_type, "VARCHAR");
    assert_eq!(page(&state, &fresh, query(&fresh)).await.rows.len(), 1);
    state.close(fresh.session_id).await.unwrap();
    let scratch = std::fs::read_dir(dir.0.join("scratch")).unwrap().count();
    assert_eq!(scratch, 1, "only the live run directory remains");
}

fn text_of(cell: &Cell) -> String {
    text(cell).unwrap().to_string()
}

#[tokio::test]
async fn hugeint_parquet_exports_are_exact_or_fail_instead_of_becoming_double() {
    use crate::adapters::delimited::DelimitedOptions;
    let dir = Directory::new();
    let state = state(&dir);
    let csv = dir.0.join("huge.csv");
    std::fs::write(&csv, format!("n\n{HUGE}\n-{HUGE}\n")).unwrap();
    let jsonl = dir.0.join("max.jsonl");
    let max = "170141183460469231731687303715884105727";
    std::fs::write(&jsonl, format!("{{\"n\":{max}}}\n")).unwrap();
    let info = open_text(&state, &csv, DelimitedOptions::default()).await;
    assert_eq!(info.object.columns[0].data_type, "HUGEINT");
    let dest = dir.0.join("huge.parquet");
    export_view(&state, &info, &dest, ExportFormat::Parquet, query(&info))
        .await
        .unwrap();
    let reopened = open(&state, &dest).await;
    let rows = page(&state, &reopened, query(&reopened)).await.rows;
    assert_eq!(text(&rows[0][0]), Some(HUGE));
    assert_eq!(
        text(&rows[1][0]).map(String::from),
        Some(format!("-{HUGE}"))
    );
    state.close(reopened.session_id).await.unwrap();
    let info = open(&state, &jsonl).await;
    assert_eq!(info.object.columns[0].data_type, "HUGEINT");
    let dest = dir.0.join("max.parquet");
    let error = export_view(&state, &info, &dest, ExportFormat::Parquet, query(&info))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Query, "{}", error.message);
    assert!(!dest.exists());
    let json = dir.0.join("max-out.jsonl");
    export_view(&state, &info, &json, ExportFormat::Jsonl, query(&info))
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&json).unwrap(),
        format!("{{\"n\":{max}}}\n")
    );
}

#[tokio::test]
async fn value_budgets_bound_tree_memory_for_dense_documents_and_records() {
    let dir = Directory::new();
    let policy = ResourcePolicy {
        json_document_bytes: 64 << 10,
        jsonl_record_bytes: 4096,
        ..ResourcePolicy::default()
    };
    let state = json_state(&dir, policy);
    let write = |name: &str, text: String| {
        let path = dir.0.join(name);
        std::fs::write(&path, text).unwrap();
        path
    };
    let values = |n: usize| (0..n).map(|_| "0").collect::<Vec<_>>().join(",");
    // 20,000 values in 40 KB exceed 64 KiB / 32 = 2,048 tree nodes.
    let dense = try_open(
        &state,
        &write("dense.json", format!("[{}]", values(20_000))),
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(dense.code, ErrorCode::ResourceLimit, "{}", dense.message);
    assert!(dense.message.contains("budget"), "{}", dense.message);
    let fine = try_open(
        &state,
        &write("fine.json", format!("[{}]", values(1_000))),
        None,
    )
    .await
    .unwrap();
    assert_eq!(fine.object.kind, ObjectKind::Document);
    state.close(fine.session_id).await.unwrap();
    // 4096 / 32 = 128 nodes per record; each record gets a fresh budget.
    let lines = format!("[{}]\n[{}]\n", values(100), values(100));
    let ok = try_open(&state, &write("ok.jsonl", lines), None)
        .await
        .unwrap();
    assert_eq!(ok.object.row_count, CountState::known(2));
    state.close(ok.session_id).await.unwrap();
    let error = try_open(
        &state,
        &write("wide.jsonl", format!("[{}]\n", values(500))),
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::ResourceLimit, "{}", error.message);
    let elements = format!("[{{\"a\":[{}]}}]", values(500));
    let error = try_open(&state, &write("wide.json", elements), None)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ResourceLimit, "{}", error.message);
}
