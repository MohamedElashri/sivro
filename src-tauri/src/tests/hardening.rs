//! Hardening: all input families through one session API, repeated
//! open/close, interleaved and cancelled work, refresh, scratch quotas,
//! source-alias export guards, and the self-contained DuckDB engine.
use super::e2e_fixtures::{write_all, FILES};
use super::*;

pub(super) async fn open_any(
    state: &Arc<AppState>,
    path: &Path,
) -> Result<DatasetDescriptor, AppError> {
    let name = path.file_name().unwrap().to_str().unwrap();
    let options = if name.ends_with(".csv") || name.ends_with(".tsv") {
        let preview = state
            .preview_delimited(path.to_str().unwrap().into(), Default::default())
            .await?;
        OpenOptions {
            preview_token: Some(preview.token),
            ..Default::default()
        }
    } else if name.ends_with(".xlsx") {
        OpenOptions {
            excel: Some(ExcelOptions {
                sheet: state
                    .inspect_workbook(path.to_str().unwrap().into())
                    .await?
                    .sheets
                    .into_iter()
                    .find(|s| s.selectable)
                    .map(|s| s.name)
                    .unwrap_or_default(),
                header_row: Some(1),
            }),
            ..Default::default()
        }
    } else if name.ends_with(".h5") {
        OpenOptions {
            hdf5: Some(Hdf5Options {
                object: "/".into(),
                columns: true,
                ..Default::default()
            }),
            ..Default::default()
        }
    } else if name.ends_with(".sqlite") {
        OpenOptions {
            sqlite: Some(SqliteOptions {
                object: state
                    .inspect_database(path.to_str().unwrap().into())
                    .await?
                    .objects
                    .into_iter()
                    .find(|o| o.selectable)
                    .map(|o| o.name)
                    .unwrap_or_default(),
            }),
            ..Default::default()
        }
    } else {
        OpenOptions::default()
    };
    state.open(path.to_str().unwrap().into(), options).await
}
/// One cheap read per object: a page for tables, a node listing for documents.
async fn touch(state: &Arc<AppState>, info: &DatasetDescriptor) -> Result<(), AppError> {
    if info.document.is_some() {
        let q = DocumentQuery {
            path: vec![],
            offset: 0,
            limit: 10,
            mode: DocumentMode::Children,
        };
        state
            .run_adapter(
                request(state, info, OperationKind::Document, q),
                OperationKind::Document,
                |a, g, q, p, _| a.document()?.read(g, &q, p),
            )
            .await
            .map(|_| ())
    } else {
        state
            .run(
                request(state, info, OperationKind::Page, query(info)),
                OperationKind::Page,
                |a, g, q, p, _| a.fetch_page(g, &q, p),
            )
            .await
            .map(|_| ())
    }
}
/// Session directories under the live scratch run (the lease file excluded).
fn scratch_entries(dir: &Directory) -> usize {
    std::fs::read_dir(dir.0.join("scratch"))
        .unwrap()
        .flatten()
        .flat_map(|run| std::fs::read_dir(run.path()).unwrap().flatten())
        .filter(|e| e.file_name() != "lease")
        .count()
}

#[tokio::test]
async fn duckdb_engine_is_self_contained_and_never_installs_extensions() {
    let dir = Directory::new();
    let policy = ResourcePolicy::default();
    let root = crate::scratch::ScratchRoot::create(&dir.0.join("scratch"), "engine").unwrap();
    let scratch = crate::scratch::SessionScratch::create(root, "s", &policy).unwrap();
    let conn = crate::adapters::duckdb::connection(&scratch, &policy).unwrap();
    for setting in ["autoinstall_known_extensions", "autoload_known_extensions"] {
        let value: String = conn
            .query_row(
                &format!("SELECT current_setting('{setting}')::VARCHAR"),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(value, "false", "{setting}");
    }
    let mut stmt = conn
        .prepare("SELECT extension_name FROM duckdb_extensions() WHERE loaded AND install_mode = 'STATICALLY_LINKED' ORDER BY 1")
        .unwrap();
    let loaded: Vec<String> = stmt
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    for required in ["core_functions", "json", "parquet"] {
        assert!(
            loaded.iter().any(|e| e == required),
            "{required} missing from {loaded:?}"
        );
    }
    let remote = conn
        .query_row(
            "SELECT count(*) FROM read_csv('https://example.invalid/x.csv')",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_err()
        .to_string();
    assert!(remote.contains("httpfs"), "{remote}");
    let installed: i64 = conn
        .query_row("SELECT count(*) FROM duckdb_extensions() WHERE installed AND install_mode <> 'STATICALLY_LINKED'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        installed, 0,
        "no extension may be installed from disk or network"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_family_survives_repeated_open_close_and_releases_everything() {
    let dir = Directory::new();
    let state = state(&dir);
    let fixtures = dir.0.join("fixtures");
    write_all(&fixtures);
    for round in 0..20 {
        let mut opened = Vec::new();
        for file in FILES {
            let info = open_any(&state, &fixtures.join(file))
                .await
                .unwrap_or_else(|e| panic!("{file} round {round}: {e:?}"));
            touch(&state, &info).await.unwrap();
            opened.push(info);
        }
        for info in opened {
            assert!(state.close(info.session_id.clone()).await.unwrap().closed);
            assert!(
                !state.close(info.session_id).await.unwrap().closed,
                "close is idempotent"
            );
        }
        assert_eq!(
            scratch_entries(&dir),
            0,
            "round {round} left scratch behind"
        );
    }
    // All session slots came back: the full budget can be opened at once.
    let mut all = Vec::new();
    for i in 0..ResourcePolicy::default().max_sessions {
        all.push(
            open_any(&state, &fixtures.join(FILES[i % FILES.len()]))
                .await
                .unwrap(),
        );
    }
    assert_eq!(
        open_any(&state, &fixtures.join(FILES[0]))
            .await
            .unwrap_err()
            .code,
        ErrorCode::ResourceLimit
    );
    for info in all {
        state.close(info.session_id).await.unwrap();
    }
    assert_eq!(scratch_entries(&dir), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interleaved_and_cancelled_work_across_families_stays_consistent() {
    let dir = Directory::new();
    let state = state(&dir);
    let fixtures = dir.0.join("fixtures");
    write_all(&fixtures);
    let mut infos = Vec::new();
    for file in FILES {
        infos.push(open_any(&state, &fixtures.join(file)).await.unwrap());
    }
    for round in 0..6 {
        let mut jobs = Vec::new();
        for info in infos.iter().filter(|i| i.document.is_none()) {
            let (s, info) = (state.clone(), info.clone());
            let cancel = round % 2 == 1;
            jobs.push(tokio::spawn(async move {
                let page = request(&s, &info, OperationKind::Page, query(&info));
                let count = request(&s, &info, OperationKind::Count, query(&info));
                let profile = request(
                    &s,
                    &info,
                    OperationKind::Profile,
                    ProfileQuery {
                        column_id: "col-0".into(),
                        context: query(&info),
                    },
                );
                if cancel {
                    s.cancel_operation(&count.operation_id).unwrap();
                }
                let (a, b, c) = tokio::join!(
                    s.run(page, OperationKind::Page, |a, g, q, p, _| a
                        .fetch_page(g, &q, p)),
                    s.run(count, OperationKind::Count, |a, g, q, p, _| a
                        .count(g, &q, p)),
                    s.run(profile, OperationKind::Profile, |a, g, q, p, _| a
                        .profile(g, &q, p)),
                );
                assert!(a.is_ok(), "{:?}", a.err());
                match b {
                    Ok(count) => {
                        assert!(!cancel || matches!(count.count, CountState::Known { .. }))
                    }
                    Err(error) => {
                        assert!(cancel && error.code == ErrorCode::Cancelled, "{error:?}")
                    }
                }
                assert!(c.is_ok(), "{:?}", c.err());
            }));
        }
        // Keep each format interleaved without exceeding the reservation cap.
        for job in jobs {
            job.await.unwrap();
        }
    }
    for info in &infos {
        touch(&state, info).await.unwrap();
    }
    for info in infos {
        state.close(info.session_id).await.unwrap();
    }
    assert_eq!(scratch_entries(&dir), 0);
}

#[tokio::test]
#[cfg_attr(
    windows,
    ignore = "Windows refuses to replace a source while DuckDB holds it open"
)]
async fn every_family_reports_changed_sources_and_refreshes() {
    let dir = Directory::new();
    let state = state(&dir);
    let fixtures = dir.0.join("fixtures");
    write_all(&fixtures);
    for file in FILES {
        let path = fixtures.join(file);
        let info = open_any(&state, &path).await.unwrap();
        // Replace the file with fresh but equivalent content.
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let error = touch(&state, &info).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::StaleDataset, "{file}");
        let fresh = state
            .refresh(info.session_id.clone())
            .await
            .unwrap_or_else(|e| panic!("{file}: {e:?}"));
        assert_eq!(fresh.generation, 2, "{file}");
        touch(&state, &fresh).await.unwrap();
        let old = state.begin(
            info.session_id.clone(),
            info.generation,
            info.object.id.clone(),
            OperationKind::Page,
        );
        assert_eq!(
            old.unwrap_err().code,
            ErrorCode::InvalidGeneration,
            "{file}: old generation"
        );
        state.close(fresh.session_id).await.unwrap();
    }
}

#[tokio::test]
async fn exports_from_every_family_refuse_sources_aliases_and_existing_files() {
    let dir = Directory::new();
    let state = state(&dir);
    let fixtures = dir.0.join("fixtures");
    write_all(&fixtures);
    for file in FILES.iter().filter(|f| **f != "document.json") {
        let path = fixtures.join(file);
        let before = std::fs::read(&path).unwrap();
        let info = open_any(&state, &path).await.unwrap();
        let export = |destination: PathBuf, format: ExportFormat| {
            let (s, info) = (state.clone(), info.clone());
            async move {
                let options = ExportOptions {
                    destination: destination.to_str().unwrap().into(),
                    format,
                    max_rows: 100,
                    view: query(&info),
                };
                s.run(
                    request(&s, &info, OperationKind::Export, options),
                    OperationKind::Export,
                    |a, g, q, p, c| a.export(g, q, p, c),
                )
                .await
            }
        };
        let existing = dir.0.join(format!("{file}.existing.csv"));
        std::fs::write(&existing, b"keep").unwrap();
        #[cfg(unix)]
        let link = {
            let link = dir.0.join(format!("{file}.link.csv"));
            std::os::unix::fs::symlink(&path, &link).unwrap();
            Some(link)
        };
        #[cfg(not(unix))]
        let link: Option<PathBuf> = None;
        for destination in [Some(existing.clone()), link].into_iter().flatten() {
            let error = export(destination, ExportFormat::Csv).await.unwrap_err();
            assert_eq!(error.code, ErrorCode::Export, "{file}: {}", error.message);
        }
        // The source itself is refused (its own extension, so the format check passes).
        let own = match path.extension().unwrap().to_str().unwrap() {
            "h5" => ExportFormat::Hdf5,
            "arrow" => ExportFormat::Arrow,
            "arrows" => ExportFormat::ArrowStream,
            "parquet" => ExportFormat::Parquet,
            "csv" => ExportFormat::Csv,
            "tsv" => ExportFormat::Tsv,
            "jsonl" => ExportFormat::Jsonl,
            _ => ExportFormat::Csv,
        };
        if matches!(
            path.extension().unwrap().to_str().unwrap(),
            "parquet" | "h5" | "arrow" | "arrows" | "csv" | "tsv" | "jsonl"
        ) {
            assert_eq!(
                export(path.clone(), own).await.unwrap_err().code,
                ErrorCode::Export,
                "{file}"
            );
        }
        assert_eq!(std::fs::read(&existing).unwrap(), b"keep");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "{file} source changed"
        );
        let leftovers = std::fs::read_dir(&dir.0)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(".sivro-export"))
            .count();
        assert_eq!(leftovers, 0, "{file} left staging behind");
        state.close(info.session_id).await.unwrap();
    }
}

#[tokio::test]
async fn scratch_quota_bounds_staging_and_exports() {
    let dir = Directory::new();
    let policy = ResourcePolicy {
        scratch_bytes: 1 << 20,
        ..ResourcePolicy::default()
    };
    let state = AppState::new(dir.0.join("scratch"), policy).unwrap();
    // JSON Lines records are staged in scratch at about twice the source size.
    let path = dir.0.join("big.jsonl");
    let line = format!("{{\"text\":\"{}\"}}\n", "x".repeat(200));
    std::fs::write(&path, line.repeat(4_000)).unwrap();
    let error = open_any(&state, &path).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::ResourceLimit, "{}", error.message);
    assert!(error.message.contains("scratch"), "{}", error.message);
    // A Parquet export larger than the scratch budget stops and leaves nothing.
    let source = dir.0.join("wide.parquet");
    fixture(
        &source,
        "SELECT i, repeat('y', 500) AS pad FROM range(20000) t(i)",
    );
    let info = open_any(&state, &source).await.unwrap();
    let destination = dir.0.join("out.csv");
    let options = ExportOptions {
        destination: destination.to_str().unwrap().into(),
        format: ExportFormat::Csv,
        max_rows: 20_000,
        view: query(&info),
    };
    let error = state
        .run(
            request(&state, &info, OperationKind::Export, options),
            OperationKind::Export,
            |a, g, q, p, c| a.export(g, q, p, c),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ResourceLimit, "{}", error.message);
    assert!(!destination.exists());
    let leftovers = std::fs::read_dir(&dir.0)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(".sivro-export"))
        .count();
    assert_eq!(leftovers, 0);
    state.close(info.session_id).await.unwrap();
}
