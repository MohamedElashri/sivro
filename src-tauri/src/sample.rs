use std::path::PathBuf;

pub fn write_sample(destination: &str) -> Result<(), String> {
    use arrow_array::{ArrayRef, BooleanArray, Date32Array, Float64Array, Int32Array, StringArray};
    use arrow_schema::{DataType, Field, Schema};
    use parquet::arrow::ArrowWriter;

    let dest = PathBuf::from(destination.trim());
    if !dest
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("parquet") || e.eq_ignore_ascii_case("pq"))
    {
        return Err("Choose a destination ending in .parquet".to_string());
    }
    if let Some(parent) = dest.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("Cannot create folder: {e}"))?;
        }
    }

    let first = [
        "Aarav", "Mia", "Liam", "Zoe", "Kai", "Nina", "Omar", "Ivy", "Ravi", "Elena", "Theo",
        "Priya", "Jonas", "Sana", "Marco", "Tara",
    ];
    let last = [
        "Sharma", "Garcia", "Nguyen", "Kim", "Patel", "Muller", "Rossi", "Silva", "Khan", "Novak",
        "Tanaka", "Ali", "Smith", "Dubois", "Costa", "Iyer",
    ];
    let departments = [
        "Engineering",
        "Design",
        "Marketing",
        "Sales",
        "Support",
        "Research",
    ];

    // Deterministic LCG so the sample is stable across runs.
    let mut state: u64 = 0x9E3779B97F4A7C15;
    let mut next = |bound: u64| -> u64 {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (state >> 33) % bound
    };

    let n: usize = 1500;
    let mut ids = Vec::with_capacity(n);
    let mut names = Vec::with_capacity(n);
    let mut depts = Vec::with_capacity(n);
    let mut salaries = Vec::with_capacity(n);
    let mut ratings = Vec::with_capacity(n);
    let mut actives = Vec::with_capacity(n);
    let mut joined = Vec::with_capacity(n);
    for i in 0..n {
        ids.push(1000 + i as i32);
        names.push(format!(
            "{} {}",
            first[next(first.len() as u64) as usize],
            last[next(last.len() as u64) as usize]
        ));
        depts.push(departments[next(departments.len() as u64) as usize].to_string());
        salaries.push(42_000.0 + next(90_000) as f64 + next(100) as f64 / 100.0);
        ratings.push((next(50) as f64 / 10.0).clamp(0.0, 5.0));
        actives.push(next(100) < 82);
        joined.push(18_000 + next(2_200) as i32);
    }

    let schema = Schema::new(vec![
        Field::new("id", DataType::Int32, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("department", DataType::Utf8, false),
        Field::new("salary", DataType::Float64, false),
        Field::new("rating", DataType::Float64, false),
        Field::new("active", DataType::Boolean, false),
        Field::new("joined", DataType::Date32, false),
    ]);
    let batch = arrow_array::RecordBatch::try_new(
        std::sync::Arc::new(schema),
        vec![
            std::sync::Arc::new(Int32Array::from(ids)) as ArrayRef,
            std::sync::Arc::new(StringArray::from(names)) as ArrayRef,
            std::sync::Arc::new(StringArray::from(depts)) as ArrayRef,
            std::sync::Arc::new(Float64Array::from(salaries)) as ArrayRef,
            std::sync::Arc::new(Float64Array::from(ratings)) as ArrayRef,
            std::sync::Arc::new(BooleanArray::from(actives)) as ArrayRef,
            std::sync::Arc::new(Date32Array::from(joined)) as ArrayRef,
        ],
    )
    .map_err(|e| format!("Failed to build sample batch: {e}"))?;

    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&dest)
        .map_err(|e| format!("Cannot write sample file: {e}"))?;
    let mut writer = ArrowWriter::try_new(file, batch.schema(), None)
        .map_err(|e| format!("Failed to create Parquet writer: {e}"))?;
    writer
        .write(&batch)
        .map_err(|e| format!("Failed to write sample rows: {e}"))?;
    writer
        .close()
        .map_err(|e| format!("Failed to finish sample file: {e}"))?;

    Ok(())
}
