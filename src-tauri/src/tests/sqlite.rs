//! SQLite coverage. Fixtures are written here with a separate
//! read-write rusqlite connection; Sivro's own connection is read-only.
use super::*;
use rusqlite::Connection as Writer;

const BIG: i64 = 9_007_199_254_740_993;

async fn open_object(
    state: &Arc<AppState>,
    path: &Path,
    object: &str,
) -> Result<DatasetDescriptor, AppError> {
    state
        .open(
            path.to_str().unwrap().into(),
            OpenOptions {
                sqlite: Some(SqliteOptions {
                    object: object.into(),
                }),
                ..Default::default()
            },
        )
        .await
}
async fn database(state: &Arc<AppState>, path: &Path) -> Result<DatabaseInfo, AppError> {
    state.inspect_database(path.to_str().unwrap().into()).await
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
async fn export_to(
    state: &Arc<AppState>,
    info: &DatasetDescriptor,
    path: &Path,
    format: ExportFormat,
    view: Query,
) -> Result<ExportResult, AppError> {
    let options = ExportOptions {
        destination: path.to_str().unwrap().into(),
        format,
        max_rows: 1_000,
        view,
    };
    state
        .run(
            request(state, info, OperationKind::Export, options),
            OperationKind::Export,
            |a, g, q, p, c| a.export(g, q, p, c),
        )
        .await
}
fn show(cell: &Cell) -> String {
    match cell {
        Cell::Null => "NULL".into(),
        Cell::Boolean { value } => value.to_string(),
        Cell::Number {
            value,
            logical_type,
            ..
        } => format!("{logical_type}:{value}"),
        Cell::Text { value, .. } => format!("text:{value}"),
        Cell::Json { value, .. } => format!("json:{value}"),
        Cell::Binary {
            preview_hex,
            byte_count,
            ..
        } => format!("blob:{byte_count}:{preview_hex}"),
    }
}
fn shown(page: &PageResult) -> Vec<Vec<String>> {
    page.rows
        .iter()
        .map(|r| r.iter().map(show).collect())
        .collect()
}
fn fixture(path: &Path) {
    let db = Writer::open(path).unwrap();
    db.execute_batch(&format!(
        r#"
        CREATE TABLE "odd ""name"" ✓" (id INTEGER PRIMARY KEY AUTOINCREMENT, label TEXT, amount REAL, big INTEGER, data BLOB, mixed, "select" TEXT);
        INSERT INTO "odd ""name"" ✓" (label, amount, big, data, mixed, "select") VALUES
          ('Alice', 1.5, {BIG}, x'00ff', 1, 'a'),
          ('', -0.25, 9223372036854775807, NULL, 'a', 'b'),
          (NULL, 1e300, -9223372036854775808, x'', x'01', 'c'),
          ('ÉMILE alice', 2.0, 0, x'0102', 2.5, '\N');
        CREATE TABLE pairs (a TEXT, b INTEGER, v, PRIMARY KEY (a, b)) WITHOUT ROWID;
        INSERT INTO pairs VALUES ('x', 2, 'later'), ('x', 1, 'first'), ('y', 1, 'other');
        CREATE TABLE shadowed (rowid TEXT, oid TEXT, _rowid_ TEXT, v INTEGER);
        INSERT INTO shadowed VALUES ('r', 'o', 'u', 10), ('r2', 'o2', 'u2', 20);
        CREATE VIEW dup AS SELECT label, label, amount FROM "odd ""name"" ✓" ORDER BY id;
        CREATE VIRTUAL TABLE docs USING fts5(body);
        "#
    ))
    .unwrap();
}

#[tokio::test]
async fn sqlite_tables_keys_views_mixed_values_and_five_exports() {
    let dir = Directory::new();
    let state = state(&dir);
    let path = dir.0.join("data.sqlite3");
    fixture(&path);
    let original = std::fs::read(&path).unwrap();
    let db = database(&state, &path).await.unwrap();
    assert_eq!(db.journal_mode, "delete");
    let objects: Vec<_> = db
        .objects
        .iter()
        .map(|o| (o.name.as_str(), o.kind, o.selectable))
        .collect();
    assert_eq!(
        objects,
        [
            ("docs", DbObjectKind::Virtual, false),
            ("dup", DbObjectKind::View, true),
            ("odd \"name\" ✓", DbObjectKind::Table, true),
            ("pairs", DbObjectKind::Table, true),
            ("shadowed", DbObjectKind::Table, true)
        ]
    );
    assert!(db.objects[3].without_rowid);
    assert_eq!(
        open_object(&state, &path, "docs").await.unwrap_err().code,
        ErrorCode::InvalidObject
    );
    assert_eq!(
        open_object(&state, &path, "sqlite_sequence")
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidObject
    );
    assert_eq!(
        state
            .open(path.to_str().unwrap().into(), OpenOptions::default())
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidInput
    );

    let info = open_object(&state, &path, "odd \"name\" ✓").await.unwrap();
    assert_eq!(info.format, FormatKind::Sqlite);
    assert_eq!(info.object.row_count, CountState::Unknown);
    let decl: Vec<_> = info
        .object
        .columns
        .iter()
        .map(|c| (c.name.as_str(), c.data_type.as_str(), c.kind))
        .collect();
    assert_eq!(
        decl,
        [
            ("id", "INTEGER", ColumnKind::Number),
            ("label", "TEXT", ColumnKind::Text),
            ("amount", "REAL", ColumnKind::Number),
            ("big", "INTEGER", ColumnKind::Number),
            ("data", "BLOB", ColumnKind::Other),
            ("mixed", "ANY", ColumnKind::Other),
            ("select", "TEXT", ColumnKind::Text)
        ]
    );
    let all = try_page(&state, &info, query(&info)).await.unwrap();
    assert_eq!(all.row_ids, ["1", "2", "3", "4"]);
    let v = shown(&all);
    assert_eq!(
        v[0],
        [
            "INTEGER:1",
            "text:Alice",
            "REAL:1.5",
            &format!("INTEGER:{BIG}"),
            "blob:2:00FF",
            "INTEGER:1",
            "text:a"
        ]
    );
    assert_eq!(v[1][1], "text:", "empty text is not NULL");
    assert_eq!(v[2][1], "NULL");
    assert_eq!(v[1][3], "INTEGER:9223372036854775807");
    assert_eq!(v[2][3], "INTEGER:-9223372036854775808");
    assert_eq!(v[2][2], "REAL:1e300");
    assert_eq!(v[2][4], "blob:0:", "an empty blob is not NULL");
    assert_eq!(
        [v[1][5].as_str(), v[2][5].as_str(), v[3][5].as_str()],
        ["text:a", "blob:1:01", "REAL:2.5"]
    );

    let mut q = query(&info);
    q.filter = predicate(
        "col-3",
        FilterOp::Eq,
        FilterValue::Number {
            value: BIG.to_string(),
        },
    );
    assert_eq!(
        try_page(&state, &info, q.clone()).await.unwrap().row_ids,
        ["1"]
    );
    q.filter = predicate(
        "col-2",
        FilterOp::Gt,
        FilterValue::Number {
            value: "1.75".into(),
        },
    );
    assert_eq!(
        try_page(&state, &info, q.clone()).await.unwrap().row_ids,
        ["3", "4"]
    );
    q.filter = predicate(
        "col-1",
        FilterOp::Contains,
        FilterValue::Text {
            value: "ALICE".into(),
        },
    );
    assert_eq!(
        try_page(&state, &info, q.clone()).await.unwrap().row_ids,
        ["1", "4"],
        "ASCII case folding"
    );
    q.filter = predicate(
        "col-5",
        FilterOp::Contains,
        FilterValue::Text { value: "a".into() },
    );
    assert_eq!(
        try_page(&state, &info, q.clone()).await.unwrap().row_ids,
        ["2"],
        "blobs never match text"
    );
    q.filter = predicate(
        "col-5",
        FilterOp::NotContains,
        FilterValue::Text { value: "a".into() },
    );
    assert_eq!(
        try_page(&state, &info, q.clone()).await.unwrap().row_ids,
        ["1", "3", "4"]
    );
    for (filter, code) in [
        (
            predicate(
                "col-3",
                FilterOp::Eq,
                FilterValue::Number {
                    value: "9223372036854775808".into(),
                },
            ),
            ErrorCode::InvalidInput,
        ),
        (
            predicate(
                "col-5",
                FilterOp::Eq,
                FilterValue::Text { value: "a".into() },
            ),
            ErrorCode::InvalidInput,
        ),
        (
            predicate(
                "col-1",
                FilterOp::Eq,
                FilterValue::Text {
                    value: "x'); DROP TABLE pairs; --".into(),
                },
            ),
            ErrorCode::InvalidInput,
        ),
    ] {
        q.filter = filter;
        match try_page(&state, &info, q.clone()).await {
            Ok(page) => assert!(page.rows.is_empty()),
            Err(error) => assert_eq!(error.code, code),
        }
    }
    q.filter = FilterExpr::default();
    q.quick_search = "émile".into();
    assert!(
        try_page(&state, &info, q.clone())
            .await
            .unwrap()
            .rows
            .is_empty(),
        "LIKE folds ASCII only"
    );
    q.quick_search.clear();
    q.sort = Some(SortSpec {
        column_id: "col-5".into(),
        descending: false,
    });
    assert_eq!(
        try_page(&state, &info, q.clone()).await.unwrap().row_ids,
        ["1", "4", "2", "3"],
        "numbers, text, then blobs"
    );
    let count = state
        .run(
            request(&state, &info, OperationKind::Count, query(&info)),
            OperationKind::Count,
            |a, g, q, p, _| a.count(g, &q, p),
        )
        .await
        .unwrap();
    assert_eq!(count.count, CountState::known(4));
    let profile = state
        .run(
            request(
                &state,
                &info,
                OperationKind::Profile,
                ProfileQuery {
                    column_id: "col-5".into(),
                    context: query(&info),
                },
            ),
            OperationKind::Profile,
            |a, g, q, p, _| a.profile(g, &q, p),
        )
        .await
        .unwrap();
    assert_eq!(
        (profile.null_count.as_str(), profile.distinct_count.as_str()),
        ("0", "4")
    );
    assert!(
        profile.warnings[0].contains("blob 1, integer 1, real 1, text 1"),
        "{:?}",
        profile.warnings
    );
    assert_eq!(show(&profile.max), "blob:1:01");
    let row = state
        .run(
            request(
                &state,
                &info,
                OperationKind::ReadRow,
                RowQuery {
                    row_id: "1".into(),
                    column_ids: vec!["col-3".into(), "col-4".into()],
                    raw_record: false,
                },
            ),
            OperationKind::ReadRow,
            |a, g, q, p, _| a.read_row(g, &q, p),
        )
        .await
        .unwrap();
    assert_eq!(
        row.cells.iter().map(show).collect::<Vec<_>>(),
        [format!("INTEGER:{BIG}"), "blob:2:00FF".into()]
    );

    for (format, ext) in [(ExportFormat::Json, "json"), (ExportFormat::Jsonl, "jsonl")] {
        let dest = dir.0.join(format!("out.{ext}"));
        let result = export_to(&state, &info, &dest, format, query(&info))
            .await
            .unwrap();
        assert_eq!(result.exported_rows, "4");
        let body = std::fs::read_to_string(&dest).unwrap();
        assert!(body.contains(&format!("\"big\":{BIG}")));
        assert!(body.contains("\"big\":-9223372036854775808"));
        assert!(body.contains(r#""data":{"$binary_hex":"00FF","byteCount":"2"}"#));
        assert!(body.contains(r#""label":"""#) && body.contains(r#""label":null"#));
        assert!(body.contains(r#""amount":1e300"#));
        let reopened = open(&state, &dest).await;
        assert_eq!(reopened.object.row_count, CountState::known(4));
        state.close(reopened.session_id).await.unwrap();
    }
    for (format, ext, sep) in [
        (ExportFormat::Csv, "csv", ','),
        (ExportFormat::Tsv, "tsv", '\t'),
    ] {
        let dest = dir.0.join(format!("out.{ext}"));
        let error = export_to(&state, &info, &dest, format, query(&info))
            .await
            .unwrap_err();
        assert!(
            error.message.contains(r"\N"),
            "a literal \\N collides with the NULL marker"
        );
        assert!(!dest.exists());
        let mut view = query(&info);
        view.column_ids = ["col-1", "col-3", "col-4"].map(String::from).to_vec();
        export_to(&state, &info, &dest, format, view).await.unwrap();
        let body = std::fs::read_to_string(&dest).unwrap();
        assert_eq!(
            body.lines().next().unwrap(),
            format!("label{sep}big{sep}data")
        );
        assert_eq!(
            body.lines().nth(1).unwrap(),
            format!("Alice{sep}{BIG}{sep}hex:00FF")
        );
        assert_eq!(
            body.lines().nth(2).unwrap(),
            format!("\"\"{sep}9223372036854775807{sep}\\N")
        );
    }
    let parquet = dir.0.join("out.parquet");
    let error = export_to(&state, &info, &parquet, ExportFormat::Parquet, query(&info))
        .await
        .unwrap_err();
    assert!(error.message.contains("mixed"), "{}", error.message);
    assert!(!parquet.exists());
    let mut view = query(&info);
    view.column_ids = ["col-0", "col-1", "col-2", "col-3", "col-4"]
        .map(String::from)
        .to_vec();
    export_to(&state, &info, &parquet, ExportFormat::Parquet, view)
        .await
        .unwrap();
    let reopened = open(&state, &parquet).await;
    assert_eq!(
        reopened
            .object
            .columns
            .iter()
            .map(|c| c.data_type.as_str())
            .collect::<Vec<_>>(),
        ["BIGINT", "VARCHAR", "DOUBLE", "BIGINT", "BLOB"]
    );
    let back = page(&state, &reopened, query(&reopened)).await;
    assert_eq!(text(&back.rows[0][3]), Some(BIG.to_string().as_str()));
    assert_eq!(back.rows[2][1], Cell::Null);
    assert_eq!(text(&back.rows[1][1]), Some(""));
    state.close(reopened.session_id).await.unwrap();
    state.close(info.session_id).await.unwrap();

    // WITHOUT ROWID: composite keys order rows and re-find them exactly.
    let pairs = open_object(&state, &path, "pairs").await.unwrap();
    let rows = try_page(&state, &pairs, query(&pairs)).await.unwrap();
    assert_eq!(
        shown(&rows)
            .iter()
            .map(|r| r[2].clone())
            .collect::<Vec<_>>(),
        ["text:first", "text:later", "text:other"]
    );
    let found = state
        .run(
            request(
                &state,
                &pairs,
                OperationKind::ReadRow,
                RowQuery {
                    row_id: rows.row_ids[1].clone(),
                    column_ids: vec!["col-2".into()],
                    raw_record: false,
                },
            ),
            OperationKind::ReadRow,
            |a, g, q, p, _| a.read_row(g, &q, p),
        )
        .await
        .unwrap();
    assert_eq!(show(&found.cells[0]), "text:later");
    for bad in ["[]", "not json", r#"["i:1"]"#] {
        let error = state
            .run(
                request(
                    &state,
                    &pairs,
                    OperationKind::ReadRow,
                    RowQuery {
                        row_id: bad.into(),
                        column_ids: vec!["col-2".into()],
                        raw_record: false,
                    },
                ),
                OperationKind::ReadRow,
                |a, g, q, p, _| a.read_row(g, &q, p),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidInput);
    }
    state.close(pairs.session_id).await.unwrap();
    // Every rowid alias is a real column: rows fall back to scan positions.
    let shadowed = open_object(&state, &path, "shadowed").await.unwrap();
    assert!(shadowed
        .object
        .warnings
        .iter()
        .any(|w| w.contains("hide the row key")));
    assert_eq!(
        shown(&try_page(&state, &shadowed, query(&shadowed)).await.unwrap())[0],
        ["text:r", "text:o", "text:u", "INTEGER:10"]
    );
    state.close(shadowed.session_id).await.unwrap();
    // Views: duplicate output names stay separate columns; exports use IDs.
    let dup = open_object(&state, &path, "dup").await.unwrap();
    assert_eq!(
        dup.object
            .columns
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["label", "label:1", "amount"]
    );
    assert!(dup
        .object
        .warnings
        .iter()
        .any(|w| w.contains("no stable row key")));
    let dest = dir.0.join("dup.jsonl");
    export_to(&state, &dup, &dest, ExportFormat::Jsonl, query(&dup))
        .await
        .unwrap();
    assert!(std::fs::read_to_string(&dest)
        .unwrap()
        .starts_with(r#"{"label":"Alice","label:1":"Alice","amount":1.5}"#));
    state.close(dup.session_id).await.unwrap();
    assert_eq!(
        std::fs::read(&path).unwrap(),
        original,
        "no read changed the database"
    );
    let wal: PathBuf = format!("{}-wal", path.display()).into();
    assert!(!wal.exists());
}

#[tokio::test]
async fn sqlite_wal_reads_are_live_schema_changes_are_stale_and_sidecars_are_reported() {
    let dir = Directory::new();
    let state = state(&dir);
    let path = dir.0.join("live.db");
    let writer = Writer::open(&path).unwrap();
    writer
        .execute_batch(
            "PRAGMA journal_mode=WAL; CREATE TABLE t(v INTEGER); INSERT INTO t VALUES (1), (2);",
        )
        .unwrap();
    let info = open_object(&state, &path, "t").await.unwrap();
    assert_eq!(info.sqlite.as_ref().unwrap().database.journal_mode, "wal");
    let count = |info: DatasetDescriptor| {
        let state = state.clone();
        async move {
            state
                .run(
                    request(&state, &info, OperationKind::Count, query(&info)),
                    OperationKind::Count,
                    |a, g, q, p, _| a.count(g, &q, p),
                )
                .await
                .unwrap()
                .count
        }
    };
    assert_eq!(count(info.clone()).await, CountState::known(2));
    writer.execute("INSERT INTO t VALUES (3)", []).unwrap();
    // Committed data is live: the cached count is dropped, not reported stale.
    assert_eq!(
        try_page(&state, &info, query(&info))
            .await
            .unwrap()
            .rows
            .len(),
        3
    );
    assert_eq!(count(info.clone()).await, CountState::known(3));
    writer.execute("CREATE TABLE extra(x)", []).unwrap();
    assert_eq!(
        try_page(&state, &info, query(&info))
            .await
            .unwrap_err()
            .code,
        ErrorCode::StaleDataset
    );
    let fresh = state.refresh(info.session_id.clone()).await.unwrap();
    assert_eq!(
        try_page(&state, &fresh, query(&fresh))
            .await
            .unwrap()
            .rows
            .len(),
        3
    );
    state.close(fresh.session_id).await.unwrap();
    drop(writer);

    // A cleanly closed WAL database has no sidecars; reading creates them.
    let main = std::fs::read(&path).unwrap();
    assert!(!PathBuf::from(format!("{}-shm", path.display())).exists());
    let info = open_object(&state, &path, "t").await.unwrap();
    assert!(info
        .object
        .warnings
        .iter()
        .any(|w| w.contains("created SQLite's -wal/-shm")));
    state.close(info.session_id).await.unwrap();
    assert_eq!(
        std::fs::read(&path).unwrap(),
        main,
        "main database bytes unchanged"
    );
    assert_eq!(
        std::fs::metadata(format!("{}-wal", path.display()))
            .unwrap()
            .len(),
        0
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let locked = dir.0.join("ro");
        std::fs::create_dir(&locked).unwrap();
        let copy = locked.join("live.db");
        std::fs::copy(&path, &copy).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
        // Privileged users can write anyway; the refusal applies otherwise.
        let enforced = std::fs::write(locked.join("probe"), b"").is_err();
        let result = database(&state, &copy).await;
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        if enforced {
            let error = result.unwrap_err();
            assert_eq!(error.code, ErrorCode::Io, "{}", error.message);
            assert!(error.message.contains("-wal/-shm"), "{}", error.message);
            assert!(!locked.join("live.db-shm").exists());
        }
    }

    // A writer holding an exclusive lock: the read waits briefly, then reports busy.
    let journal = dir.0.join("locked.db");
    let holder = Writer::open(&journal).unwrap();
    holder
        .execute_batch("CREATE TABLE t(v); INSERT INTO t VALUES (1);")
        .unwrap();
    let info = open_object(&state, &journal, "t").await.unwrap();
    holder
        .execute_batch("BEGIN EXCLUSIVE; INSERT INTO t VALUES (2);")
        .unwrap();
    let started = std::time::Instant::now();
    let error = try_page(&state, &info, query(&info)).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Busy, "{}", error.message);
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    holder.execute_batch("ROLLBACK").unwrap();
    assert_eq!(
        try_page(&state, &info, query(&info))
            .await
            .unwrap()
            .rows
            .len(),
        1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sqlite_authorizer_injection_names_cancellation_and_bad_files() {
    let dir = Directory::new();
    let state = state(&dir);
    let path = dir.0.join("tricky.db");
    let writer = Writer::open(&path).unwrap();
    writer.execute_batch(r#"
        CREATE TABLE "x""; DROP TABLE t; --" ("a""b" TEXT, " " INTEGER, "Ü" REAL);
        INSERT INTO "x""; DROP TABLE t; --" VALUES ('ok', 1, 2.5);
        CREATE TABLE t(v);
        CREATE VIEW forever AS WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r) SELECT x FROM r;
        CREATE VIEW loader AS SELECT load_extension('/nonexistent/evil') AS x;
    "#).unwrap();
    drop(writer);
    let before = std::fs::read(&path).unwrap();
    let info = open_object(&state, &path, "x\"; DROP TABLE t; --")
        .await
        .unwrap();
    assert_eq!(
        info.object
            .columns
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["a\"b", " ", "Ü"]
    );
    let mut q = query(&info);
    q.filter = predicate(
        "col-0",
        FilterOp::Contains,
        FilterValue::Text {
            value: "%'; DROP TABLE t; --".into(),
        },
    );
    assert!(try_page(&state, &info, q).await.unwrap().rows.is_empty());
    assert_eq!(
        shown(&try_page(&state, &info, query(&info)).await.unwrap())[0],
        ["text:ok", "INTEGER:1", "REAL:2.5"]
    );
    state.close(info.session_id).await.unwrap();

    // The connection itself refuses writes, attachment and unlisted pragmas.
    let conn = crate::adapters::sqlite::connect_for_test(&path).unwrap();
    for statement in [
        "INSERT INTO t VALUES (1)",
        "CREATE TEMP TABLE scratch(x)",
        "ATTACH 'other.db' AS other",
        "PRAGMA writable_schema = 1",
        "DELETE FROM t",
        "VACUUM",
    ] {
        assert!(conn.execute_batch(statement).is_err(), "{statement}");
    }
    assert_eq!(
        conn.query_row("SELECT count(*) FROM t", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(conn);

    // A view that tries to load native code is refused when read.
    let loader = open_object(&state, &path, "loader").await;
    match loader {
        Ok(info) => {
            let error = try_page(&state, &info, query(&info)).await.unwrap_err();
            assert!(
                error.message.contains("not authorized") || error.message.contains("unsafe use"),
                "{}",
                error.message
            );
            state.close(info.session_id).await.unwrap();
        }
        Err(error) => assert!(
            error.message.contains("not authorized") || error.message.contains("unsafe use"),
            "{}",
            error.message
        ),
    }
    let conn = crate::adapters::sqlite::connect_for_test(&path).unwrap();
    assert!(conn
        .query_row("SELECT load_extension('/nonexistent/evil')", [], |_| Ok(()))
        .is_err());
    drop(conn);

    // An endless view: an unsorted page must number every row, so it runs
    // until cancelled; the connection is usable afterwards.
    let info = open_object(&state, &path, "forever").await.unwrap();
    let req = request(&state, &info, OperationKind::Page, query(&info));
    let id = req.operation_id.clone();
    let worker = state.clone();
    let job = tokio::spawn(async move {
        worker
            .run(req, OperationKind::Page, |a, g, q, p, _| {
                a.fetch_page(g, &q, p)
            })
            .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let started = std::time::Instant::now();
    state.cancel_operation(&id).unwrap();
    let error = job.await.unwrap().unwrap_err();
    assert_eq!(error.code, ErrorCode::Cancelled, "{}", error.message);
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    let mut limited = query(&info);
    limited.filter = predicate(
        "col-0",
        FilterOp::Lte,
        FilterValue::Number { value: "3".into() },
    );
    limited.sort = Some(SortSpec {
        column_id: "col-0".into(),
        descending: false,
    });
    state.close(info.session_id).await.unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), before);

    let write = |name: &str, bytes: &[u8]| {
        let p = dir.0.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    };
    let mut corrupt = b"SQLite format 3\0".to_vec();
    corrupt.extend(vec![0xAB; 4096]);
    for (name, bytes, code) in [
        (
            "not.sqlite",
            b"plain text, not a database".to_vec(),
            ErrorCode::FormatMismatch,
        ),
        ("encrypted.db", vec![0x5A; 8192], ErrorCode::FormatMismatch),
        ("corrupt.sqlite", corrupt, ErrorCode::FormatMismatch),
    ] {
        assert_eq!(
            database(&state, &write(name, &bytes))
                .await
                .unwrap_err()
                .code,
            code,
            "{name}"
        );
    }
}
