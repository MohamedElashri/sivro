use super::*;
use arrow_array::{
    builder::StringDictionaryBuilder, types::Int8Type, ArrayRef, BinaryArray, Decimal128Array,
    Float64Array, Int64Array, ListArray, RecordBatch, StringArray, TimestampNanosecondArray,
    UInt64Array,
};
use arrow_ipc::{
    reader::{FileReader, StreamReader},
    writer::{FileWriter, IpcWriteOptions, StreamWriter},
    CompressionType,
};
use arrow_schema::{DataType, Field, Schema};
use std::fs::File;

fn batch() -> RecordBatch {
    let mut dictionary = StringDictionaryBuilder::<Int8Type>::new();
    dictionary.append("Alice").unwrap();
    dictionary.append("Bob").unwrap();
    dictionary.append_null();
    let arrays: Vec<ArrayRef> = vec![
        Arc::new(Int64Array::from(vec![
            Some(9007199254740993),
            Some(-7),
            None,
        ])),
        Arc::new(UInt64Array::from(vec![u64::MAX, 0, 7])),
        Arc::new(
            Decimal128Array::from(vec![
                Some(12345678901234567890123456789012345678),
                Some(-1),
                None,
            ])
            .with_precision_and_scale(38, 8)
            .unwrap(),
        ),
        Arc::new(dictionary.finish()),
        Arc::new(BinaryArray::from(vec![
            Some(b"\x00\xff".as_slice()),
            Some(b"".as_slice()),
            None,
        ])),
        Arc::new(ListArray::from_iter_primitive::<
            arrow_array::types::Int64Type,
            _,
            _,
        >(vec![
            Some(vec![Some(9007199254740993), None]),
            Some(vec![]),
            None,
        ])),
        Arc::new(TimestampNanosecondArray::from(vec![
            Some(1700000000123456789),
            None,
            Some(-1),
        ])),
        Arc::new(Float64Array::from(vec![Some(1.5), Some(-0.25), None])),
    ];
    let names = [
        "big", "unsigned", "decimal", "name", "bytes", "nested", "time", "real",
    ];
    let fields = names
        .iter()
        .zip(&arrays)
        .map(|(n, a)| {
            Field::new(*n, a.data_type().clone(), true)
                .with_metadata(HashMap::from([("unit".into(), "test".into())]))
        })
        .collect::<Vec<_>>();
    let schema = Arc::new(Schema::new_with_metadata(
        fields,
        HashMap::from([("producer".into(), "Sivro tests".into())]),
    ));
    RecordBatch::try_new(schema, arrays).unwrap()
}
use std::collections::HashMap;

pub(super) fn write(
    path: &Path,
    batches: &[RecordBatch],
    schema: &Schema,
    stream: bool,
    compression: Option<CompressionType>,
) {
    let options = IpcWriteOptions::default()
        .try_with_compression(compression)
        .unwrap();
    let file = File::create(path).unwrap();
    if stream {
        let mut writer = StreamWriter::try_new_with_options(file, schema, options).unwrap();
        for b in batches {
            writer.write(b).unwrap();
        }
        writer.finish().unwrap();
    } else {
        let mut writer = FileWriter::try_new_with_options(file, schema, options).unwrap();
        for b in batches {
            writer.write(b).unwrap();
        }
        writer.finish().unwrap();
    }
}

fn read(path: &Path, stream: bool) -> Vec<RecordBatch> {
    let file = File::open(path).unwrap();
    if stream {
        StreamReader::try_new(file, None)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    } else {
        FileReader::try_new(file, None)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }
}

#[tokio::test]
async fn arrow_files_streams_aliases_compression_and_native_values() {
    let dir = Directory::new();
    let state = state(&dir);
    let batch = batch();
    for (suffix, stream) in [
        ("arrow", false),
        ("ipc", false),
        ("feather", false),
        ("arrows", true),
        ("ARROW", true),
    ] {
        for (i, compression) in [
            None,
            Some(CompressionType::LZ4_FRAME),
            Some(CompressionType::ZSTD),
        ]
        .into_iter()
        .enumerate()
        {
            let path = dir.0.join(format!("data-{i}.{suffix}"));
            write(
                &path,
                &[batch.clone(), batch.clone()],
                &batch.schema(),
                stream,
                compression,
            );
            let before = std::fs::read(&path).unwrap();
            let info = open(&state, &path).await;
            assert_eq!(info.format, FormatKind::Arrow);
            assert_eq!(info.object.row_count, CountState::known(6));
            let meta = info.arrow.as_ref().unwrap();
            assert_eq!(meta.encoding, if stream { "stream" } else { "file" });
            assert_eq!(meta.metadata["producer"], "Sivro tests");
            assert_eq!(meta.fields[0].metadata["unit"], "test");
            let mut q = query(&info);
            q.page_size = 2;
            let p = page(&state, &info, q.clone()).await;
            assert!(p.has_more);
            assert_eq!(text(&p.rows[0][0]), Some("9007199254740993"));
            assert_eq!(text(&p.rows[0][1]), Some("18446744073709551615"));
            assert_eq!(
                text(&p.rows[0][2]),
                Some("123456789012345678901234567890.12345678")
            );
            assert_eq!(text(&p.rows[0][3]), Some("Alice"));
            assert!(
                matches!(&p.rows[0][4], Cell::Binary { preview_hex, .. } if preview_hex == "00FF")
            );
            assert!(text(&p.rows[0][5]).unwrap().contains("9007199254740993"));
            assert!(text(&p.rows[0][6]).unwrap().ends_with(".123456789"));
            q.offset = 2;
            let p = page(&state, &info, q).await;
            assert!(matches!(p.rows[0][0], Cell::Null));
            assert_eq!(p.row_ids, ["2", "3"]);
            assert_eq!(std::fs::read(&path).unwrap(), before);
            state.close(info.session_id).await.unwrap();
        }
    }
}

#[tokio::test]
async fn arrow_queries_profiles_rows_and_all_export_roundtrips() {
    let dir = Directory::new();
    let state = state(&dir);
    let batch = batch();
    let path = dir.0.join("source.arrow");
    write(
        &path,
        std::slice::from_ref(&batch),
        &batch.schema(),
        false,
        None,
    );
    let info = open(&state, &path).await;
    let mut q = query(&info);
    q.column_ids = vec!["col-0".into(), "col-3".into()];
    q.filter = predicate(
        "col-0",
        FilterOp::Gt,
        FilterValue::Number { value: "0".into() },
    );
    q.quick_search = "Alice".into();
    q.sort = Some(SortSpec {
        column_id: "col-0".into(),
        descending: true,
    });
    let p = page(&state, &info, q.clone()).await;
    assert_eq!(p.rows.len(), 1);
    let count = state
        .run(
            request(&state, &info, OperationKind::Count, q.clone()),
            OperationKind::Count,
            |a, g, q, p, _| a.count(g, &q, p),
        )
        .await
        .unwrap();
    assert_eq!(count.count, CountState::known(1));
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
    assert_eq!(text(&profile.max), Some("9007199254740993"));
    let row = state
        .run(
            request(
                &state,
                &info,
                OperationKind::ReadRow,
                RowQuery {
                    row_id: p.row_ids[0].clone(),
                    column_ids: q.column_ids.clone(),
                    raw_record: false,
                },
            ),
            OperationKind::ReadRow,
            |a, g, q, p, _| a.read_row(g, &q, p),
        )
        .await
        .unwrap();
    assert_eq!(row.cells, p.rows[0]);
    for (format, suffix) in [
        (ExportFormat::Arrow, "arrow"),
        (ExportFormat::Arrow, "feather"),
        (ExportFormat::ArrowStream, "arrows"),
        (ExportFormat::Parquet, "parquet"),
        (ExportFormat::Csv, "csv"),
        (ExportFormat::Tsv, "tsv"),
        (ExportFormat::Json, "json"),
        (ExportFormat::Jsonl, "jsonl"),
    ] {
        let destination = dir.0.join(format!("out.{suffix}"));
        let result = text_export(&state, &info, &destination, format, q.clone())
            .await
            .unwrap();
        assert_eq!(result.exported_rows, "1");
        assert!(!result.truncated);
        if matches!(format, ExportFormat::Arrow | ExportFormat::ArrowStream) {
            let batches = read(&destination, format == ExportFormat::ArrowStream);
            assert_eq!(batches[0].schema().field(0).name(), "big");
            let reopened = open(&state, &destination).await;
            assert_eq!(page(&state, &reopened, query(&reopened)).await.rows, p.rows);
        }
    }
    // Full-schema native columnar export preserves nested, binary, decimals and ns timestamps.
    for (format, suffix) in [
        (ExportFormat::Arrow, "arrow"),
        (ExportFormat::ArrowStream, "arrows"),
    ] {
        let dest = dir.0.join(format!("full.{suffix}"));
        text_export(&state, &info, &dest, format, query(&info))
            .await
            .unwrap();
        let reopened = open(&state, &dest).await;
        assert_eq!(
            page(&state, &info, query(&info)).await.rows,
            page(&state, &reopened, query(&reopened)).await.rows
        );
    }
}

#[tokio::test]
async fn arrow_empty_batches_empty_tables_and_duplicate_names() {
    let dir = Directory::new();
    let state = state(&dir);
    let schema = Schema::new(vec![
        Field::new("", DataType::Int64, true),
        Field::new("same", DataType::Utf8, true),
        Field::new("same", DataType::Utf8, true),
    ]);
    for stream in [false, true] {
        let path = dir.0.join(if stream {
            "empty.arrows"
        } else {
            "empty.arrow"
        });
        write(&path, &[], &schema, stream, None);
        let info = open(&state, &path).await;
        assert_eq!(info.object.columns.len(), 3);
        assert_eq!(info.object.row_count, CountState::known(0));
        assert!(page(&state, &info, query(&info)).await.rows.is_empty());
        let out = dir.0.join(if stream { "out.arrows" } else { "out.arrow" });
        text_export(
            &state,
            &info,
            &out,
            if stream {
                ExportFormat::ArrowStream
            } else {
                ExportFormat::Arrow
            },
            query(&info),
        )
        .await
        .unwrap();
        let reopened = open(&state, &out).await;
        assert_eq!(reopened.object.row_count, CountState::known(0));
        assert_eq!(
            reopened
                .object
                .columns
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            ["col-0", "col-1", "col-2"]
        );
    }
    let source = dir.0.join("duplicates.arrow");
    let b = RecordBatch::try_new(
        Arc::new(schema),
        vec![
            Arc::new(Int64Array::from(vec![42])),
            Arc::new(StringArray::from(vec!["left"])),
            Arc::new(StringArray::from(vec!["right"])),
        ],
    )
    .unwrap();
    write(&source, std::slice::from_ref(&b), &b.schema(), false, None);
    let info = open(&state, &source).await;
    let p = page(&state, &info, query(&info)).await;
    assert_eq!(text(&p.rows[0][1]), Some("left"));
    assert_eq!(text(&p.rows[0][2]), Some("right"));
}

#[tokio::test]
async fn arrow_precision_fallbacks_are_explicit_and_exact() {
    use arrow_array::{Decimal256Array, Time64NanosecondArray};
    use duckdb::arrow::datatypes::i256;
    let dir = Directory::new();
    let state = state(&dir);
    let arrays: Vec<ArrayRef> = vec![
        Arc::new(
            Decimal256Array::from(vec!["7".repeat(76).parse::<i256>().unwrap()])
                .with_precision_and_scale(76, 2)
                .unwrap(),
        ),
        Arc::new(Time64NanosecondArray::from(vec![123456789])),
        Arc::new(TimestampNanosecondArray::from(vec![1700000000123456789]).with_timezone("UTC")),
        Arc::new(
            Decimal128Array::from(vec![123])
                .with_precision_and_scale(5, -2)
                .unwrap(),
        ),
    ];
    let schema = Arc::new(Schema::new(
        arrays
            .iter()
            .enumerate()
            .map(|(i, a)| Field::new(format!("f{i}"), a.data_type().clone(), true))
            .collect::<Vec<_>>(),
    ));
    let b = RecordBatch::try_new(schema, arrays).unwrap();
    let path = dir.0.join("wide.arrow");
    write(&path, std::slice::from_ref(&b), &b.schema(), false, None);
    let info = open(&state, &path).await;
    let p = page(&state, &info, query(&info)).await;
    assert_eq!(
        text(&p.rows[0][0]),
        Some(format!("{}.77", "7".repeat(74)).as_str())
    );
    assert!(text(&p.rows[0][1]).unwrap().contains("123456789"));
    assert!(text(&p.rows[0][2]).unwrap().contains("123456789"));
    assert_eq!(text(&p.rows[0][3]), Some("12300"));
    assert_eq!(
        info.object
            .warnings
            .iter()
            .filter(|w| w.contains("as text"))
            .count(),
        4
    );
}

#[tokio::test]
async fn arrow_malformed_inputs_resource_limits_and_refresh() {
    let dir = Directory::new();
    let state = state(&dir);
    let b = batch();
    let source = dir.0.join("good.arrow");
    write(&source, std::slice::from_ref(&b), &b.schema(), false, None);
    let bytes = std::fs::read(&source).unwrap();
    for (name, data) in [
        ("empty.arrow", vec![]),
        ("bad.arrow", b"not arrow data".to_vec()),
        ("old.feather", b"FEA1legacy feather".to_vec()),
        ("truncated.arrow", bytes[..bytes.len() - 1].to_vec()),
        ("fake.arrow", b"ARROW1\0\0garbageARROW1".to_vec()),
    ] {
        let path = dir.0.join(name);
        std::fs::write(&path, data).unwrap();
        assert!(
            state
                .open(path.to_str().unwrap().into(), OpenOptions::default())
                .await
                .is_err(),
            "{name}"
        );
    }
    let stream = dir.0.join("stream.arrows");
    write(&stream, std::slice::from_ref(&b), &b.schema(), true, None);
    let mut bad = std::fs::read(&stream).unwrap();
    bad.extend_from_slice(b"trailing");
    std::fs::write(&stream, bad).unwrap();
    assert_eq!(
        state
            .open(stream.to_str().unwrap().into(), OpenOptions::default())
            .await
            .unwrap_err()
            .code,
        ErrorCode::FormatMismatch
    );
    for policy in [
        ResourcePolicy {
            arrow_file_bytes: 32,
            ..Default::default()
        },
        ResourcePolicy {
            arrow_batch_bytes: 32,
            ..Default::default()
        },
        ResourcePolicy {
            max_columns: 2,
            ..Default::default()
        },
    ] {
        let limited = AppState::new(
            dir.0.join(format!(
                "limit-{}-{}-{}",
                policy.arrow_file_bytes, policy.arrow_batch_bytes, policy.max_columns
            )),
            policy,
        )
        .unwrap();
        assert_eq!(
            limited
                .open(source.to_str().unwrap().into(), OpenOptions::default())
                .await
                .unwrap_err()
                .code,
            ErrorCode::ResourceLimit
        );
    }
    let info = open(&state, &source).await;
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
    write(&source, std::slice::from_ref(&b), &b.schema(), false, None);
    let refreshed = state.refresh(info.session_id.clone()).await.unwrap();
    assert!(refreshed.generation > info.generation);
    assert_eq!(
        page(&state, &refreshed, query(&refreshed)).await.rows.len(),
        3
    );
}

#[tokio::test]
async fn arrow_exports_from_every_table_family_and_no_clobber() {
    let dir = Directory::new();
    let state = state(&dir);
    let fixtures = dir.0.join("fixtures");
    super::e2e_fixtures::write_all(&fixtures);
    for source in super::e2e_fixtures::FILES
        .iter()
        .filter(|s| **s != "document.json")
    {
        let info = super::hardening::open_any(&state, &fixtures.join(source))
            .await
            .unwrap();
        // JSON_NUMBER is deliberately rejected by typed exports. Select its exact integer/name fields.
        let mut q = query(&info);
        if *source == "records.jsonl" {
            q.column_ids = vec!["col-0".into(), "col-1".into()];
        }
        let expected = page(&state, &info, q.clone()).await.rows.len() as u64;
        for (format, suffix) in [
            (ExportFormat::Arrow, "arrow"),
            (ExportFormat::ArrowStream, "arrows"),
        ] {
            let dest = dir.0.join(format!("{source}.{suffix}"));
            text_export(&state, &info, &dest, format, q.clone())
                .await
                .unwrap();
            let reopened = open(&state, &dest).await;
            assert_eq!(reopened.object.row_count, CountState::known(expected));
            let before = std::fs::read(&dest).unwrap();
            assert!(text_export(&state, &info, &dest, format, q.clone())
                .await
                .is_err());
            assert_eq!(std::fs::read(&dest).unwrap(), before);
        }
    }
}

#[tokio::test]
async fn arrow_compressed_length_lies_and_bombs_fail_before_decoding() {
    let dir = Directory::new();
    let state = state(&dir);
    let array = Arc::new(StringArray::from(vec!["x".repeat(100000)])) as ArrayRef;
    let schema = Arc::new(Schema::new(vec![Field::new("large", DataType::Utf8, true)]));
    let b = RecordBatch::try_new(schema, vec![array]).unwrap();
    let source = dir.0.join("compressed.arrow");
    write(
        &source,
        std::slice::from_ref(&b),
        &b.schema(),
        false,
        Some(CompressionType::LZ4_FRAME),
    );
    let bytes = std::fs::read(&source).unwrap();
    let footer_length =
        i32::from_le_bytes(bytes[bytes.len() - 10..bytes.len() - 6].try_into().unwrap()) as usize;
    let footer =
        arrow_ipc::root_as_footer(&bytes[bytes.len() - 10 - footer_length..bytes.len() - 10])
            .unwrap();
    let block = footer.recordBatches().unwrap().get(0);
    let start = block.offset() as usize;
    let message_length =
        i32::from_le_bytes(bytes[start + 4..start + 8].try_into().unwrap()) as usize;
    let msg = arrow_ipc::root_as_message(&bytes[start + 8..start + 8 + message_length]).unwrap();
    let buffers = msg.header_as_record_batch().unwrap().buffers().unwrap();
    let data_buffer = buffers.get(buffers.len() - 1);
    let offset = start + block.metaDataLength() as usize + data_buffer.offset() as usize;
    assert!(i64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap()) > 0);
    for (name, length, code) in [
        ("lie", 1i64, ErrorCode::FormatMismatch),
        ("bomb", i64::MAX, ErrorCode::ResourceLimit),
    ] {
        let mut bad = bytes.clone();
        bad[offset..offset + 8].copy_from_slice(&length.to_le_bytes());
        let path = dir.0.join(format!("{name}.arrow"));
        std::fs::write(&path, bad).unwrap();
        assert_eq!(
            state
                .open(path.to_str().unwrap().into(), OpenOptions::default())
                .await
                .unwrap_err()
                .code,
            code
        );
    }
}

#[tokio::test]
async fn arrow_legacy_stream_framing_and_dictionary_updates() {
    let dir = Directory::new();
    let state = state(&dir);
    let path = dir.0.join("legacy.ipc");
    let schema = Arc::new(Schema::new(vec![Field::new(
        "name",
        DataType::Dictionary(Box::new(DataType::Int8), Box::new(DataType::Utf8)),
        true,
    )]));
    let opts = IpcWriteOptions::try_new(8, true, arrow_ipc::MetadataVersion::V4).unwrap();
    let mut writer =
        StreamWriter::try_new_with_options(File::create(&path).unwrap(), &schema, opts).unwrap();
    for value in ["Alice", "Bob"] {
        let mut dictionary = StringDictionaryBuilder::<Int8Type>::new();
        dictionary.append(value).unwrap();
        let batch =
            RecordBatch::try_new(schema.clone(), vec![Arc::new(dictionary.finish())]).unwrap();
        writer.write(&batch).unwrap();
    }
    writer.finish().unwrap();
    drop(writer);
    let info = open(&state, &path).await;
    let p = page(&state, &info, query(&info)).await;
    assert_eq!(text(&p.rows[0][0]), Some("Alice"));
    assert_eq!(text(&p.rows[1][0]), Some("Bob"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn arrow_export_cancellation_never_publishes_and_session_recovers() {
    let dir = Directory::new();
    let state = state(&dir);
    let source = dir.0.join("cancel.parquet");
    fixture(
        &source,
        "SELECT range AS id, repeat('x',1024) AS data FROM range(1000000)",
    );
    let info = open(&state, &source).await;
    for (format, suffix) in [
        (ExportFormat::Arrow, "arrow"),
        (ExportFormat::ArrowStream, "arrows"),
    ] {
        let dest = dir.0.join(format!("large.{suffix}"));
        let req = request(
            &state,
            &info,
            OperationKind::Export,
            ExportOptions {
                destination: dest.to_str().unwrap().into(),
                format,
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
        let error = tokio::time::timeout(Duration::from_secs(5), job)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Cancelled);
        assert!(!dest.exists());
        assert_eq!(page(&state, &info, query(&info)).await.rows.len(), 100);
        assert!(!std::fs::read_dir(&dir.0).unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".sivro-export-")));
    }
}

#[tokio::test]
async fn arrow_structs_maps_fixed_arrays_nulls_and_nonfinite_values() {
    let dir = Directory::new();
    let state = state(&dir);
    let conn = Connection::open_in_memory().unwrap();
    let mut stmt=conn.prepare("SELECT {'n': 9007199254740993::BIGINT, 's': 'a'} AS object, map(['key'],[9007199254740993::BIGINT]) AS mapping, [1,2]::INTEGER[2] AS fixed, 'NaN'::DOUBLE AS nan, 'Infinity'::DOUBLE AS inf").unwrap();
    let b = stmt.query_arrow([]).unwrap().next().unwrap();
    let source = dir.0.join("nested.arrow");
    write(&source, std::slice::from_ref(&b), &b.schema(), false, None);
    let info = open(&state, &source).await;
    let original = page(&state, &info, query(&info)).await;
    assert!(text(&original.rows[0][0])
        .unwrap()
        .contains("9007199254740993"));
    assert!(text(&original.rows[0][1])
        .unwrap()
        .contains("9007199254740993"));
    assert_eq!(text(&original.rows[0][2]), Some("[1,2]"));
    assert_eq!(text(&original.rows[0][3]), Some("nan"));
    for (format, suffix) in [
        (ExportFormat::Arrow, "arrow"),
        (ExportFormat::ArrowStream, "arrows"),
    ] {
        let dest = dir.0.join(format!("nested-out.{suffix}"));
        text_export(&state, &info, &dest, format, query(&info))
            .await
            .unwrap();
        let reopened = open(&state, &dest).await;
        assert_eq!(
            page(&state, &reopened, query(&reopened)).await.rows,
            original.rows
        );
    }
    let null = RecordBatch::try_from_iter(vec![(
        "null",
        Arc::new(arrow_array::NullArray::new(2)) as ArrayRef,
    )])
    .unwrap();
    let source = dir.0.join("null.arrow");
    write(
        &source,
        std::slice::from_ref(&null),
        &null.schema(),
        false,
        None,
    );
    let info = open(&state, &source).await;
    assert!(page(&state, &info, query(&info))
        .await
        .rows
        .iter()
        .all(|r| r[0] == Cell::Null));
}

#[tokio::test]
async fn arrow_run_end_union_list_view_and_duration_values_are_readable() {
    use arrow_array::{
        types::{Int16Type, Int64Type},
        DurationNanosecondArray, Int16Array, ListViewArray, RunArray, UnionArray,
    };
    let run = RunArray::<Int16Type>::try_new(
        &Int16Array::from(vec![2]),
        &Int64Array::from(vec![9007199254740993]),
    )
    .unwrap();
    let union = UnionArray::try_new(
        arrow_schema::UnionFields::try_new([0], [Field::new("int", DataType::Int64, true)])
            .unwrap(),
        vec![0i8, 0].into(),
        Some(vec![0i32, 1].into()),
        vec![Arc::new(Int64Array::from(vec![9007199254740993, 7]))],
    )
    .unwrap();
    let view = ListViewArray::from_iter_primitive::<Int64Type, _, _>(vec![
        Some(vec![Some(9007199254740993)]),
        None,
    ]);
    let arrays: Vec<ArrayRef> = vec![
        Arc::new(run),
        Arc::new(union),
        Arc::new(view),
        Arc::new(DurationNanosecondArray::from(vec![123456789, -1])),
    ];
    let schema = Arc::new(Schema::new(
        arrays
            .iter()
            .enumerate()
            .map(|(i, a)| Field::new(format!("f{i}"), a.data_type().clone(), true))
            .collect::<Vec<_>>(),
    ));
    let b = RecordBatch::try_new(schema, arrays).unwrap();
    let dir = Directory::new();
    let state = state(&dir);
    let source = dir.0.join("layouts.arrow");
    write(&source, std::slice::from_ref(&b), &b.schema(), false, None);
    let info = open(&state, &source).await;
    let p = page(&state, &info, query(&info)).await;
    for c in &p.rows[0][..3] {
        assert!(text(c).unwrap().contains("9007199254740993"));
    }
    assert!(text(&p.rows[0][3]).unwrap().contains("123456789"));
    assert!(matches!(p.rows[1][2], Cell::Null));
}

#[tokio::test]
async fn arrow_pyarrow_generated_feather_and_ipc_interoperate() {
    let dir = Directory::new();
    let state = state(&dir);
    for suffix in ["arrow", "arrows", "feather"] {
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../tests/fixtures/arrow/pyarrow.{suffix}"));
        let before = std::fs::read(&source).unwrap();
        let info = open(&state, &source).await;
        assert_eq!(info.arrow.as_ref().unwrap().metadata["producer"], "PyArrow");
        let p = page(&state, &info, query(&info)).await;
        assert_eq!(p.rows.len(), 2);
        assert_eq!(text(&p.rows[0][0]), Some("9007199254740993"));
        assert_eq!(text(&p.rows[0][1]), Some("18446744073709551615"));
        assert_eq!(
            text(&p.rows[0][2]),
            Some("123456789012345678901234567890.12345678")
        );
        assert_eq!(text(&p.rows[0][3]), Some("Alice"));
        assert!(text(&p.rows[0][5]).unwrap().contains("123456789"));
        assert!(matches!(p.rows[1][0], Cell::Null));
        assert!(matches!(p.rows[1][6],Cell::Binary{ref byte_count,..} if byte_count=="0"));
        let dest = dir.0.join(format!("out-{suffix}.arrow"));
        text_export(&state, &info, &dest, ExportFormat::Arrow, query(&info))
            .await
            .unwrap();
        let reopened = open(&state, &dest).await;
        assert_eq!(page(&state, &reopened, query(&reopened)).await.rows, p.rows);
        assert_eq!(std::fs::read(&source).unwrap(), before);
    }
}
