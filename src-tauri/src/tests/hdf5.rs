use super::*;
use hdf5_pure::{AttrValue, FileBuilder};
fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../tests/fixtures/hdf5/{name}"))
}
fn selection(object: &str) -> Hdf5Options {
    Hdf5Options {
        object: object.into(),
        ..Default::default()
    }
}
async fn open_hdf(
    state: &Arc<AppState>,
    path: &Path,
    options: Hdf5Options,
) -> Result<DatasetDescriptor, AppError> {
    state
        .open(
            path.to_str().unwrap().into(),
            OpenOptions {
                hdf5: Some(options),
                ..Default::default()
            },
        )
        .await
}
fn index(info: &DatasetDescriptor, name: &str) -> usize {
    info.object
        .columns
        .iter()
        .position(|c| c.name == name)
        .unwrap()
}

#[tokio::test]
async fn hdf5_h5py_classic_modern_and_userblock_metadata_exact_records() {
    let dir = Directory::new();
    let state = state(&dir);
    for name in ["classic.h5", "modern.hdf5", "userblock.hdf", "types-v5.h5"] {
        let source = fixture_path(name);
        let before = std::fs::read(&source).unwrap();
        let root = state
            .inspect_hdf5(source.to_str().unwrap().into(), "/".into())
            .await
            .unwrap();
        assert!(root.attributes.iter().any(
            |a| a.name == "wide" && a.value.as_ref().unwrap().contains("18446744073709551615")
        ));
        assert!(root
            .objects
            .iter()
            .any(|o| o.path == "/arrays" && o.kind == "group"));
        if name != "classic.h5" {
            assert!(
                !root
                    .objects
                    .iter()
                    .find(|o| o.path == "/virtual")
                    .unwrap()
                    .selectable
            );
        }
        let info = open_hdf(&state, &source, selection("/records"))
            .await
            .unwrap();
        let p = page(&state, &info, query(&info)).await;
        assert_eq!(info.format, FormatKind::Hdf5);
        assert_eq!(p.rows.len(), 2);
        assert_eq!(
            text(&p.rows[0][index(&info, "id")]),
            Some("18446744073709551615")
        );
        assert_eq!(text(&p.rows[0][index(&info, "label")]), Some("Alice"));
        assert_eq!(text(&p.rows[1][index(&info, "label")]), Some(""));
        assert_eq!(
            text(&p.rows[0][index(&info, "samples")]),
            Some("[9007199254740993,2]")
        );
        assert_eq!(text(&p.rows[0][index(&info, "color")]), Some("1"));
        assert_eq!(std::fs::read(&source).unwrap(), before);
    }
}
#[tokio::test]
async fn hdf5_multidimensional_axes_slices_windows_and_filters() {
    let dir = Directory::new();
    let state = state(&dir);
    let path = fixture_path("modern.hdf5");
    for (row_axis, column_axis, fixed, rows, cols) in [
        (0, Some(1), vec!["0", "0", "2"], 2, 3),
        (1, Some(2), vec!["1", "0", "0"], 3, 4),
        (2, Some(0), vec!["0", "1", "0"], 4, 2),
        (2, None, vec!["1", "2", "0"], 4, 1),
    ] {
        let info = open_hdf(
            &state,
            &path,
            Hdf5Options {
                row_axis,
                column_axis,
                indices: fixed.into_iter().map(String::from).collect(),
                ..selection("/arrays/cube")
            },
        )
        .await
        .unwrap();
        let p = page(&state, &info, query(&info)).await;
        assert_eq!(p.rows.len(), rows);
        assert_eq!(p.rows[0].len(), cols);
        for (r, row) in p.rows.iter().enumerate() {
            for (c, cell) in row.iter().enumerate() {
                let mut coordinates = info
                    .hdf5
                    .as_ref()
                    .unwrap()
                    .options
                    .indices
                    .iter()
                    .map(|n| n.parse::<usize>().unwrap())
                    .collect::<Vec<_>>();
                coordinates[row_axis] = r;
                if let Some(axis) = column_axis {
                    coordinates[axis] = c;
                }
                let expected = 9007199254740993u64
                    + (coordinates[0] * 12 + coordinates[1] * 4 + coordinates[2]) as u64;
                assert_eq!(text(cell), Some(expected.to_string().as_str()));
            }
        }
    }
    let info = open_hdf(
        &state,
        &path,
        Hdf5Options {
            row_axis: 1,
            column_axis: Some(2),
            indices: vec!["1".into(), "0".into(), "0".into()],
            row_start: "1".into(),
            row_limit: Some(1),
            ..selection("/arrays/cube")
        },
    )
    .await
    .unwrap();
    assert_eq!(info.object.row_count, CountState::known(1));
    assert_eq!(
        text(&page(&state, &info, query(&info)).await.rows[0][0]),
        Some("9007199254741009")
    );
    let mut q = query(&info);
    q.filter = predicate(
        "col-0",
        FilterOp::Eq,
        FilterValue::Number {
            value: "9007199254741009".into(),
        },
    );
    q.sort = Some(SortSpec {
        column_id: "col-1".into(),
        descending: true,
    });
    let profile = state
        .run(
            request(
                &state,
                &info,
                OperationKind::Profile,
                ProfileQuery {
                    column_id: "col-0".into(),
                    context: q.clone(),
                },
            ),
            OperationKind::Profile,
            |a, g, q, p, _| a.profile(g, &q, p),
        )
        .await
        .unwrap();
    assert_eq!(text(&profile.min), Some("9007199254741009"));
    let count = state
        .run(
            request(&state, &info, OperationKind::Count, q.clone()),
            OperationKind::Count,
            |a, g, q, p, _| a.count(g, &q, p),
        )
        .await
        .unwrap();
    assert_eq!(count.count, CountState::known(1));
    let result = page(&state, &info, q.clone()).await;
    let row = state
        .run(
            request(
                &state,
                &info,
                OperationKind::ReadRow,
                RowQuery {
                    row_id: result.row_ids[0].clone(),
                    column_ids: q.column_ids,
                    raw_record: false,
                },
            ),
            OperationKind::ReadRow,
            |a, g, q, p, _| a.read_row(g, &q, p),
        )
        .await
        .unwrap();
    assert_eq!(row.cells, result.rows[0]);
}
#[tokio::test]
async fn hdf5_column_groups_scalar_empty_null_vlen_half_and_bytes() {
    let dir = Directory::new();
    let state = state(&dir);
    let path = fixture_path("classic.h5");
    let group = state
        .inspect_hdf5(path.to_str().unwrap().into(), "/rows".into())
        .await
        .unwrap();
    assert!(group.columns_selectable);
    let info = open_hdf(
        &state,
        &path,
        Hdf5Options {
            columns: true,
            ..selection("/rows")
        },
    )
    .await
    .unwrap();
    let p = page(&state, &info, query(&info)).await;
    assert_eq!(
        text(&p.rows[0][index(&info, "id")]),
        Some("9007199254740993")
    );
    assert!(matches!(
        p.rows[0][index(&info, "flag")],
        Cell::Boolean { value: true }
    ));
    assert!(
        matches!(&p.rows[0][index(&info,"blob")],Cell::Binary{preview_hex,..}if preview_hex=="00FF")
    );
    assert_eq!(text(&p.rows[1][index(&info, "name")]), Some("Émile ✓"));
    let scalar = open_hdf(&state, &path, selection("/scalar")).await.unwrap();
    assert_eq!(
        text(&page(&state, &scalar, query(&scalar)).await.rows[0][0]),
        Some("42")
    );
    for object in ["/empty", "/null"] {
        let info = open_hdf(&state, &path, selection(object)).await.unwrap();
        assert!(page(&state, &info, query(&info)).await.rows.is_empty());
    }
    let variable = open_hdf(&state, &path, selection("/variable"))
        .await
        .unwrap();
    let p = page(&state, &variable, query(&variable)).await;
    assert_eq!(text(&p.rows[0][0]), Some("[1,2,3]"));
    assert_eq!(text(&p.rows[1][0]), Some("[]"));
    let half = open_hdf(&state, &path, selection("/arrays/half"))
        .await
        .unwrap();
    let p = page(&state, &half, query(&half)).await;
    assert_eq!(text(&p.rows[2][0]), Some("1.5"));
    assert!(matches!(&p.rows[4][0],Cell::Number{value,..}if value.eq_ignore_ascii_case("nan")));
    let reference = open_hdf(&state, &path, selection("/references"))
        .await
        .unwrap();
    assert!(matches!(
        page(&state, &reference, query(&reference)).await.rows[0][0],
        Cell::Binary { .. }
    ));
}
#[tokio::test]
async fn hdf5_all_exports_and_exact_hdf5_roundtrip_nulls_labels_decimals_binary() {
    let dir = Directory::new();
    let state = state(&dir);
    let path = dir.0.join("source.parquet");
    fixture(&path,"SELECT * FROM (VALUES (9007199254740993::BIGINT,18446744073709551615::UBIGINT,12345678901234567890.123456789::DECIMAL(29,9),true,'',from_hex('00FF'),{'n':9007199254740993::BIGINT}), (NULL,NULL,NULL,false,NULL,NULL,NULL)) t(id,u,d,b,s,bytes,nested)");
    let original = open(&state, &path).await;
    let expected = page(&state, &original, query(&original)).await;
    let dest = dir.0.join("export.h5");
    text_export(
        &state,
        &original,
        &dest,
        ExportFormat::Hdf5,
        query(&original),
    )
    .await
    .unwrap();
    let reopened = open_hdf(
        &state,
        &dest,
        Hdf5Options {
            columns: true,
            ..selection("/table")
        },
    )
    .await
    .unwrap();
    let result = page(&state, &reopened, query(&reopened)).await;
    // Logical cell type names can change when integers are normalized, but exact representations cannot.
    assert_eq!(
        result
            .rows
            .iter()
            .map(|r| r.iter().map(crate::tests::text).collect::<Vec<_>>())
            .collect::<Vec<_>>(),
        expected
            .rows
            .iter()
            .map(|r| r.iter().map(crate::tests::text).collect::<Vec<_>>())
            .collect::<Vec<_>>()
    );
    assert!(matches!(&result.rows[0][5],Cell::Binary{preview_hex,..}if preview_hex=="00FF"));
    assert!(matches!(result.rows[1][4], Cell::Null));
    assert_eq!(text(&result.rows[0][4]), Some(""));
    for (format, suffix) in [
        (ExportFormat::Parquet, "parquet"),
        (ExportFormat::Csv, "csv"),
        (ExportFormat::Tsv, "tsv"),
        (ExportFormat::Json, "json"),
        (ExportFormat::Jsonl, "jsonl"),
        (ExportFormat::Hdf5, "hdf5"),
    ] {
        let destination = dir.0.join(format!("roundtrip.{suffix}"));
        text_export(&state, &reopened, &destination, format, query(&reopened))
            .await
            .unwrap();
        if format == ExportFormat::Json {
            let v: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&destination).unwrap()).unwrap();
            assert_eq!(v[0]["nested"]["n"].to_string(), "9007199254740993");
            assert_eq!(v[0]["bytes"]["$binary_hex"], "00FF");
        }
    }
    assert_eq!(
        text_export(
            &state,
            &original,
            &dest,
            ExportFormat::Hdf5,
            query(&original)
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::Export
    );
    // More than ten columns keep their projection order, even with ambiguous labels.
    let labels = dir.0.join("labels.csv");
    std::fs::write(
        &labels,
        format!(
            "{}\n{}\n",
            ["same"; 12].join(","),
            (0..12).map(|n| n.to_string()).collect::<Vec<_>>().join(",")
        ),
    )
    .unwrap();
    let info = super::hardening::open_any(&state, &labels).await.unwrap();
    let out = dir.0.join("labels.h5");
    text_export(&state, &info, &out, ExportFormat::Hdf5, query(&info))
        .await
        .unwrap();
    let back = open_hdf(
        &state,
        &out,
        Hdf5Options {
            columns: true,
            ..selection("/table")
        },
    )
    .await
    .unwrap();
    let p = page(&state, &back, query(&back)).await;
    for (i, c) in p.rows[0].iter().enumerate() {
        assert_eq!(text(c), Some(i.to_string().as_str()));
        assert_eq!(back.object.columns[i].name, format!("col-{i}"));
    }
}
#[tokio::test]
async fn hdf5_limits_bad_options_unsafe_storage_and_refresh() {
    let dir = Directory::new();
    let state = state(&dir);
    let source = dir.0.join("changing.h5");
    std::fs::copy(fixture_path("modern.hdf5"), &source).unwrap();
    for object in [
        "/external_storage",
        "/virtual",
        "/links/soft",
        "/links/external",
    ] {
        assert!(
            open_hdf(&state, &source, selection(object)).await.is_err(),
            "{object}"
        );
    }
    let links = state
        .inspect_hdf5(source.to_str().unwrap().into(), "/links".into())
        .await
        .unwrap();
    assert_eq!(links.objects.len(), 1);
    assert_eq!(links.objects[0].name, "self");
    for options in [
        selection("/../scalar"),
        Hdf5Options {
            row_axis: 5,
            ..selection("/arrays/cube")
        },
        Hdf5Options {
            column_axis: Some(0),
            ..selection("/arrays/cube")
        },
        Hdf5Options {
            indices: vec!["0".into(), "999".into(), "0".into()],
            ..selection("/arrays/cube")
        },
        Hdf5Options {
            row_start: "9".into(),
            ..selection("/scalar")
        },
    ] {
        assert!(open_hdf(&state, &source, options).await.is_err());
    }
    for policy in [
        ResourcePolicy {
            hdf5_file_bytes: 32,
            ..Default::default()
        },
        ResourcePolicy {
            hdf5_read_bytes: 1,
            ..Default::default()
        },
        ResourcePolicy {
            hdf5_cells: 1,
            ..Default::default()
        },
    ] {
        let limited = AppState::new(
            dir.0.join(format!(
                "limit-{}-{}-{}",
                policy.hdf5_file_bytes, policy.hdf5_read_bytes, policy.hdf5_cells
            )),
            policy,
        )
        .unwrap();
        assert_eq!(
            open_hdf(&limited, &source, selection("/records"))
                .await
                .unwrap_err()
                .code,
            ErrorCode::ResourceLimit
        );
    }
    let info = open_hdf(
        &state,
        &source,
        Hdf5Options {
            row_limit: Some(1),
            ..selection("/records")
        },
    )
    .await
    .unwrap();
    std::fs::write(&source, b"changed").unwrap();
    assert_eq!(
        state
            .run(
                request(&state, &info, OperationKind::Page, query(&info)),
                OperationKind::Page,
                |a, g, q, p, _| a.fetch_page(g, &q, p)
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::StaleDataset
    );
    std::fs::copy(fixture_path("modern.hdf5"), &source).unwrap();
    let refreshed = state.refresh(info.session_id).await.unwrap();
    assert_eq!(refreshed.hdf5.as_ref().unwrap().options.row_limit, Some(1));
    assert_eq!(
        page(&state, &refreshed, query(&refreshed)).await.rows.len(),
        1
    );
    let bad = dir.0.join("bad.h5");
    std::fs::write(&bad, b"not HDF5").unwrap();
    assert_eq!(
        open_hdf(&state, &bad, selection("/scalar"))
            .await
            .unwrap_err()
            .code,
        ErrorCode::FormatMismatch
    );
}

pub(super) fn write_fixture(path: &Path) {
    let mut b = FileBuilder::new();
    b.set_attr("producer", AttrValue::String("Sivro test".into()));
    b.create_dataset("id").with_i64_data(&[1, 2]);
    b.create_dataset("name")
        .with_vlen_strings(&["Alice", "Bob"]);
    b.create_dataset("big")
        .with_i64_data(&[9007199254740993, 7]);
    b.write(path).unwrap();
}
#[tokio::test]
async fn hdf5_v4_v5_all_chunk_indexes_and_userblocks_are_exact() {
    use hdf5_pure::ChunkIndex;
    let dir = Directory::new();
    let state = state(&dir);
    for filename in ["layout-v5.h5", "layout-v5-userblock.h5", "indexes-v4.h5"] {
        let source = fixture_path(filename);
        let before = std::fs::read(&source).unwrap();
        let low = hdf5_pure::File::from_bytes(before.clone()).unwrap();
        for group in ["filtered", "plain"] {
            let metadata = state
                .inspect_hdf5(source.to_str().unwrap().into(), format!("/{group}"))
                .await
                .unwrap();
            assert!(
                metadata.objects.iter().all(|o| o.selectable),
                "{filename}: {metadata:?}"
            );
            for (name, kind) in [
                ("single", ChunkIndex::SingleChunk),
                ("fixed", ChunkIndex::FixedArray),
                ("extensible", ChunkIndex::ExtensibleArray),
                ("extensible_inner", ChunkIndex::ExtensibleArray),
                ("btree", ChunkIndex::BTreeV2),
            ] {
                let path = format!("/{group}/{name}");
                let ds = low.dataset(&path).unwrap();
                assert_eq!(ds.chunk_index().unwrap(), Some(kind));
                assert_eq!(
                    ds.read_raw_rows(1, 2).unwrap(),
                    ds.read_raw().unwrap()[64..192]
                );
                assert!(!ds.chunks().unwrap().is_empty());
                let info = open_hdf(
                    &state,
                    &source,
                    Hdf5Options {
                        row_axis: 1,
                        column_axis: Some(0),
                        indices: vec!["0".into(), "0".into()],
                        row_start: "2".into(),
                        row_limit: Some(2),
                        ..selection(&path)
                    },
                )
                .await
                .unwrap();
                let p = page(&state, &info, query(&info)).await;
                assert_eq!(p.rows.len(), 2);
                for (r, row) in p.rows.iter().enumerate() {
                    for (c, cell) in row.iter().enumerate() {
                        assert_eq!(
                            text(cell),
                            Some(
                                (9007199254740993u64 + c as u64 * 8 + r as u64 + 2)
                                    .to_string()
                                    .as_str()
                            )
                        );
                    }
                }
                state.close(info.session_id).await.unwrap();
            }
        }
        for (path, kind, width, rows) in [
            ("/btree_deep", ChunkIndex::BTreeV2, 40, 40),
            ("/implicit", ChunkIndex::Implicit, 8, 6),
        ] {
            let ds = low.dataset(path).unwrap();
            assert_eq!(ds.chunk_index().unwrap(), Some(kind));
            let raw = ds.read_raw().unwrap();
            assert_eq!(raw.len(), width * rows * 8);
            assert_eq!(
                i64::from_le_bytes(raw[raw.len() - 8..].try_into().unwrap()),
                9007199254740993 + (width * rows - 1) as i64
            );
            assert_eq!(ds.read_raw_rows(1, 2).unwrap(), raw[width * 8..width * 24]);
            let info = open_hdf(
                &state,
                &source,
                Hdf5Options {
                    column_axis: Some(1),
                    row_start: "1".into(),
                    row_limit: Some(2),
                    ..selection(path)
                },
            )
            .await
            .unwrap();
            let p = page(&state, &info, query(&info)).await;
            assert_eq!(
                text(&p.rows[1][width - 1]),
                Some(
                    (9007199254740993u64 + 3 * width as u64 - 1)
                        .to_string()
                        .as_str()
                )
            );
            state.close(info.session_id).await.unwrap();
        }
        for (path, kind) in [
            ("/fixed_paged", ChunkIndex::FixedArray),
            ("/extensible_paged", ChunkIndex::ExtensibleArray),
        ] {
            let ds = low.dataset(path).unwrap();
            assert_eq!(ds.chunk_index().unwrap(), Some(kind));
            assert_eq!(ds.chunks().unwrap().len(), 10000);
            let raw = ds.read_raw_rows(9998, 2).unwrap();
            assert_eq!(
                i64::from_le_bytes(raw[8..].try_into().unwrap()),
                9007199254750992
            );
            let info = open_hdf(
                &state,
                &source,
                Hdf5Options {
                    row_start: "9998".into(),
                    row_limit: Some(2),
                    ..selection(path)
                },
            )
            .await
            .unwrap();
            assert_eq!(
                text(&page(&state, &info, query(&info)).await.rows[1][0]),
                Some("9007199254750992")
            );
            state.close(info.session_id).await.unwrap();
        }
        let info = open_hdf(
            &state,
            &source,
            Hdf5Options {
                column_axis: Some(1),
                ..selection("/unallocated")
            },
        )
        .await
        .unwrap();
        let p = page(&state, &info, query(&info)).await;
        assert!(p.rows.iter().flatten().all(|c| text(c) == Some("-7")));
        state.close(info.session_id).await.unwrap();
        if filename != "indexes-v4.h5" {
            let info = state
                .inspect_hdf5(source.to_str().unwrap().into(), "/".into())
                .await
                .unwrap();
            assert!(
                !info
                    .objects
                    .iter()
                    .find(|o| o.name == "huge_chunk")
                    .unwrap()
                    .selectable
            );
            assert_eq!(
                open_hdf(&state, &source, selection("/huge_chunk"))
                    .await
                    .unwrap_err()
                    .code,
                ErrorCode::ResourceLimit
            );
        }
        assert_eq!(std::fs::read(&source).unwrap(), before);
    }
}

#[tokio::test]
async fn hdf5_v5_corrupt_trees_and_chunks_fail_and_exports_stay_exact() {
    let dir = Directory::new();
    let state = state(&dir);
    let original = std::fs::read(fixture_path("layout-v5.h5")).unwrap();
    let low = hdf5_pure::File::from_bytes(original.clone()).unwrap();
    let chunk = low.dataset("/btree_deep").unwrap().chunks().unwrap()[0].address as usize;
    let internal = original
        .windows(6)
        .position(|w| w == b"BTIN\0\x0b")
        .unwrap();
    for (name, offset) in [("bad-tree.h5", internal + 6), ("bad-chunk.h5", chunk)] {
        let mut damaged = original.clone();
        damaged[offset] ^= 0x55;
        let path = dir.0.join(name);
        std::fs::write(&path, damaged).unwrap();
        assert_eq!(
            open_hdf(
                &state,
                &path,
                Hdf5Options {
                    column_axis: Some(1),
                    ..selection("/btree_deep")
                }
            )
            .await
            .unwrap_err()
            .code,
            ErrorCode::FormatMismatch
        );
    }
    let info = open_hdf(
        &state,
        &fixture_path("layout-v5.h5"),
        Hdf5Options {
            column_axis: Some(1),
            row_start: "1".into(),
            row_limit: Some(2),
            ..selection("/filtered/btree")
        },
    )
    .await
    .unwrap();
    let json = dir.0.join("v5.json");
    text_export(&state, &info, &json, ExportFormat::Json, query(&info))
        .await
        .unwrap();
    assert!(std::fs::read_to_string(json)
        .unwrap()
        .contains("9007199254741001"));
    let out = dir.0.join("v5.h5");
    text_export(&state, &info, &out, ExportFormat::Hdf5, query(&info))
        .await
        .unwrap();
    let restored = open_hdf(
        &state,
        &out,
        Hdf5Options {
            columns: true,
            ..selection("/table")
        },
    )
    .await
    .unwrap();
    assert_eq!(
        page(&state, &restored, query(&restored)).await.rows,
        page(&state, &info, query(&info)).await.rows
    );
}

#[tokio::test]
async fn arrow_hdf5_exports_preserve_exact_values_in_both_directions() {
    let dir = Directory::new();
    let state = state(&dir);
    let source = dir.0.join("cross-format.parquet");
    fixture(&source, "SELECT * FROM (VALUES (9007199254740993::BIGINT, 18446744073709551615::UBIGINT, 12345678901234567890.123456789::DECIMAL(29,9), true, 'a' || chr(0) || 'b', from_hex('00FF')), (NULL, NULL, NULL, false, '', NULL)) t(i,u,d,b,s,bytes)");
    let original = open(&state, &source).await;
    let expected = page(&state, &original, query(&original)).await.rows;
    for (format, suffix, next, next_suffix) in [
        (ExportFormat::Arrow, "arrow", ExportFormat::Hdf5, "h5"),
        (
            ExportFormat::ArrowStream,
            "arrows",
            ExportFormat::Hdf5,
            "h5",
        ),
        (ExportFormat::Hdf5, "h5", ExportFormat::Arrow, "arrow"),
    ] {
        let first = dir.0.join(format!("first.{suffix}"));
        text_export(&state, &original, &first, format, query(&original))
            .await
            .unwrap();
        let intermediate = if format == ExportFormat::Hdf5 {
            open_hdf(
                &state,
                &first,
                Hdf5Options {
                    columns: true,
                    ..selection("/table")
                },
            )
            .await
            .unwrap()
        } else {
            open(&state, &first).await
        };
        let final_path = dir.0.join(format!("via-{suffix}.{next_suffix}"));
        text_export(
            &state,
            &intermediate,
            &final_path,
            next,
            query(&intermediate),
        )
        .await
        .unwrap();
        let restored = if next == ExportFormat::Hdf5 {
            open_hdf(
                &state,
                &final_path,
                Hdf5Options {
                    columns: true,
                    ..selection("/table")
                },
            )
            .await
            .unwrap()
        } else {
            open(&state, &final_path).await
        };
        assert_eq!(
            page(&state, &restored, query(&restored)).await.rows,
            expected
        );
    }
}

#[tokio::test]
async fn hdf5_exports_every_table_family_and_checks_caps_sources_and_buffers() {
    let dir = Directory::new();
    let state = state(&dir);
    let fixtures = dir.0.join("fixtures");
    super::e2e_fixtures::write_all(&fixtures);
    for name in super::e2e_fixtures::FILES
        .iter()
        .filter(|f| **f != "document.json")
    {
        let info = super::hardening::open_any(&state, &fixtures.join(name))
            .await
            .unwrap();
        let mut q = query(&info);
        q.page_size = 10;
        let expected = page(&state, &info, q.clone()).await;
        let dest = dir.0.join(format!("{name}.h5"));
        text_export(&state, &info, &dest, ExportFormat::Hdf5, q.clone())
            .await
            .unwrap();
        let back = open_hdf(
            &state,
            &dest,
            Hdf5Options {
                columns: true,
                ..selection("/table")
            },
        )
        .await
        .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let actual = page(&state, &back, query(&back)).await;
        assert_eq!(
            actual
                .rows
                .iter()
                .map(|r| r.iter().map(text).collect::<Vec<_>>())
                .collect::<Vec<_>>(),
            expected
                .rows
                .iter()
                .map(|r| r.iter().map(text).collect::<Vec<_>>())
                .collect::<Vec<_>>(),
            "{name}"
        );
        let capped = dir.0.join(format!("{name}-cap.h5"));
        let result = state
            .run(
                request(
                    &state,
                    &info,
                    OperationKind::Export,
                    ExportOptions {
                        destination: capped.to_str().unwrap().into(),
                        format: ExportFormat::Hdf5,
                        max_rows: 1,
                        view: q,
                    },
                ),
                OperationKind::Export,
                |a, g, q, p, c| a.export(g, q, p, c),
            )
            .await
            .unwrap();
        assert!(result.truncated);
        assert_eq!(result.exported_rows, "1");
        state.close(back.session_id).await.unwrap();
        state.close(info.session_id).await.unwrap();
    }
}
#[tokio::test]
async fn hdf5_empty_nul_text_and_128bit_integers_roundtrip_without_rounding() {
    let dir = Directory::new();
    let state = state(&dir);
    let json = dir.0.join("exact.json");
    std::fs::write(&json,"[{\"text\":\"\\u0000abc\\u0000\",\"signed\":170141183460469231731687303715884105727,\"unsigned\":340282366920938463463374607431768211455}]").unwrap();
    let original = open(&state, &json).await;
    let expected = page(&state, &original, query(&original)).await;
    let out = dir.0.join("wide.h5");
    text_export(
        &state,
        &original,
        &out,
        ExportFormat::Hdf5,
        query(&original),
    )
    .await
    .unwrap();
    let back = open_hdf(
        &state,
        &out,
        Hdf5Options {
            columns: true,
            ..selection("/table")
        },
    )
    .await
    .unwrap();
    let actual = page(&state, &back, query(&back)).await;
    assert_eq!(actual.rows, expected.rows);
    let empty = dir.0.join("empty.parquet");
    fixture(&empty, "SELECT 1 AS id, true AS flag WHERE false");
    let info = open(&state, &empty).await;
    let dest = dir.0.join("empty.h5");
    text_export(&state, &info, &dest, ExportFormat::Hdf5, query(&info))
        .await
        .unwrap();
    let reopened = open_hdf(
        &state,
        &dest,
        Hdf5Options {
            columns: true,
            ..selection("/table")
        },
    )
    .await
    .unwrap();
    assert_eq!(reopened.object.columns.len(), 2);
    assert_eq!(reopened.object.row_count, CountState::known(0));
}
#[tokio::test]
async fn hdf5_annotations_cannot_inject_sql_or_silently_round_values() {
    let dir = Directory::new();
    let state = state(&dir);
    for (index, kind, value) in [
        (0, "DECIMAL(10,2)", "1.234"),
        (1, "TIMESTAMP_MS", "2024-01-01 00:00:00.1234"),
        (2, "JSON_NUMBER", "not a number"),
        (3, "BLOB", "GG"),
    ] {
        let mut b = FileBuilder::new();
        let mut g = b.create_group("table");
        g.set_attr("sivro.table", AttrValue::I64(1));
        g.create_dataset("x")
            .with_vlen_strings(&[value])
            .set_attr("sivro.type", AttrValue::String(kind.into()));
        b.add_group(g.finish());
        let path = dir.0.join(format!("bad-{index}.h5"));
        b.write(&path).unwrap();
        assert!(
            open_hdf(
                &state,
                &path,
                Hdf5Options {
                    columns: true,
                    ..selection("/table")
                }
            )
            .await
            .is_err(),
            "{kind}"
        );
    }
    let mut b = FileBuilder::new();
    let mut g = b.create_group("table");
    g.set_attr("sivro.table", AttrValue::I64(1));
    g.create_dataset("x").with_vlen_strings(&["keep"]).set_attr(
        "sivro.type",
        AttrValue::String("VARCHAR); DROP TABLE records; --".into()),
    );
    b.add_group(g.finish());
    let path = dir.0.join("injection.h5");
    b.write(&path).unwrap();
    let info = open_hdf(
        &state,
        &path,
        Hdf5Options {
            columns: true,
            ..selection("/table")
        },
    )
    .await
    .unwrap();
    assert_eq!(
        text(&page(&state, &info, query(&info)).await.rows[0][0]),
        Some("keep")
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hdf5_cancelled_export_never_publishes_and_session_recovers() {
    let dir = Directory::new();
    let state = state(&dir);
    let source = dir.0.join("cancel.parquet");
    fixture(
        &source,
        "SELECT range AS id, repeat('x',1024) AS data FROM range(1000000)",
    );
    let info = open(&state, &source).await;
    let dest = dir.0.join("cancel.h5");
    let req = request(
        &state,
        &info,
        OperationKind::Export,
        ExportOptions {
            destination: dest.to_str().unwrap().into(),
            format: ExportFormat::Hdf5,
            max_rows: 1000000,
            view: query(&info),
        },
    );
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
    state.cancel_operation(&id).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), job)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.unwrap_err().code, ErrorCode::Cancelled);
    assert!(!dest.exists());
    assert_eq!(page(&state, &info, query(&info)).await.rows.len(), 100);
    assert!(!std::fs::read_dir(&dir.0).unwrap().any(|e| e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".sivro-export-")));
}

#[tokio::test]
async fn hdf5_native_128bit_values_nested_array_shape_and_export_limits() {
    use hdf5_pure::{Datatype as Ty, DatatypeByteOrder as Order, FixedPointLayout};
    let dir = Directory::new();
    let state = state(&dir);
    let source = dir.0.join("native.h5");
    let mut b = FileBuilder::new();
    for (name, signed, bytes) in [
        ("signed", true, i128::MAX.to_le_bytes()),
        ("unsigned", false, u128::MAX.to_le_bytes()),
    ] {
        b.create_dataset(name).with_raw_data(
            Ty::FixedPoint {
                size: 16,
                byte_order: Order::LittleEndian,
                layout: FixedPointLayout {
                    signed,
                    bit_offset: 0,
                    bit_precision: 128,
                },
            },
            bytes.to_vec(),
            1,
        );
    }
    b.create_dataset("nested").with_raw_data(
        Ty::Array {
            base_type: Box::new(hdf5_pure::make_i64_type()),
            dimensions: vec![2, 2],
        },
        [1i64, 2, 3, 9007199254740993]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect(),
        1,
    );
    b.write(&source).unwrap();
    for (name, expected) in [
        ("signed", i128::MAX.to_string()),
        ("unsigned", u128::MAX.to_string()),
        ("nested", "[[1,2],[3,9007199254740993]]".into()),
    ] {
        let info = open_hdf(&state, &source, selection(&format!("/{name}")))
            .await
            .unwrap();
        assert_eq!(
            text(&page(&state, &info, query(&info)).await.rows[0][0]),
            Some(expected.as_str())
        );
        let out = dir.0.join(format!("{name}.h5"));
        text_export(&state, &info, &out, ExportFormat::Hdf5, query(&info))
            .await
            .unwrap();
        let back = open_hdf(
            &state,
            &out,
            Hdf5Options {
                columns: true,
                ..selection("/table")
            },
        )
        .await
        .unwrap();
        assert_eq!(
            text(&page(&state, &back, query(&back)).await.rows[0][0]),
            Some(expected.as_str())
        );
    }
    let limited = AppState::new(
        dir.0.join("limited"),
        ResourcePolicy {
            hdf5_read_bytes: 64,
            ..Default::default()
        },
    )
    .unwrap();
    let parquet = dir.0.join("source.parquet");
    fixture(&parquet, "SELECT range AS id FROM range(10)");
    let info = open(&limited, &parquet).await;
    let dest = dir.0.join("over-budget.h5");
    assert_eq!(
        text_export(&limited, &info, &dest, ExportFormat::Hdf5, query(&info))
            .await
            .unwrap_err()
            .code,
        ErrorCode::ResourceLimit
    );
    assert!(!dest.exists());
}

#[tokio::test]
#[ignore]
async fn write_hdf5_interop_export() {
    let dir = Directory::new();
    let state = state(&dir);
    let source = dir.0.join("interop.parquet");
    fixture(&source,"SELECT * FROM (VALUES (9007199254740993::BIGINT,18446744073709551615::UBIGINT,12345678901234567890.123456789::DECIMAL(29,9),true,chr(0)||'abc'||chr(0),from_hex('00FF')), (NULL,NULL,NULL,false,NULL,NULL)) t(id,u,d,b,s,bytes)");
    let info = open(&state, &source).await;
    let dest = PathBuf::from(std::env::var("SIVRO_HDF5_EXPORT").expect("SIVRO_HDF5_EXPORT"));
    text_export(&state, &info, &dest, ExportFormat::Hdf5, query(&info))
        .await
        .unwrap();
}
