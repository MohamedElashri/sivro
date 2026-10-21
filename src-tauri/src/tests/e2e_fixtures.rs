//! Writes one small fixture per input family for the GUI end-to-end runner
//! (tests/e2e/run.mjs). Ignored in normal runs; the runner invokes it with
//! SIVRO_E2E_DIR set.
use super::*;

#[test]
#[ignore]
fn write_e2e_fixtures() {
    write_all(&PathBuf::from(
        std::env::var("SIVRO_E2E_DIR").expect("SIVRO_E2E_DIR"),
    ));
}

/// One file per input family; the GUI runner and hardening tests share them.
pub(super) const FILES: [&str; 7] = [
    "people.parquet",
    "people.csv",
    "people.tsv",
    "document.json",
    "records.jsonl",
    "workbook.xlsx",
    "people.sqlite",
];

pub(super) fn write_all(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    let big = "9007199254740993";
    fixture(
        &dir.join("people.parquet"),
        &format!("SELECT * FROM (VALUES (1, 'Alice', {big}::BIGINT), (2, 'Bob', 7::BIGINT)) t(id, name, big)"),
    );
    std::fs::write(
        dir.join("people.csv"),
        format!("id,name,code\n1,Alice,{big}\n2,Bob,00123\n"),
    )
    .unwrap();
    std::fs::write(
        dir.join("people.tsv"),
        format!("id\tname\tcode\n1\tAlice\t{big}\n2\tBob\t00123\n"),
    )
    .unwrap();
    std::fs::write(
        dir.join("document.json"),
        "{\"precise\": 0.123456789012345678901234567890, \"items\": [1, 2, 3]}",
    )
    .unwrap();
    std::fs::write(
        dir.join("records.jsonl"),
        format!("{{\"id\":1,\"name\":\"Alice\",\"big\":{big}}}\n{{\"id\":2,\"name\":\"Bob\",\"big\":1e400}}\n"),
    )
    .unwrap();
    std::fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/excel/workbook.xlsx"),
        dir.join("workbook.xlsx"),
    )
    .unwrap();
    let db = rusqlite::Connection::open(dir.join("people.sqlite")).unwrap();
    db.execute_batch(&format!(
        "CREATE TABLE people(id INTEGER PRIMARY KEY, name TEXT, big INTEGER, data BLOB);
         INSERT INTO people VALUES (1, 'Alice', {big}, x'00ff'), (2, 'Bob', 7, NULL);"
    ))
    .unwrap();
}
