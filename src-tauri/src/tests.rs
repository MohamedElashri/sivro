use crate::{
    api::*,
    dataset::*,
    errors::{AppError, ErrorCode},
    export,
    policy::ResourcePolicy,
    query::{duckdb as sql, FilterExpr, FilterValue, Query, SortSpec},
    registry,
    sessions::AppState,
};
use duckdb::Connection;
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

static NEXT: AtomicU64 = AtomicU64::new(1);
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "sivro-test-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn fixture(path: &Path, query: &str) {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("SET autoinstall_known_extensions=false; SET autoload_known_extensions=false; SET memory_limit='512MB'; SET threads=1;").unwrap();
    conn.execute_batch(&format!(
        "COPY ({query}) TO {} (FORMAT PARQUET, ROW_GROUP_SIZE 2048)",
        sql::quote_literal(path.to_str().unwrap())
    ))
    .unwrap();
}
fn state(dir: &Directory) -> Arc<AppState> {
    AppState::new(dir.0.join("scratch"), ResourcePolicy::default()).unwrap()
}
async fn open(state: &Arc<AppState>, path: &Path) -> DatasetDescriptor {
    state
        .open(path.to_str().unwrap().into(), OpenOptions::default())
        .await
        .unwrap()
}
fn query(info: &DatasetDescriptor) -> Query {
    Query {
        column_ids: info.object.columns.iter().map(|c| c.id.clone()).collect(),
        offset: 0,
        page_size: 100,
        filter: FilterExpr::default(),
        quick_search: String::new(),
        sort: None,
    }
}
fn request<T>(
    state: &Arc<AppState>,
    info: &DatasetDescriptor,
    kind: OperationKind,
    query: T,
) -> OperationRequest<T> {
    let handle = state
        .begin(
            info.session_id.clone(),
            info.generation,
            info.object.id.clone(),
            kind,
        )
        .unwrap();
    OperationRequest {
        session_id: info.session_id.clone(),
        generation: info.generation,
        object_id: info.object.id.clone(),
        operation_id: handle.id,
        query,
    }
}
async fn page(state: &Arc<AppState>, info: &DatasetDescriptor, q: Query) -> PageResult {
    state
        .run(
            request(state, info, OperationKind::Page, q),
            OperationKind::Page,
            |a, g, q, p, _| a.fetch_page(g, &q, p),
        )
        .await
        .unwrap()
}
fn text(cell: &Cell) -> Option<&str> {
    match cell {
        Cell::Text { value, .. } | Cell::Number { value, .. } | Cell::Json { value, .. } => {
            Some(value)
        }
        _ => None,
    }
}
fn predicate(id: &str, op: FilterOp, value: FilterValue) -> FilterExpr {
    FilterExpr::Predicate {
        column_id: id.into(),
        op,
        value,
    }
}

#[tokio::test]
async fn sample_session_end_to_end_and_five_export_roundtrips() {
    let dir = Directory::new();
    let state = state(&dir);
    let source = dir.0.join("sample.parquet");
    crate::sample::write_sample(source.to_str().unwrap()).unwrap();
    let bytes = std::fs::read(&source).unwrap();
    let info = open(&state, &source).await;
    assert_eq!(info.api_version, 1);
    assert_eq!(info.object.row_count, CountState::known(1500));
    assert_eq!(info.object.columns.len(), 7);
    assert!(!info.row_groups.is_empty());
    let mut q = query(&info);
    q.column_ids = vec!["col-0".into(), "col-3".into(), "col-2".into()];
    q.filter = predicate(
        "col-2",
        FilterOp::Eq,
        FilterValue::Text {
            value: "Engineering".into(),
        },
    );
    q.quick_search = "Engineering".into();
    q.sort = Some(SortSpec {
        column_id: "col-3".into(),
        descending: true,
    });
    q.page_size = 5;
    let preview = page(&state, &info, q.clone()).await;
    assert_eq!(preview.rows.len(), 5);
    assert_eq!(preview.count, CountState::Unknown);
    assert_eq!(text(&preview.rows[0][2]), Some("Engineering"));
    for pair in preview.rows.windows(2) {
        assert!(
            text(&pair[0][1]).unwrap().parse::<f64>().unwrap()
                >= text(&pair[1][1]).unwrap().parse::<f64>().unwrap()
        );
    }
    let counted = state
        .run(
            request(&state, &info, OperationKind::Count, q.clone()),
            OperationKind::Count,
            |a, g, q, p, _| a.count(g, &q, p),
        )
        .await
        .unwrap();
    assert!(matches!(counted.count, CountState::Known { .. }));
    assert_eq!(page(&state, &info, q.clone()).await.count, counted.count);
    let prof = state
        .run(
            request(
                &state,
                &info,
                OperationKind::Profile,
                ProfileQuery {
                    column_id: "col-2".into(),
                    context: q.clone(),
                },
            ),
            OperationKind::Profile,
            |a, g, q, p, _| a.profile(g, &q, p),
        )
        .await
        .unwrap();
    assert_eq!(prof.distinct_count, "1");
    assert_eq!(prof.scope, "current_view");
    for (format, extension) in [
        (ExportFormat::Parquet, "parquet"),
        (ExportFormat::Csv, "csv"),
        (ExportFormat::Tsv, "tsv"),
        (ExportFormat::Jsonl, "jsonl"),
        (ExportFormat::Json, "json"),
    ] {
        let destination = dir.0.join(format!("out.{extension}"));
        let options = ExportOptions {
            destination: destination.to_str().unwrap().into(),
            format,
            max_rows: 3,
            view: q.clone(),
        };
        let result = state
            .run(
                request(&state, &info, OperationKind::Export, options),
                OperationKind::Export,
                |a, g, q, p, c| a.export(g, q, p, c),
            )
            .await
            .unwrap();
        assert_eq!(result.exported_rows, "3");
        assert!(result.truncated);
        assert!(!result.warnings.is_empty());
        let conn = Connection::open_in_memory().unwrap();
        let reader = match format {
            ExportFormat::Arrow | ExportFormat::ArrowStream => unreachable!(),
            ExportFormat::Parquet => "read_parquet",
            ExportFormat::Csv => "read_csv_auto",
            ExportFormat::Json | ExportFormat::Jsonl => "read_json_auto",
            ExportFormat::Tsv => "read_csv_auto",
        };
        let (count, name): (u64, String) = conn
            .query_row(
                &format!(
                    "SELECT count(*), first(department) FROM {reader}({})",
                    sql::quote_literal(destination.to_str().unwrap())
                ),
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(count, 3);
        assert_eq!(name, "Engineering");
        if format == ExportFormat::Json {
            let rows: Vec<serde_json::Value> =
                serde_json::from_slice(&std::fs::read(&destination).unwrap()).unwrap();
            assert_eq!(rows.len(), 3);
            assert_eq!(
                rows[0]["id"].to_string(),
                text(&preview.rows[0][0]).unwrap()
            );
        }
        if format == ExportFormat::Parquet {
            let exported = open(&state, &destination).await;
            assert_eq!(exported.object.row_count, CountState::known(3));
            state.close(exported.session_id).await.unwrap();
        }
    }
    assert_eq!(std::fs::read(&source).unwrap(), bytes);
    assert!(state.close(info.session_id.clone()).await.unwrap().closed);
    assert!(!state.close(info.session_id).await.unwrap().closed);
    assert_eq!(std::fs::read_dir(&state.scratch.path).unwrap().count(), 1); // lease only
}

#[tokio::test]
async fn exact_tagged_scalars_nested_values_and_copy_row() {
    let dir = Directory::new();
    let state = state(&dir);
    let source = dir.0.join("exact.parquet");
    fixture(&source,"SELECT 9007199254740993::BIGINT AS id, 18446744073709551615::UBIGINT AS u, 123456789012345678.12345678901234567890::DECIMAL(38,20) AS d, {'value':9007199254740993::BIGINT} AS nested, false AS flag, '' AS empty, 0 AS zero, NULL::VARCHAR AS nil, '\\x00\\xFF'::BLOB AS bin");
    let info = open(&state, &source).await;
    let p = page(&state, &info, query(&info)).await;
    assert!(matches!(&p.rows[0][0],Cell::Number{value,..} if value=="9007199254740993"));
    assert_eq!(text(&p.rows[0][1]), Some("18446744073709551615"));
    assert_eq!(
        text(&p.rows[0][2]),
        Some("123456789012345678.12345678901234567890")
    );
    assert!(matches!(&p.rows[0][3],Cell::Json{value,..} if value=="{\"value\":9007199254740993}"));
    assert_eq!(p.rows[0][4], Cell::Boolean { value: false });
    assert_eq!(text(&p.rows[0][5]), Some(""));
    assert_eq!(text(&p.rows[0][6]), Some("0"));
    assert_eq!(p.rows[0][7], Cell::Null);
    assert!(
        matches!(&p.rows[0][8],Cell::Binary{preview_hex,byte_count,..} if preview_hex=="00FF" && byte_count=="2")
    );
    let row = state
        .run(
            request(
                &state,
                &info,
                OperationKind::ReadRow,
                RowQuery {
                    row_id: p.row_ids[0].clone(),
                    column_ids: p.column_ids.clone(),
                    raw_record: false,
                },
            ),
            OperationKind::ReadRow,
            |a, g, q, p, _| a.read_row(g, &q, p),
        )
        .await
        .unwrap();
    assert_eq!(row.cells, p.rows[0]);
    let mut q = query(&info);
    q.filter = predicate(
        "col-2",
        FilterOp::Eq,
        FilterValue::Number {
            value: "123456789012345678.123456789012345678901".into(),
        },
    );
    let err = state
        .run(
            request(&state, &info, OperationKind::Page, q),
            OperationKind::Page,
            |a, g, q, p, _| a.fetch_page(g, &q, p),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidInput);
}

#[tokio::test]
async fn opaque_columns_alias_collisions_sort_ties_and_stable_source_rows() {
    let dir = Directory::new();
    let state = state(&dir);
    let source = dir.0.join("sort.parquet");
    fixture(&source,"SELECT range AS a, (6000-range) AS c0, range%2 AS field_0, range+100 AS __sivro_row FROM range(6000)");
    let info = open(&state, &source).await;
    assert!(info.row_groups.len() > 1);
    let mut unsorted = query(&info);
    unsorted.offset = 2050;
    unsorted.page_size = 3;
    let direct = page(&state, &info, unsorted).await;
    assert_eq!(direct.row_ids[0], "2050");
    assert_eq!(text(&direct.rows[0][0]), Some("2050"));
    for selected in [
        vec!["col-1".into()],
        vec!["col-0".into(), "col-1".into()],
        vec!["col-0".into()],
    ] {
        for descending in [false, true] {
            let mut q = query(&info);
            q.column_ids = selected.clone();
            q.sort = Some(SortSpec {
                column_id: "col-1".into(),
                descending,
            });
            q.page_size = 3;
            let p = page(&state, &info, q).await;
            let expected = if descending { "0" } else { "5999" };
            assert_eq!(p.row_ids[0], expected);
        }
    }
    let mut q = query(&info);
    q.sort = Some(SortSpec {
        column_id: "col-2".into(),
        descending: false,
    });
    q.page_size = 10;
    q.offset = 10;
    for _ in 0..3 {
        let p = page(&state, &info, q.clone()).await;
        assert_eq!(p.row_ids[0], "20");
        assert_eq!(text(&p.rows[0][0]), Some("20"));
    }
    let row = state
        .run(
            request(
                &state,
                &info,
                OperationKind::ReadRow,
                RowQuery {
                    row_id: "5177".into(),
                    column_ids: vec!["col-0".into()],
                    raw_record: false,
                },
            ),
            OperationKind::ReadRow,
            |a, g, q, p, _| a.read_row(g, &q, p),
        )
        .await
        .unwrap();
    assert_eq!(text(&row.cells[0]), Some("5177"));
}

#[tokio::test]
async fn limits_preview_expansion_and_backend_validation() {
    let dir = Directory::new();
    let state = state(&dir);
    let source = dir.0.join("limits.parquet");
    fixture(
        &source,
        "SELECT repeat('é',3000) AS text, repeat('x',1100000) AS large",
    );
    let info = open(&state, &source).await;
    let p = page(&state, &info, query(&info)).await;
    assert!(
        matches!(&p.rows[0][0],Cell::Text{value,truncated:true,byte_length} if value.len()==4096 && byte_length=="6000")
    );
    let read = |col: &str| RowQuery {
        row_id: "0".into(),
        column_ids: vec![col.into()],
        raw_record: false,
    };
    let full = state
        .run(
            request(&state, &info, OperationKind::ReadRow, read("col-0")),
            OperationKind::ReadRow,
            |a, g, q, p, _| a.read_row(g, &q, p),
        )
        .await
        .unwrap();
    assert_eq!(text(&full.cells[0]).unwrap().len(), 6000);
    let err = state
        .run(
            request(&state, &info, OperationKind::ReadRow, read("col-1")),
            OperationKind::ReadRow,
            |a, g, q, p, _| a.read_row(g, &q, p),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::ResourceLimit);
    let mut q = query(&info);
    q.page_size = 1001;
    assert_eq!(
        state
            .run(
                request(&state, &info, OperationKind::Page, q),
                OperationKind::Page,
                |a, g, q, p, _| a.fetch_page(g, &q, p)
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::ResourceLimit
    );
    let mut q = query(&info);
    q.column_ids = vec!["text".into()];
    assert_eq!(
        state
            .run(
                request(&state, &info, OperationKind::Page, q),
                OperationKind::Page,
                |a, g, q, p, _| a.fetch_page(g, &q, p)
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidColumn
    );
    let mut q = query(&info);
    q.column_ids = vec!["col-0".into(), "col-0".into()];
    assert_eq!(
        state
            .run(
                request(&state, &info, OperationKind::Page, q),
                OperationKind::Page,
                |a, g, q, p, _| a.fetch_page(g, &q, p)
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidInput
    );
}

#[tokio::test]
async fn typed_filters_bind_injection_values_and_validate_object_handles() {
    let dir = Directory::new();
    let state = state(&dir);
    let source = dir.0.join("a'; SELECT 1;--.parquet");
    fixture(
        &source,
        "SELECT * FROM (VALUES ('x'' OR true --',1),('other',2)) t(\"a\"\";--\", id)",
    );
    let info = open(&state, &source).await;
    let mut q = query(&info);
    q.filter = predicate(
        "col-0",
        FilterOp::Eq,
        FilterValue::Text {
            value: "x' OR true --".into(),
        },
    );
    assert_eq!(page(&state, &info, q.clone()).await.rows.len(), 1);
    q.filter = predicate(
        "col-0",
        FilterOp::Contains,
        FilterValue::Text { value: "%_".into() },
    );
    assert!(page(&state, &info, q.clone()).await.rows.is_empty());
    let mut req = request(&state, &info, OperationKind::Page, q);
    req.object_id = "read_parquet('/etc/passwd')".into();
    assert_eq!(
        state
            .run(req, OperationKind::Page, |a, g, q, p, _| a
                .fetch_page(g, &q, p))
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidObject
    );
    assert!(serde_json::from_value::<OperationRequest<Query>>(serde_json::json!({"sessionId":info.session_id,"generation":1,"objectId":info.object.id,"operationId":"fake","path":"/etc/passwd","query":query(&info)})).is_err());
    let mut req = request(&state, &info, OperationKind::Page, query(&info));
    req.operation_id = "unknown".into();
    assert_eq!(
        state
            .run(req, OperationKind::Page, |a, g, q, p, _| a
                .fetch_page(g, &q, p))
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidOperation
    );
}

#[tokio::test]
async fn stale_source_requires_refresh_and_rejects_old_generation() {
    let dir = Directory::new();
    let state = state(&dir);
    let source = dir.0.join("changing.parquet");
    fixture(&source, "SELECT 1 AS before");
    let info = open(&state, &source).await;
    let replacement = dir.0.join("new.parquet");
    fixture(&replacement, "SELECT range AS after FROM range(4)");
    std::fs::rename(&replacement, &source).unwrap();
    let error = state
        .run(
            request(&state, &info, OperationKind::Page, query(&info)),
            OperationKind::Page,
            |a, g, q, p, _| a.fetch_page(g, &q, p),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::StaleDataset);
    let fresh = state.refresh(info.session_id.clone()).await.unwrap();
    assert_eq!(fresh.session_id, info.session_id);
    assert!(fresh.generation > info.generation);
    assert_ne!(fresh.object.id, info.object.id);
    assert_eq!(fresh.object.columns[0].name, "after");
    assert_eq!(fresh.object.row_count, CountState::known(4));
    assert_eq!(
        state
            .begin(
                info.session_id.clone(),
                info.generation,
                info.object.id.clone(),
                OperationKind::Page
            )
            .unwrap_err()
            .code,
        ErrorCode::InvalidGeneration
    );
    assert_eq!(page(&state, &fresh, query(&fresh)).await.rows.len(), 4);
    assert_eq!(std::fs::read_dir(&state.scratch.path).unwrap().count(), 2); // current generation + root lease
}

#[tokio::test]
async fn probing_rejects_spoofed_truncated_and_unsupported_inputs() {
    let dir = Directory::new();
    let state = state(&dir);
    for (name, bytes, code) in [
        ("empty.parquet", b"".as_slice(), ErrorCode::FormatMismatch),
        (
            "fake.parquet",
            b"SQLite format 3\0".as_slice(),
            ErrorCode::FormatMismatch,
        ),
        (
            "fake.db",
            b"not sqlite".as_slice(),
            ErrorCode::FormatMismatch,
        ),
        ("file.xyz", b"data".as_slice(), ErrorCode::UnsupportedFormat),
        (
            "bad.parquet",
            b"PAR1xxxxPAR1".as_slice(),
            ErrorCode::FormatMismatch,
        ),
    ] {
        let path = dir.0.join(name);
        std::fs::write(&path, bytes).unwrap();
        assert_eq!(
            state
                .open(path.to_str().unwrap().into(), OpenOptions::default())
                .await
                .unwrap_err()
                .code,
            code
        );
    }
    let path = dir.0.join("alias.PQ");
    fixture(&path, "SELECT 1 AS x");
    assert_eq!(open(&state, &path).await.format, FormatKind::Parquet);
    // All seven input families are implemented; a valid SQLite header probes as supported.
    let sqlite = dir.0.join("valid.sqlite");
    std::fs::write(&sqlite, b"SQLite format 3\0").unwrap();
    assert!(registry::probe(&sqlite).unwrap().supported);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_queued_active_and_late_operations_without_poisoning_connection() {
    let dir = Directory::new();
    let state = state(&dir);
    let source = dir.0.join("cancel.parquet");
    fixture(&source, "SELECT 1 AS x");
    let info = open(&state, &source).await;
    let queued = request(&state, &info, OperationKind::Page, query(&info));
    state.cancel_operation(&queued.operation_id).unwrap();
    assert_eq!(
        state
            .run(queued, OperationKind::Page, |a, g, q, p, _| a
                .fetch_page(g, &q, p))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Cancelled
    );
    let req = request(&state, &info, OperationKind::Page, query(&info));
    let id = req.operation_id.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let worker = state.clone();
    let running = tokio::spawn(async move {
        worker.run(req,OperationKind::Page,move|a,g,q,p,_|{let _=tx.send(());a.duckdb().unwrap().conn.query_row::<String,_,_>("SELECT sum(a.range*b.range)::VARCHAR FROM range(1000000000) a, range(1000000000) b",[],|r|r.get(0)).map_err(AppError::query)?;a.fetch_page(g,&q,p)}).await
    });
    rx.await.unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(
        state.cancel_operation(&id).unwrap().status,
        "cancel_pending"
    );
    let error = tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Cancelled);
    assert_eq!(state.cancel_operation(&id).unwrap().status, "finished");
    assert_eq!(page(&state, &info, query(&info)).await.rows.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn global_admission_is_two_and_each_session_has_one_execution_lane() {
    let dir = Directory::new();
    let state = state(&dir);
    let source = dir.0.join("lanes.parquet");
    fixture(&source, "SELECT 1 AS x");
    let a = open(&state, &source).await;
    let b = open(&state, &source).await;
    let c = open(&state, &source).await;
    let active = Arc::new(AtomicUsize::new(0));
    let maximum = Arc::new(AtomicUsize::new(0));
    let mut jobs = Vec::new();
    for info in [a.clone(), b, c, a.clone(), a.clone()] {
        let req = request(&state, &info, OperationKind::Page, query(&info));
        let s = state.clone();
        let active = active.clone();
        let maximum = maximum.clone();
        jobs.push(tokio::spawn(async move {
            s.run(req, OperationKind::Page, move |a, g, q, p, _| {
                let n = active.fetch_add(1, Ordering::SeqCst) + 1;
                maximum.fetch_max(n, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(25));
                let result = a.fetch_page(g, &q, p);
                active.fetch_sub(1, Ordering::SeqCst);
                result
            })
            .await
        }));
    }
    for job in jobs {
        job.await.unwrap().unwrap();
    }
    assert_eq!(maximum.load(Ordering::SeqCst), 2);
    let active = Arc::new(AtomicUsize::new(0));
    let maximum = Arc::new(AtomicUsize::new(0));
    let mut jobs = Vec::new();
    for _ in 0..3 {
        let req = request(&state, &a, OperationKind::Page, query(&a));
        let s = state.clone();
        let active = active.clone();
        let maximum = maximum.clone();
        jobs.push(tokio::spawn(async move {
            s.run(req, OperationKind::Page, move |a, g, q, p, _| {
                let n = active.fetch_add(1, Ordering::SeqCst) + 1;
                maximum.fetch_max(n, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(10));
                let result = a.fetch_page(g, &q, p);
                active.fetch_sub(1, Ordering::SeqCst);
                result
            })
            .await
        }));
    }
    for job in jobs {
        job.await.unwrap().unwrap();
    }
    assert_eq!(maximum.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn close_interrupts_active_work_and_removes_owned_scratch() {
    let dir = Directory::new();
    let state = state(&dir);
    let source = dir.0.join("close.parquet");
    fixture(&source, "SELECT 1 AS x");
    let info = open(&state, &source).await;
    let req = request(&state, &info, OperationKind::Page, query(&info));
    let (tx, rx) = tokio::sync::oneshot::channel();
    let s = state.clone();
    let job = tokio::spawn(async move {
        s.run(req,OperationKind::Page,move|a,g,q,p,_|{let _=tx.send(());a.duckdb().unwrap().conn.query_row::<String,_,_>("SELECT sum(a.range*b.range)::VARCHAR FROM range(1000000000) a, range(1000000000) b",[],|r|r.get(0)).map_err(AppError::query)?;a.fetch_page(g,&q,p)}).await
    });
    rx.await.unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(
        tokio::time::timeout(Duration::from_secs(5), state.close(info.session_id.clone()))
            .await
            .unwrap()
            .unwrap()
            .closed
    );
    assert!(job.await.unwrap().is_err());
    assert_eq!(
        state
            .metadata(info.session_id, info.generation, info.object.id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidSession
    );
    assert_eq!(std::fs::read_dir(&state.scratch.path).unwrap().count(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_timeout_is_acknowledged_and_connection_can_be_reused() {
    let dir = Directory::new();
    let policy = ResourcePolicy {
        timeout_ms: 40,
        ..ResourcePolicy::default()
    };
    let state = AppState::new(dir.0.join("scratch"), policy).unwrap();
    let source = dir.0.join("timeout.parquet");
    fixture(&source, "SELECT 1 AS x");
    let info = open(&state, &source).await;
    let result=state.run(request(&state,&info,OperationKind::Page,query(&info)),OperationKind::Page,|a,g,q,p,_|{a.duckdb().unwrap().conn.query_row::<String,_,_>("SELECT sum(a.range*b.range)::VARCHAR FROM range(1000000000) a, range(1000000000) b",[],|r|r.get(0)).map_err(AppError::query)?;a.fetch_page(g,&q,p)}).await;
    assert_eq!(result.unwrap_err().code, ErrorCode::Timeout);
    assert_eq!(page(&state, &info, query(&info)).await.rows.len(), 1);
}

#[tokio::test]
async fn export_aliases_collisions_and_nonfinite_json_fail_without_source_writes() {
    let dir = Directory::new();
    let state = state(&dir);
    let source = dir.0.join("source.parquet");
    fixture(&source, "SELECT 1 AS id");
    let info = open(&state, &source).await;
    let original = std::fs::read(&source).unwrap();
    let hard = dir.0.join("hard.parquet");
    std::fs::hard_link(&source, &hard).unwrap();
    let other = dir.0.join("other.parquet");
    std::fs::write(&other, b"keep").unwrap();
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut paths = vec![source.clone(), hard, other.clone()];
    #[cfg(unix)]
    {
        let alias = dir.0.join("link.parquet");
        std::os::unix::fs::symlink(&source, &alias).unwrap();
        paths.push(alias);
    }
    for destination in paths {
        let options = ExportOptions {
            destination: destination.to_str().unwrap().into(),
            format: ExportFormat::Parquet,
            max_rows: 1,
            view: query(&info),
        };
        assert!(state
            .run(
                request(&state, &info, OperationKind::Export, options),
                OperationKind::Export,
                |a, g, q, p, c| a.export(g, q, p, c)
            )
            .await
            .is_err());
    }
    assert_eq!(std::fs::read(&source).unwrap(), original);
    assert_eq!(std::fs::read(&other).unwrap(), b"keep");
    for (name, sql_text, format) in [
        ("marker", "SELECT '\\N' AS x", ExportFormat::Csv),
        ("infinite", "SELECT 'NaN'::DOUBLE AS x", ExportFormat::Json),
    ] {
        let file = dir.0.join(format!("{name}.parquet"));
        fixture(&file, sql_text);
        let info = open(&state, &file).await;
        let dest = dir.0.join(if name == "marker" {
            "out.csv"
        } else {
            "out.json"
        });
        let options = ExportOptions {
            destination: dest.to_str().unwrap().into(),
            format,
            max_rows: 10,
            view: query(&info),
        };
        assert!(state
            .run(
                request(&state, &info, OperationKind::Export, options),
                OperationKind::Export,
                |a, g, q, p, c| a.export(g, q, p, c)
            )
            .await
            .is_err());
        assert!(!dest.exists());
    }
    assert!(!std::fs::read_dir(&dir.0).unwrap().any(|e| e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".sivro-export-")));
}

#[test]
fn resource_policy_only_accepts_bounded_reductions_and_scratch_lease_protects_live_runs() {
    let policy = ResourcePolicy {
        max_page_size: 1001,
        ..ResourcePolicy::default()
    };
    assert!(policy.validate().is_err());
    let policy = ResourcePolicy {
        timeout_ms: 0,
        ..ResourcePolicy::default()
    };
    assert!(policy.validate().is_err());
    let dir = Directory::new();
    let first = state(&dir);
    let first_path = first.scratch.path.clone();
    let orphan = dir.0.join("scratch/run-orphan");
    std::fs::create_dir(&orphan).unwrap();
    std::fs::write(orphan.join("lease"), b"").unwrap();
    std::fs::write(orphan.join("private-data"), b"synthetic").unwrap();
    let second = state(&dir);
    assert!(first_path.is_dir());
    assert!(!orphan.exists());
    assert_ne!(first.scratch.path, second.scratch.path);
}

#[tokio::test]
async fn failed_refresh_discards_old_catalog_and_can_be_retried() {
    let dir = Directory::new();
    let state = state(&dir);
    let source = dir.0.join("refresh.parquet");
    fixture(&source, "SELECT 1 AS old");
    let old = open(&state, &source).await;
    std::fs::write(&source, b"bad").unwrap();
    assert!(state.refresh(old.session_id.clone()).await.is_err());
    let failed_generation = old.generation + 1;
    assert!(state
        .metadata(
            old.session_id.clone(),
            failed_generation,
            format!("object-{}-{failed_generation}", old.session_id)
        )
        .await
        .is_err());
    std::fs::remove_file(&source).unwrap();
    fixture(&source, "SELECT 2 AS new");
    let new = state.refresh(old.session_id).await.unwrap();
    assert_eq!(new.generation, 3);
    assert_eq!(new.object.columns[0].name, "new");
    assert_eq!(
        text(&page(&state, &new, query(&new)).await.rows[0][0]),
        Some("2")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refresh_interrupts_an_old_query_and_cannot_reuse_its_operation() {
    let dir = Directory::new();
    let state = state(&dir);
    let source = dir.0.join("refresh-active.parquet");
    fixture(&source, "SELECT 1 AS id");
    let info = open(&state, &source).await;
    let req = request(&state, &info, OperationKind::Page, query(&info));
    let (tx, rx) = tokio::sync::oneshot::channel();
    let s = state.clone();
    let job = tokio::spawn(async move {
        s.run(req,OperationKind::Page,move|a,g,q,p,_|{let _=tx.send(());a.duckdb().unwrap().conn.query_row::<String,_,_>("SELECT sum(a.range*b.range)::VARCHAR FROM range(1000000000) a, range(1000000000) b",[],|r|r.get(0)).map_err(AppError::query)?;a.fetch_page(g,&q,p)}).await
    });
    rx.await.unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    let new = tokio::time::timeout(
        Duration::from_secs(5),
        state.refresh(info.session_id.clone()),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(job.await.unwrap().is_err());
    assert_eq!(new.generation, 2);
    assert_eq!(page(&state, &new, query(&new)).await.rows.len(), 1);
}

#[tokio::test]
async fn counts_and_profiles_use_filter_context_and_bounded_generation_local_caches() {
    let dir = Directory::new();
    let policy = ResourcePolicy {
        max_cache_entries: 2,
        ..ResourcePolicy::default()
    };
    let state = AppState::new(dir.0.join("scratch"), policy).unwrap();
    let source = dir.0.join("cache.parquet");
    fixture(
        &source,
        "SELECT range AS id, (range%2)::VARCHAR AS category FROM range(10)",
    );
    let info = open(&state, &source).await;
    for (category, expected) in [("0", 5), ("1", 5), ("missing", 0)] {
        let mut q = query(&info);
        q.filter = predicate(
            "col-1",
            FilterOp::Eq,
            FilterValue::Text {
                value: category.into(),
            },
        );
        let preview = page(&state, &info, q.clone()).await;
        assert_eq!(preview.count, CountState::Unknown);
        let req = request(&state, &info, OperationKind::Count, q.clone());
        let counted = state
            .run(req, OperationKind::Count, |a, g, q, p, _| a.count(g, &q, p))
            .await
            .unwrap();
        assert_eq!(counted.count, CountState::known(expected));
        let profile = state
            .run(
                request(
                    &state,
                    &info,
                    OperationKind::Profile,
                    ProfileQuery {
                        column_id: "col-1".into(),
                        context: q,
                    },
                ),
                OperationKind::Profile,
                |a, g, q, p, _| a.profile(g, &q, p),
            )
            .await
            .unwrap();
        assert_eq!(profile.non_null_count, expected.to_string());
    }
    state
        .run(
            request(&state, &info, OperationKind::Page, query(&info)),
            OperationKind::Page,
            |a, _, _, _, _| {
                let a = a.duckdb().unwrap();
                assert!(a.counts.len() <= 2);
                assert!(a.profiles.len() <= 2);
                Ok(())
            },
        )
        .await
        .unwrap();
    let fresh = state.refresh(info.session_id).await.unwrap();
    let mut q = query(&fresh);
    q.quick_search = "0".into();
    assert_eq!(page(&state, &fresh, q).await.count, CountState::Unknown);
}

#[tokio::test]
async fn typed_boolean_temporal_grouped_filters_and_nested_profiles_regressions() {
    let dir = Directory::new();
    let state = state(&dir);
    let source = dir.0.join("types.parquet");
    fixture(&source,"SELECT range AS id, range%2=0 AS flag, DATE '2026-10-21' AS day, INTERVAL '2 days' AS elapsed, {'v':[9007199254740993::BIGINT,NULL]} AS nested, map(['key'],[1]) AS mapping FROM range(4)");
    let info = open(&state, &source).await;
    assert_eq!(info.object.columns[3].kind, ColumnKind::Date);
    let mut q = query(&info);
    q.filter = FilterExpr::All {
        terms: vec![
            predicate("col-1", FilterOp::Eq, FilterValue::Boolean { value: false }),
            FilterExpr::Any {
                terms: vec![
                    predicate(
                        "col-0",
                        FilterOp::Eq,
                        FilterValue::Number { value: "1".into() },
                    ),
                    predicate(
                        "col-0",
                        FilterOp::Eq,
                        FilterValue::Number { value: "3".into() },
                    ),
                ],
            },
            predicate(
                "col-2",
                FilterOp::Eq,
                FilterValue::Temporal {
                    value: "2026-10-21".into(),
                },
            ),
        ],
    };
    assert_eq!(page(&state, &info, q).await.rows.len(), 2);
    for column in ["col-4", "col-5"] {
        let profile = state
            .run(
                request(
                    &state,
                    &info,
                    OperationKind::Profile,
                    ProfileQuery {
                        column_id: column.into(),
                        context: query(&info),
                    },
                ),
                OperationKind::Profile,
                |a, g, q, p, _| a.profile(g, &q, p),
            )
            .await
            .unwrap();
        assert_eq!(profile.non_null_count, "4");
        assert!(matches!(profile.min, Cell::Json { .. }));
    }
    let mut q = query(&info);
    q.filter = predicate(
        "col-2",
        FilterOp::Eq,
        FilterValue::Temporal {
            value: "2026-10-21 12:00".into(),
        },
    );
    assert_eq!(
        state
            .run(
                request(&state, &info, OperationKind::Page, q),
                OperationKind::Page,
                |a, g, q, p, _| a.fetch_page(g, &q, p)
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidInput
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_export_never_publishes_partial_output_and_cleans_staging() {
    let dir = Directory::new();
    let state = state(&dir);
    let source = dir.0.join("export-cancel.parquet");
    fixture(
        &source,
        "SELECT range AS id, repeat('x',1024) AS data FROM range(1000000)",
    );
    let info = open(&state, &source).await;
    let destination = dir.0.join("large.csv");
    let options = ExportOptions {
        destination: destination.to_str().unwrap().into(),
        format: ExportFormat::Csv,
        max_rows: 1000000,
        view: query(&info),
    };
    let req = request(&state, &info, OperationKind::Export, options);
    let id = req.operation_id.clone();
    let s = state.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let job = tokio::spawn(async move {
        s.run(req, OperationKind::Export, move |a, g, q, p, c| {
            let _ = tx.send(());
            a.export(g, q, p, c)
        })
        .await
    });
    rx.await.unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(
        state.cancel_operation(&id).unwrap().status,
        "cancel_pending"
    );
    let result = tokio::time::timeout(Duration::from_secs(5), job)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.unwrap_err().code, ErrorCode::Cancelled);
    assert!(!destination.exists());
    assert!(!std::fs::read_dir(&dir.0).unwrap().any(|e| e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".sivro-export-")));
    assert_eq!(page(&state, &info, query(&info)).await.rows.len(), 100);
}

#[tokio::test]
async fn bounded_session_and_reservation_slots_are_released_on_close() {
    let dir = Directory::new();
    let policy = ResourcePolicy {
        max_sessions: 1,
        max_pending_operations: 2,
        ..ResourcePolicy::default()
    };
    let state = AppState::new(dir.0.join("scratch"), policy).unwrap();
    let source = dir.0.join("slots.parquet");
    fixture(&source, "SELECT 1 AS id");
    let info = open(&state, &source).await;
    assert_eq!(
        state
            .open(source.to_str().unwrap().into(), OpenOptions::default())
            .await
            .unwrap_err()
            .code,
        ErrorCode::ResourceLimit
    );
    state
        .begin(
            info.session_id.clone(),
            info.generation,
            info.object.id.clone(),
            OperationKind::Page,
        )
        .unwrap();
    state
        .begin(
            info.session_id.clone(),
            info.generation,
            info.object.id.clone(),
            OperationKind::Page,
        )
        .unwrap();
    assert_eq!(
        state
            .begin(
                info.session_id.clone(),
                info.generation,
                info.object.id.clone(),
                OperationKind::Page
            )
            .unwrap_err()
            .code,
        ErrorCode::ResourceLimit
    );
    state.close(info.session_id).await.unwrap();
    let fresh = open(&state, &source).await;
    assert_eq!(page(&state, &fresh, query(&fresh)).await.rows.len(), 1);
}

#[tokio::test]
async fn read_only_input_permissions_and_binary_preview_are_preserved() {
    let dir = Directory::new();
    let state = state(&dir);
    let source = dir.0.join("readonly.parquet");
    fixture(&source, "SELECT repeat('x',200)::BLOB AS data");
    let original = std::fs::read(&source).unwrap();
    let mut permissions = source.metadata().unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&source, permissions).unwrap();
    let info = open(&state, &source).await;
    let p = page(&state, &info, query(&info)).await;
    assert!(
        matches!(&p.rows[0][0],Cell::Binary{byte_count,preview_hex,truncated:true} if byte_count=="200" && preview_hex.len()==256)
    );
    let full = state
        .run(
            request(
                &state,
                &info,
                OperationKind::ReadRow,
                RowQuery {
                    row_id: "0".into(),
                    column_ids: vec!["col-0".into()],
                    raw_record: false,
                },
            ),
            OperationKind::ReadRow,
            |a, g, q, p, _| a.read_row(g, &q, p),
        )
        .await
        .unwrap();
    assert!(
        matches!(&full.cells[0],Cell::Binary{preview_hex,truncated:false,..} if preview_hex.len()==400)
    );
    assert_eq!(std::fs::read(&source).unwrap(), original);
    state.close(info.session_id).await.unwrap();
    #[cfg(windows)]
    {
        let mut permissions = source.metadata().unwrap().permissions();
        permissions.set_readonly(false);
        std::fs::set_permissions(&source, permissions).unwrap();
    }
}

#[tokio::test]
async fn exact_numeric_and_binary_exports_preserve_values_with_explicit_encodings() {
    let dir = Directory::new();
    let state = state(&dir);
    let source = dir.0.join("export-exact.parquet");
    fixture(&source,"SELECT 9007199254740993::BIGINT AS id, 18446744073709551615::UBIGINT AS big, 123456789012345678.12345678901234567890::DECIMAL(38,20) AS d, '\\x00\\xFF'::BLOB AS bin");
    let info = open(&state, &source).await;
    for (format, ext) in [
        (ExportFormat::Json, "json"),
        (ExportFormat::Csv, "csv"),
        (ExportFormat::Parquet, "parquet"),
    ] {
        let dest = dir.0.join(format!("exact.{ext}"));
        let options = ExportOptions {
            destination: dest.to_str().unwrap().into(),
            format,
            max_rows: 2,
            view: query(&info),
        };
        let result = state
            .run(
                request(&state, &info, OperationKind::Export, options),
                OperationKind::Export,
                |a, g, q, p, c| a.export(g, q, p, c),
            )
            .await
            .unwrap();
        assert_eq!(result.exported_rows, "1");
        assert!(!result.truncated);
        if format == ExportFormat::Json {
            let values: Vec<serde_json::Value> =
                serde_json::from_slice(&std::fs::read(dest).unwrap()).unwrap();
            assert_eq!(values[0]["id"].to_string(), "9007199254740993");
            assert_eq!(values[0]["big"].to_string(), "18446744073709551615");
            assert_eq!(
                values[0]["d"].to_string(),
                "123456789012345678.12345678901234567890"
            );
            assert_eq!(values[0]["bin"]["$binary_hex"], "00FF");
            assert_eq!(values[0]["bin"]["byteCount"], "2");
            assert!(!result.warnings.is_empty());
        } else if format == ExportFormat::Csv {
            assert!(std::fs::read_to_string(dest).unwrap().contains("hex:00FF"));
            assert!(!result.warnings.is_empty());
        } else {
            let exported = open(&state, &dest).await;
            assert_eq!(exported.object.columns[3].kind, ColumnKind::Binary);
            assert_eq!(
                text(&page(&state, &exported, query(&exported)).await.rows[0][2]),
                Some("123456789012345678.12345678901234567890")
            );
        }
    }
}

#[tokio::test]
async fn empty_tables_and_json_null_are_not_conflated_with_sql_null() {
    let dir = Directory::new();
    let state = state(&dir);
    let source = dir.0.join("empty.parquet");
    fixture(&source, "SELECT 1::BIGINT AS id WHERE false");
    let info = open(&state, &source).await;
    let p = page(&state, &info, query(&info)).await;
    assert!(p.rows.is_empty());
    assert!(!p.has_more);
    assert_eq!(p.count, CountState::known(0));
    for (format, ext) in [
        (ExportFormat::Json, "json"),
        (ExportFormat::Csv, "csv"),
        (ExportFormat::Tsv, "tsv"),
        (ExportFormat::Jsonl, "jsonl"),
        (ExportFormat::Parquet, "parquet"),
    ] {
        let dest = dir.0.join(format!("empty-export.{ext}"));
        let options = ExportOptions {
            destination: dest.to_str().unwrap().into(),
            format,
            max_rows: 10,
            view: query(&info),
        };
        let result = state
            .run(
                request(&state, &info, OperationKind::Export, options),
                OperationKind::Export,
                |a, g, q, p, c| a.export(g, q, p, c),
            )
            .await
            .unwrap();
        assert_eq!(result.exported_rows, "0");
        assert!(!result.truncated);
        if format == ExportFormat::Jsonl {
            assert!(std::fs::read(&dest).unwrap().is_empty());
        }
        if format == ExportFormat::Json {
            assert_eq!(
                serde_json::from_slice::<Vec<serde_json::Value>>(&std::fs::read(dest).unwrap())
                    .unwrap()
                    .len(),
                0
            );
        }
    }
    let source = dir.0.join("nulls.parquet");
    fixture(
        &source,
        "SELECT 'null'::JSON AS jsonnull, NULL::JSON AS sqlnull",
    );
    let info = open(&state, &source).await;
    let p = page(&state, &info, query(&info)).await;
    assert!(matches!(&p.rows[0][0],Cell::Json{value,..} if value=="null"));
    assert_eq!(p.rows[0][1], Cell::Null);
}

#[tokio::test]
async fn versioned_wire_contract_and_normal_shutdown_cleanup() {
    let dir = Directory::new();
    let state = state(&dir);
    let source = dir.0.join("wire.parquet");
    fixture(&source, "SELECT 9007199254740993::BIGINT AS id");
    let info = open(&state, &source).await;
    let wire = serde_json::to_value(&info).unwrap();
    assert_eq!(wire["apiVersion"], 1);
    assert_eq!(wire["format"], "parquet");
    assert_eq!(wire["object"]["kind"], "table");
    assert_eq!(wire["object"]["rowCount"]["value"], "1");
    let p = page(&state, &info, query(&info)).await;
    let value = serde_json::to_value(&p).unwrap();
    assert_eq!(value["rows"][0][0]["kind"], "number");
    assert_eq!(value["rows"][0][0]["value"], "9007199254740993");
    assert_eq!(value["rows"][0][0]["logicalType"], "BIGINT");
    assert_eq!(value["columnIds"][0], "col-0");
    assert_eq!(value["rowIds"][0], "0");
    let request = request(&state, &info, OperationKind::Page, query(&info));
    let serialized = serde_json::to_value(request).unwrap();
    assert!(serialized.get("path").is_none());
    assert!(serialized["query"].get("path").is_none());
    let decoded: OperationRequest<Query> = serde_json::from_value(serialized).unwrap();
    assert_eq!(decoded.query.column_ids, vec!["col-0"]);
    let scratch = state.scratch.path.clone();
    state.drain_shutdown();
    assert!(!scratch.exists());
    assert_eq!(
        state
            .metadata(info.session_id, info.generation, info.object.id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidSession
    );
}

#[test]
fn completed_export_publication_refuses_racing_destinations_and_aliases() {
    let dir = Directory::new();
    let source = dir.0.join("source.parquet");
    fixture(&source, "SELECT 1 AS id");
    let before = std::fs::read(&source).unwrap();
    let destination = dir.0.join("race.parquet");
    let stage = export::ExportStage::new(
        destination.to_str().unwrap(),
        ExportFormat::Parquet,
        Some(&source),
        &ResourcePolicy::default(),
    )
    .unwrap();
    std::fs::write(&stage.file, b"complete output").unwrap();
    std::fs::hard_link(&source, &destination).unwrap();
    assert!(stage.publish().is_err());
    assert_eq!(std::fs::read(&source).unwrap(), before);
    assert_eq!(std::fs::read(&destination).unwrap(), before);
    drop(stage);
    assert!(!std::fs::read_dir(&dir.0).unwrap().any(|e| e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".sivro-export-")));
}

#[tokio::test]
async fn parquet_footer_budget_is_checked_before_metadata_allocation() {
    use std::io::Write;
    let dir = Directory::new();
    let state = state(&dir);
    let path = dir.0.join("oversized-footer.parquet");
    let bytes = 9 * 1024 * 1024u32;
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(b"PAR1").unwrap();
    file.set_len(4 + bytes as u64).unwrap();
    use std::io::Seek;
    file.seek(std::io::SeekFrom::End(0)).unwrap();
    file.write_all(&bytes.to_le_bytes()).unwrap();
    file.write_all(b"PAR1").unwrap();
    drop(file);
    assert_eq!(
        state
            .open(path.to_str().unwrap().into(), OpenOptions::default())
            .await
            .unwrap_err()
            .code,
        ErrorCode::ResourceLimit
    );
}

#[tokio::test]
async fn selected_source_glob_characters_are_literal_in_files_and_parent_directories() {
    let dir = Directory::new();
    let state = state(&dir);
    let literal_dir = dir.0.join("inputs[1]");
    let decoy_dir = dir.0.join("inputs1");
    std::fs::create_dir(&literal_dir).unwrap();
    std::fs::create_dir(&decoy_dir).unwrap();
    let source = literal_dir.join("data[1].parquet");
    fixture(&source, "SELECT 111 AS selected");
    fixture(&literal_dir.join("data1.parquet"), "SELECT 222 AS decoy");
    fixture(&decoy_dir.join("data1.parquet"), "SELECT 333 AS decoy");
    let info = open(&state, &source).await;
    assert_eq!(info.object.columns[0].name, "selected");
    assert_eq!(info.object.row_count, CountState::known(1));
    assert_eq!(
        text(&page(&state, &info, query(&info)).await.rows[0][0]),
        Some("111")
    );
    state.close(info.session_id).await.unwrap();

    #[cfg(unix)]
    {
        let source = dir.0.join("literal*?.parquet");
        fixture(&source, "SELECT 444 AS selected");
        fixture(&dir.0.join("literalXY.parquet"), "SELECT 555 AS decoy");
        let info = open(&state, &source).await;
        assert_eq!(info.object.columns[0].name, "selected");
        let rows = page(&state, &info, query(&info)).await.rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(text(&rows[0][0]), Some("444"));
        state.close(info.session_id).await.unwrap();

        let unsupported = dir.0.join(r"mixed\[1].parquet");
        fixture(&unsupported, "SELECT 666 AS selected");
        assert_eq!(
            state
                .open(unsupported.to_str().unwrap().into(), OpenOptions::default())
                .await
                .unwrap_err()
                .code,
            ErrorCode::InvalidInput
        );
    }
}

#[tokio::test]
async fn delimited_csv_tsv_preview_and_session_smoke() {
    use crate::adapters::delimited::DelimitedOptions;
    let dir = Directory::new();
    let state = state(&dir);
    for (ext, sep) in [("csv", ","), ("tsv", "\t")] {
        let path = dir.0.join(format!("source.{ext}"));
        std::fs::write(
            &path,
            format!("id{sep}name{sep}amount\n001{sep}Alice{sep}1.2500\n002{sep}Bob{sep}2.5000\n"),
        )
        .unwrap();
        let preview = state
            .preview_delimited(path.to_str().unwrap().into(), DelimitedOptions::default())
            .await
            .unwrap();
        assert_eq!(preview.options.delimiter, sep);
        assert!(preview.options.header);
        assert_eq!(preview.rows.len(), 2);
        assert_eq!(preview.columns[2].data_type, "DECIMAL(5,4)");
        let info = state
            .open(
                path.to_str().unwrap().into(),
                OpenOptions {
                    preview_token: Some(preview.token),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(info.object.row_count, CountState::Unknown);
        let result = page(&state, &info, query(&info)).await;
        assert_eq!(result.rows.len(), 2);
        assert_eq!(text(&result.rows[0][0]), Some("1"));
        assert_eq!(text(&result.rows[0][2]), Some("1.2500"));
        state.close(info.session_id).await.unwrap();
    }
}

async fn open_text(
    state: &Arc<AppState>,
    path: &Path,
    options: crate::adapters::delimited::DelimitedOptions,
) -> DatasetDescriptor {
    let preview = state
        .preview_delimited(path.to_str().unwrap().into(), options)
        .await
        .unwrap();
    state
        .open(
            path.to_str().unwrap().into(),
            OpenOptions {
                preview_token: Some(preview.token),
                ..Default::default()
            },
        )
        .await
        .unwrap()
}
async fn text_export(
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
                    max_rows: 10,
                    view,
                },
            ),
            OperationKind::Export,
            |a, g, q, p, c| a.export(g, q, p, c),
        )
        .await
}

#[tokio::test]
async fn delimited_both_families_full_workflow_and_five_encoding_roundtrips() {
    use crate::adapters::delimited::DelimitedOptions;
    let dir = Directory::new();
    let state = state(&dir);
    for (ext, sep) in [("csv", ","), ("tsv", "\t")] {
        let source = dir.0.join(format!("input.{ext}"));
        let original = format!("id{sep}name{sep}amount\r\n9007199254740993{sep}Alice{sep}1.2500\r\n9007199254740994{sep}Bob{sep}2.5000\r\n9007199254740995{sep}Alice{sep}3.7500\r\n");
        std::fs::write(&source, &original).unwrap();
        let info = open_text(&state, &source, DelimitedOptions::default()).await;
        assert_eq!(info.delimited.as_ref().unwrap().delimiter, sep);
        assert_eq!(
            page(&state, &info, query(&info)).await.count,
            CountState::Unknown
        );
        let mut q = query(&info);
        q.column_ids = vec!["col-2".into(), "col-0".into(), "col-1".into()];
        q.filter = predicate(
            "col-1",
            FilterOp::Eq,
            FilterValue::Text {
                value: "Alice".into(),
            },
        );
        q.sort = Some(SortSpec {
            column_id: "col-0".into(),
            descending: true,
        });
        let shown = page(&state, &info, q.clone()).await;
        assert_eq!(shown.row_ids, vec!["2", "0"]);
        assert_eq!(text(&shown.rows[0][1]), Some("9007199254740995"));
        let count = state
            .run(
                request(&state, &info, OperationKind::Count, q.clone()),
                OperationKind::Count,
                |a, g, q, p, _| a.count(g, &q, p),
            )
            .await
            .unwrap();
        assert_eq!(count.count, CountState::known(2));
        let profile = state
            .run(
                request(
                    &state,
                    &info,
                    OperationKind::Profile,
                    ProfileQuery {
                        column_id: "col-2".into(),
                        context: q.clone(),
                    },
                ),
                OperationKind::Profile,
                |a, g, q, p, _| a.profile(g, &q, p),
            )
            .await
            .unwrap();
        assert_eq!(profile.non_null_count, "2");
        assert_eq!(text(&profile.max), Some("3.7500"));
        let full = state
            .run(
                request(
                    &state,
                    &info,
                    OperationKind::ReadRow,
                    RowQuery {
                        row_id: shown.row_ids[0].clone(),
                        column_ids: q.column_ids.clone(),
                        raw_record: false,
                    },
                ),
                OperationKind::ReadRow,
                |a, g, q, p, _| a.read_row(g, &q, p),
            )
            .await
            .unwrap();
        assert_eq!(full.cells, shown.rows[0]);
        for (format, out_ext) in [
            (ExportFormat::Parquet, "parquet"),
            (ExportFormat::Csv, "csv"),
            (ExportFormat::Tsv, "tsv"),
            (ExportFormat::Json, "json"),
            (ExportFormat::Jsonl, "ndjson"),
        ] {
            let dest = dir.0.join(format!("{ext}-out.{out_ext}"));
            let exported = text_export(&state, &info, &dest, format, q.clone())
                .await
                .unwrap();
            assert_eq!(exported.exported_rows, "2");
            assert!(!exported.truncated);
            if matches!(format, ExportFormat::Json | ExportFormat::Jsonl) {
                let output = std::fs::read_to_string(&dest).unwrap();
                let values: Vec<serde_json::Value> = if format == ExportFormat::Json {
                    serde_json::from_str(&output).unwrap()
                } else {
                    output
                        .lines()
                        .map(|l| serde_json::from_str(l).unwrap())
                        .collect()
                };
                assert_eq!(values.len(), 2);
                assert_eq!(values[0]["id"].to_string(), "9007199254740995");
                assert_eq!(
                    values[0]["amount"].to_string().trim_end_matches('0'),
                    "3.75"
                );
            } else {
                let reopened = if format == ExportFormat::Parquet {
                    open(&state, &dest).await
                } else {
                    open_text(&state, &dest, DelimitedOptions::default()).await
                };
                let values = page(&state, &reopened, query(&reopened)).await;
                assert_eq!(values.rows.len(), 2);
                assert_eq!(text(&values.rows[0][1]), Some("9007199254740995"));
                assert_eq!(
                    text(&values.rows[0][0]).unwrap().trim_end_matches('0'),
                    "3.75"
                );
                state.close(reopened.session_id).await.unwrap();
            }
        }
        assert_eq!(std::fs::read_to_string(&source).unwrap(), original);
        state.close(info.session_id).await.unwrap();
    }
}

#[tokio::test]
async fn delimited_bom_quoted_separators_newlines_escaped_quotes_and_null_policy() {
    use crate::adapters::delimited::DelimitedOptions;
    let dir = Directory::new();
    let state = state(&dir);
    for (ext, sep) in [("csv", ","), ("tsv", "\t")] {
        let path = dir.0.join(format!("quoted.{ext}"));
        std::fs::write(&path,format!("\u{feff}id{sep}value\r\n001{sep}\"é雪🌍a{sep}b\"\r\n002{sep}\"two\nlines\"\r\n003{sep}\"say \"\"hi\"\"\"\r\n004{sep}\\N\r\n005{sep}\"\\N\"\r\n006{sep}\r\n")).unwrap();
        let info = open_text(
            &state,
            &path,
            DelimitedOptions {
                force_text: true,
                ..Default::default()
            },
        )
        .await;
        let rows = page(&state, &info, query(&info)).await.rows;
        assert_eq!(rows.len(), 6);
        assert_eq!(text(&rows[0][0]), Some("001"));
        assert_eq!(text(&rows[0][1]), Some(format!("é雪🌍a{sep}b").as_str()));
        assert_eq!(text(&rows[1][1]), Some("two\nlines"));
        assert_eq!(text(&rows[2][1]), Some("say \"hi\""));
        assert_eq!(rows[3][1], Cell::Null);
        assert_eq!(text(&rows[4][1]), Some(r"\N"));
        assert_eq!(text(&rows[5][1]), Some(""));
        for (format, suffix) in [(ExportFormat::Csv, "csv"), (ExportFormat::Tsv, "tsv")] {
            assert_eq!(
                text_export(
                    &state,
                    &info,
                    &dir.0.join(format!("collision-{ext}.{suffix}")),
                    format,
                    query(&info)
                )
                .await
                .unwrap_err()
                .code,
                ErrorCode::Export
            );
        }
        state.close(info.session_id).await.unwrap();
        let empty_marker = open_text(
            &state,
            &path,
            DelimitedOptions {
                header: Some(true),
                force_text: true,
                null_marker: Some(String::new()),
                ..Default::default()
            },
        )
        .await;
        let rows = page(&state, &empty_marker, query(&empty_marker)).await.rows;
        assert_eq!(text(&rows[3][1]), Some(r"\N"));
        assert_eq!(rows[5][1], Cell::Null);
        state.close(empty_marker.session_id).await.unwrap();
    }
}

#[tokio::test]
async fn delimited_duplicate_blank_and_marker_headers_remain_independent() {
    use crate::adapters::delimited::DelimitedOptions;
    let dir = Directory::new();
    let state = state(&dir);
    for (ext, sep) in [("csv", ","), ("tsv", "\t")] {
        let path = dir.0.join(format!("labels.{ext}"));
        std::fs::write(
            &path,
            format!("same{sep}same{sep}{sep}\\N\n1{sep}2{sep}3{sep}4\n"),
        )
        .unwrap();
        let info = open_text(
            &state,
            &path,
            DelimitedOptions {
                header: Some(true),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(
            info.object
                .columns
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            vec!["same", "same", "", r"\N"]
        );
        let mut q = query(&info);
        q.column_ids = vec!["col-1".into(), "col-0".into(), "col-2".into()];
        let rows = page(&state, &info, q.clone()).await.rows;
        assert_eq!(text(&rows[0][0]), Some("2"));
        let dest = dir.0.join(format!("labels-{ext}.json"));
        let exported = text_export(&state, &info, &dest, ExportFormat::Json, q)
            .await
            .unwrap();
        assert!(!exported.warnings.is_empty());
        let values: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dest).unwrap()).unwrap();
        assert_eq!(values[0]["col-1"], 2);
        assert_eq!(values[0]["col-0"], 1);
        state.close(info.session_id).await.unwrap();
    }
}

#[tokio::test]
async fn delimited_overrides_headerless_custom_dialects_and_exact_inference() {
    use crate::adapters::delimited::DelimitedOptions;
    let dir = Directory::new();
    let state = state(&dir);
    let path = dir.0.join("custom.csv");
    std::fs::write(
        &path,
        "001;'a;b';123456789012345678.12345678901234567890\n002;'c''d';0.00000000000000000001\n",
    )
    .unwrap();
    let info = open_text(
        &state,
        &path,
        DelimitedOptions {
            delimiter: Some(";".into()),
            header: Some(false),
            quote: Some("'".into()),
            escape: Some("'".into()),
            type_overrides: [("col-0".into(), "VARCHAR".into())].into(),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(info.object.columns[0].name, "Column 1");
    let rows = page(&state, &info, query(&info)).await.rows;
    assert_eq!(text(&rows[0][0]), Some("001"));
    assert_eq!(text(&rows[0][1]), Some("a;b"));
    assert_eq!(text(&rows[1][1]), Some("c'd"));
    assert_eq!(
        text(&rows[0][2]),
        Some("123456789012345678.12345678901234567890")
    );
    state.close(info.session_id).await.unwrap();
    let path = dir.0.join("auto.csv");
    std::fs::write(&path, "id|text\n1|a\n2|b\n").unwrap();
    let p = state
        .preview_delimited(
            path.to_str().unwrap().into(),
            DelimitedOptions {
                delimiter: Some("auto".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(p.options.delimiter, "|");
    for bad in ["DOUBLE); DROP TABLE x;--", "DECIMAL(39,2)", "DECIMAL(2,3)"] {
        assert!(crate::adapters::delimited::type_override(bad).is_err());
    }
}

#[tokio::test]
async fn delimited_strict_encoding_malformed_rows_and_stale_preview_are_rejected() {
    use crate::adapters::delimited::DelimitedOptions;
    let dir = Directory::new();
    let state = state(&dir);
    for (i, bytes) in [
        b"a,b\n1,2,3\n".as_slice(),
        b"a,b\n1,\"unclosed\n",
        b"a,b\n1,\xff\n",
        b"\xff\xfea\0,b\0",
        b"",
    ]
    .iter()
    .enumerate()
    {
        let path = dir.0.join(format!("bad{i}.csv"));
        std::fs::write(&path, bytes).unwrap();
        assert!(
            state
                .preview_delimited(
                    path.to_str().unwrap().into(),
                    DelimitedOptions {
                        header: Some(true),
                        ..Default::default()
                    }
                )
                .await
                .is_err(),
            "bad fixture {i}"
        );
    }
    let path = dir.0.join("late-encoding.tsv");
    let mut invalid = b"id\ttext\n1\t".to_vec();
    invalid.extend(vec![b'a'; 70_000]);
    invalid.extend([0xff, b'\n']);
    std::fs::write(&path, invalid).unwrap();
    assert!(state
        .preview_delimited(path.to_str().unwrap().into(), DelimitedOptions::default())
        .await
        .is_err());
    let path = dir.0.join("change.csv");
    std::fs::write(&path, "id\n1\n").unwrap();
    assert_eq!(
        state
            .open(path.to_str().unwrap().into(), OpenOptions::default())
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidInput
    );
    let p = state
        .preview_delimited(path.to_str().unwrap().into(), DelimitedOptions::default())
        .await
        .unwrap();
    std::fs::write(&path, "id\n1234\n").unwrap();
    assert_eq!(
        state
            .open(
                path.to_str().unwrap().into(),
                OpenOptions {
                    preview_token: Some(p.token),
                    ..Default::default()
                }
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::StaleDataset
    );
}

#[tokio::test]
async fn delimited_million_row_preview_is_bounded_and_late_precision_mismatches_fail() {
    use crate::adapters::delimited::DelimitedOptions;
    use std::io::Write;
    let dir = Directory::new();
    let state = state(&dir);
    for ext in ["csv", "tsv"] {
        let path = dir.0.join(format!("million.{ext}"));
        let mut f = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
        writeln!(f, "id").unwrap();
        for i in 0..1_000_000 {
            writeln!(f, "{i}").unwrap();
        }
        drop(f);
        let p = state
            .preview_delimited(path.to_str().unwrap().into(), DelimitedOptions::default())
            .await
            .unwrap();
        assert_eq!(p.sampled_rows, 20_480);
        assert_eq!(p.rows.len(), 20);
        assert!(p.has_more);
        let info = state
            .open(
                path.to_str().unwrap().into(),
                OpenOptions {
                    preview_token: Some(p.token),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let page = page(&state, &info, query(&info)).await;
        assert_eq!(page.rows.len(), 100);
        assert!(page.has_more);
        assert_eq!(page.count, CountState::Unknown);
        state.close(info.session_id).await.unwrap();
    }
    let policy = ResourcePolicy {
        schema_sample_rows: 8,
        ..Default::default()
    };
    let bounded = AppState::new(dir.0.join("bounded"), policy).unwrap();
    for (name, first, last) in [("decimal", "1.20", "1.234"), ("integer", "1", "1.5")] {
        let path = dir.0.join(format!("late-{name}.csv"));
        std::fs::write(
            &path,
            format!("value\n{}{last}\n", format!("{first}\n").repeat(20)),
        )
        .unwrap();
        let info = open_text(
            &bounded,
            &path,
            DelimitedOptions {
                header: Some(true),
                ..Default::default()
            },
        )
        .await;
        let result = bounded
            .run(
                request(&bounded, &info, OperationKind::Page, query(&info)),
                OperationKind::Page,
                |a, g, q, p, _| a.fetch_page(g, &q, p),
            )
            .await;
        assert!(result.is_err(), "{name} rounding must fail");
    }
}

#[tokio::test]
async fn delimited_refresh_freezes_schema_and_reopen_recovers_text_and_null_markers() {
    use crate::adapters::delimited::DelimitedOptions;
    let dir = Directory::new();
    let state = state(&dir);
    let path = dir.0.join("refresh.csv");
    std::fs::write(&path, "id,value\n001,NULL\n").unwrap();
    let info = open_text(
        &state,
        &path,
        DelimitedOptions {
            null_marker: Some("NULL".into()),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        page(&state, &info, query(&info)).await.rows[0][1],
        Cell::Null
    );
    std::fs::write(&path, "id,value\n002,NULL\n003,new\n").unwrap();
    let fresh = state.refresh(info.session_id.clone()).await.unwrap();
    assert!(fresh.generation > info.generation);
    assert_eq!(fresh.object.columns[0].data_type, "BIGINT");
    assert_eq!(page(&state, &fresh, query(&fresh)).await.rows.len(), 2);
    std::fs::write(&path, "different,value\n004,NULL\n").unwrap();
    assert!(state.refresh(info.session_id.clone()).await.is_err());
    state.close(info.session_id).await.unwrap();
    let recovered = open_text(
        &state,
        &path,
        DelimitedOptions {
            header: Some(true),
            force_text: true,
            null_marker: Some(String::new()),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        text(&page(&state, &recovered, query(&recovered)).await.rows[0][0]),
        Some("004")
    );
    state.close(recovered.session_id).await.unwrap();
}

#[tokio::test]
async fn delimited_precise_decimals_and_very_wide_numbers_export_without_rounding() {
    use crate::adapters::delimited::DelimitedOptions;
    let dir = Directory::new();
    let state = state(&dir);
    for (ext, sep) in [("csv", ","), ("tsv", "\t")] {
        let path = dir.0.join(format!("precise.{ext}"));
        std::fs::write(&path,format!("small{sep}wide{sep}beyond\n1.12345678901234567{sep}123456789012345678.12345678901234567890{sep}12345678901234567890123456789012345678901234567890\n")).unwrap();
        let info = open_text(&state, &path, DelimitedOptions::default()).await;
        let first = page(&state, &info, query(&info)).await.rows.remove(0);
        assert_eq!(text(&first[0]), Some("1.12345678901234567"));
        assert_eq!(
            text(&first[1]),
            Some("123456789012345678.12345678901234567890")
        );
        assert_eq!(info.object.columns[2].data_type, "VARCHAR");
        for (format, suffix) in [
            (ExportFormat::Json, "json"),
            (ExportFormat::Jsonl, "jsonl"),
            (ExportFormat::Csv, "csv"),
            (ExportFormat::Tsv, "tsv"),
            (ExportFormat::Parquet, "parquet"),
        ] {
            let dest = dir.0.join(format!("{ext}-precise.{suffix}"));
            text_export(&state, &info, &dest, format, query(&info))
                .await
                .unwrap();
            if matches!(format, ExportFormat::Json | ExportFormat::Jsonl) {
                let body = std::fs::read_to_string(&dest).unwrap();
                let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
                let row = if format == ExportFormat::Json {
                    &parsed[0]
                } else {
                    &parsed
                };
                assert_eq!(row["small"].to_string(), "1.12345678901234567");
                assert_eq!(
                    row["wide"].to_string(),
                    "123456789012345678.12345678901234567890"
                );
                assert_eq!(row["beyond"].as_str(), text(&first[2]));
            } else {
                let opened = if format == ExportFormat::Parquet {
                    open(&state, &dest).await
                } else {
                    open_text(&state, &dest, DelimitedOptions::default()).await
                };
                let row = page(&state, &opened, query(&opened)).await.rows.remove(0);
                assert_eq!(text(&row[0]), text(&first[0]));
                assert_eq!(text(&row[1]), text(&first[1]));
                assert_eq!(text(&row[2]), text(&first[2]));
                state.close(opened.session_id).await.unwrap();
            }
        }
        state.close(info.session_id).await.unwrap();
    }
}

#[tokio::test]
async fn delimited_readonly_sources_glob_names_aliases_and_preview_handles_are_safe() {
    use crate::adapters::delimited::DelimitedOptions;
    let dir = Directory::new();
    let state = state(&dir);
    for ext in ["csv", "tsv"] {
        let source = dir.0.join(format!("source[1].{ext}"));
        let before = b"id\n001\n";
        std::fs::write(&source, before).unwrap();
        std::fs::write(dir.0.join(format!("source1.{ext}")), b"decoy\n999\n").unwrap();
        let mut permissions = source.metadata().unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&source, permissions).unwrap();
        let info = open_text(
            &state,
            &source,
            DelimitedOptions {
                force_text: true,
                ..Default::default()
            },
        )
        .await;
        assert_eq!(info.object.columns[0].name, "id");
        assert_eq!(
            text(&page(&state, &info, query(&info)).await.rows[0][0]),
            Some("001")
        );
        for (format, suffix) in [
            (ExportFormat::Csv, "csv"),
            (ExportFormat::Tsv, "tsv"),
            (ExportFormat::Parquet, "parquet"),
            (ExportFormat::Json, "json"),
            (ExportFormat::Jsonl, "ndjson"),
        ] {
            let alias = dir.0.join(format!("alias-{ext}.{suffix}"));
            std::fs::hard_link(&source, &alias).unwrap();
            assert_eq!(
                text_export(&state, &info, &alias, format, query(&info))
                    .await
                    .unwrap_err()
                    .code,
                ErrorCode::Export
            );
        }
        assert_eq!(std::fs::read(&source).unwrap(), before);
        state.close(info.session_id).await.unwrap();
        let preview = state
            .preview_delimited(source.to_str().unwrap().into(), DelimitedOptions::default())
            .await
            .unwrap();
        state.discard_preview(&preview.token).unwrap();
        assert!(state
            .open(
                source.to_str().unwrap().into(),
                OpenOptions {
                    preview_token: Some(preview.token),
                    ..Default::default()
                }
            )
            .await
            .is_err());
        #[cfg(windows)]
        {
            let mut p = source.metadata().unwrap().permissions();
            p.set_readonly(false);
            std::fs::set_permissions(&source, p).unwrap();
        }
    }
}

#[tokio::test]
async fn delimited_explicit_types_apply_over_force_text_and_reject_lossy_overrides() {
    use crate::adapters::delimited::DelimitedOptions;
    let dir = Directory::new();
    let state = state(&dir);
    for (ext, sep) in [("csv", ","), ("tsv", "\t")] {
        let path = dir.0.join(format!("overrides.{ext}"));
        std::fs::write(&path,format!("id{sep}n{sep}day{sep}stamp{sep}flag\n001{sep}1.20{sep}2026-10-21{sep}2026-10-21 10:00:00.123456{sep}true\n")).unwrap();
        let options = DelimitedOptions {
            header: Some(true),
            force_text: true,
            type_overrides: [
                ("col-0".into(), "BIGINT".into()),
                ("col-1".into(), "DECIMAL(5,2)".into()),
                ("col-2".into(), "DATE".into()),
                ("col-3".into(), "TIMESTAMP".into()),
                ("col-4".into(), "BOOLEAN".into()),
            ]
            .into(),
            ..Default::default()
        };
        let info = open_text(&state, &path, options).await;
        let row = page(&state, &info, query(&info)).await.rows.remove(0);
        assert_eq!(text(&row[0]), Some("1"));
        assert_eq!(text(&row[1]), Some("1.20"));
        assert_eq!(text(&row[2]), Some("2026-10-21"));
        assert_eq!(text(&row[3]), Some("2026-10-21 10:00:00.123456"));
        assert_eq!(row[4], Cell::Boolean { value: true });
        state.close(info.session_id).await.unwrap();
        for (ty, value) in [
            ("BIGINT", "1.5"),
            ("DECIMAL(5,2)", "1.234"),
            ("DATE", "2026-10-21 23:00:00"),
            ("TIMESTAMP", "2026-10-21 10:00:00.1234567"),
        ] {
            std::fs::write(&path, format!("value\n{value}\n")).unwrap();
            let result = state
                .preview_delimited(
                    path.to_str().unwrap().into(),
                    DelimitedOptions {
                        header: Some(true),
                        type_overrides: [("col-0".into(), ty.into())].into(),
                        ..Default::default()
                    },
                )
                .await;
            assert!(result.is_err(), "{ty} must reject lossy input");
        }
    }
}

mod json;

mod excel;

mod sqlite;

mod e2e_fixtures;

mod hardening;

mod perf;

mod arrow;
