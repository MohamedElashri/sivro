//! Excel workbooks through Calamine: one selected worksheet, cached formula
//! values only, never macros or recalculation. XLSX/XLSM/XLSB cells are
//! streamed with a bounding-box budget before anything grid-sized is
//! allocated; XLS is preflighted because Calamine parses all of it at open.
pub mod preflight;

use super::{
    duckdb::{connection, with_deadline, DuckDbAdapter},
    stage,
};
use crate::{
    api::*,
    dataset::*,
    errors::{AppError, AppResult, ErrorCode},
    policy::ResourcePolicy,
    query::duckdb as sql,
    registry::Fingerprint,
    scratch::SessionScratch,
};
use calamine::{
    CellErrorType, Data, DataRef, ExcelDateTime, Reader, SheetType, SheetVisible, Xls, Xlsb, Xlsx,
};
use serde_json::json;
use std::{
    collections::HashMap,
    fs::File,
    io::BufReader,
    panic::{catch_unwind, AssertUnwindSafe},
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

type Source = BufReader<File>;
enum Book {
    Xlsx(Xlsx<Source>),
    Xlsb(Xlsb<Source>),
    Xls(Xls<Source>),
}

fn variant(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn damaged(error: impl std::fmt::Display) -> AppError {
    AppError::new(
        ErrorCode::FormatMismatch,
        format!("This workbook is damaged, encrypted or not a supported Excel file: {error}"),
    )
}

/// Calamine indexes some tables without bounds checks; a malformed workbook
/// must become an error, not a crashed worker.
fn guarded<T>(work: impl FnOnce() -> AppResult<T>) -> AppResult<T> {
    catch_unwind(AssertUnwindSafe(work))
        .unwrap_or_else(|_| Err(damaged("the parser rejected an inconsistent structure")))
}

fn open_book(path: &Path, policy: &ResourcePolicy) -> AppResult<(Book, String)> {
    let variant = variant(path);
    let size = std::fs::metadata(path).map_err(AppError::io)?.len();
    if variant == "xls" {
        preflight::xls(path, size, policy)?;
    } else {
        preflight::zip(path, size, policy)?;
    }
    let reader = || File::open(path).map(BufReader::new).map_err(AppError::io);
    let book = guarded(|| {
        Ok(match variant.as_str() {
            "xls" => Book::Xls(Xls::new(reader()?).map_err(damaged)?),
            "xlsb" => Book::Xlsb(Xlsb::new(reader()?).map_err(damaged)?),
            _ => Book::Xlsx(Xlsx::new(reader()?).map_err(damaged)?),
        })
    })?;
    Ok((book, variant))
}

impl Book {
    fn sheets(&self) -> &[calamine::Sheet] {
        match self {
            Self::Xlsx(b) => b.sheets_metadata(),
            Self::Xlsb(b) => b.sheets_metadata(),
            Self::Xls(b) => b.sheets_metadata(),
        }
    }
    fn date1904(&self) -> bool {
        match self {
            Self::Xlsx(b) => b.has_1904_epoch(),
            Self::Xlsb(b) => b.has_1904_epoch(),
            Self::Xls(b) => b.has_1904_epoch(),
        }
    }
    fn info(&self, variant: &str) -> WorkbookInfo {
        let sheets = self
            .sheets()
            .iter()
            .map(|s| {
                let kind = match s.typ {
                    SheetType::WorkSheet => SheetKind::Worksheet,
                    SheetType::ChartSheet => SheetKind::Chartsheet,
                    SheetType::DialogSheet => SheetKind::Dialogsheet,
                    SheetType::MacroSheet => SheetKind::Macrosheet,
                    _ => SheetKind::Other,
                };
                SheetInfo {
                    name: s.name.clone(),
                    kind,
                    visibility: match s.visible {
                        SheetVisible::Visible => SheetVisibility::Visible,
                        SheetVisible::Hidden => SheetVisibility::Hidden,
                        SheetVisible::VeryHidden => SheetVisibility::VeryHidden,
                    },
                    selectable: kind == SheetKind::Worksheet,
                }
            })
            .collect();
        let mut warnings = vec!["Formulas are never recalculated and macros are never run; Sivro reads only cached cell values.".into()];
        if variant == "xls" {
            warnings.push(".xls workbooks are parsed in full when opened, so the cell limit applies to all sheets together.".into());
        }
        WorkbookInfo {
            variant: variant.into(),
            date1904: self.date1904(),
            sheets,
            warnings,
        }
    }
}

pub fn inspect(path: &Path, policy: &ResourcePolicy) -> AppResult<WorkbookInfo> {
    let fingerprint = Fingerprint::read(path)?;
    let (book, variant) = open_book(path, policy)?;
    let info = book.info(&variant);
    fingerprint.check(path)?;
    Ok(info)
}

// ---------------------------------------------------------------------------
// Cell values
// ---------------------------------------------------------------------------

/// A source cell after conversion; `Special` is already tagged JSON text.
#[derive(Debug, Clone, PartialEq)]
enum Val {
    Int(i64),
    Float(f64),
    Text(String),
    Bool(bool),
    Date(String),
    Timestamp(String),
    Time(String),
    Special(String),
}

enum Raw<'a> {
    Int(i64),
    Float(f64),
    Str(&'a str),
    Bool(bool),
    DateTime(&'a ExcelDateTime),
    Error(&'a CellErrorType),
    Empty,
}
impl<'a> From<&'a DataRef<'a>> for Raw<'a> {
    fn from(v: &'a DataRef<'a>) -> Self {
        match v {
            DataRef::Int(i) => Raw::Int(*i),
            DataRef::Float(f) => Raw::Float(*f),
            DataRef::String(s) | DataRef::DateTimeIso(s) | DataRef::DurationIso(s) => Raw::Str(s),
            DataRef::SharedString(s) => Raw::Str(s),
            DataRef::Bool(b) => Raw::Bool(*b),
            DataRef::DateTime(d) => Raw::DateTime(d),
            DataRef::Error(e) => Raw::Error(e),
            DataRef::Empty => Raw::Empty,
        }
    }
}
impl<'a> From<&'a Data> for Raw<'a> {
    fn from(v: &'a Data) -> Self {
        match v {
            Data::Int(i) => Raw::Int(*i),
            Data::Float(f) => Raw::Float(*f),
            Data::String(s) | Data::DateTimeIso(s) | Data::DurationIso(s) => Raw::Str(s),
            Data::Bool(b) => Raw::Bool(*b),
            Data::DateTime(d) => Raw::DateTime(d),
            Data::Error(e) => Raw::Error(e),
            Data::Empty => Raw::Empty,
        }
    }
}

/// Largest integer every f64 below it represents exactly.
const EXACT_INT: f64 = 9_007_199_254_740_992.0;

fn convert(raw: Raw<'_>, formula: Option<&str>, date1904: bool) -> Option<Val> {
    Some(match raw {
        Raw::Int(i) => Val::Int(i),
        Raw::Float(f) if !f.is_finite() => {
            Val::Special(json!({"$excel_number": f.to_string()}).to_string())
        }
        Raw::Float(f) if f.fract() == 0.0 && f.abs() <= EXACT_INT => Val::Int(f as i64),
        Raw::Float(f) => Val::Float(f),
        Raw::Str(s) => Val::Text(s.to_owned()),
        Raw::Bool(b) => Val::Bool(b),
        Raw::DateTime(d) => excel_datetime(d.as_f64(), d.is_duration(), date1904),
        Raw::Error(e) => Val::Special(json!({"$excel_error": e.to_string()}).to_string()),
        Raw::Empty => {
            let f = formula?;
            Val::Special(json!({"$excel_formula": f, "$no_cached_value": true}).to_string())
        }
    })
}

/// Days since 1970-01-01 for a proleptic Gregorian date (H. Hinnant).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// Excel serials: whole days from the 1900 or 1904 epoch plus a day fraction,
/// rounded to Excel's millisecond resolution. The 1900 system counts a
/// nonexistent 1900-02-29 (serial 60), which is reported, not shifted.
fn excel_datetime(serial: f64, duration: bool, date1904: bool) -> Val {
    if duration {
        return Val::Special(json!({"$excel_duration_days": serial}).to_string());
    }
    if !(0.0..2_958_466.0).contains(&serial) {
        return Val::Special(json!({"$excel_serial": serial, "$invalid_date": true}).to_string());
    }
    let ms = (serial * 86_400_000.0).round() as i64;
    let (days, of_day) = (ms.div_euclid(86_400_000), ms.rem_euclid(86_400_000));
    let time = format!(
        "{:02}:{:02}:{:02}.{:03}",
        of_day / 3_600_000,
        of_day / 60_000 % 60,
        of_day / 1000 % 60,
        of_day % 1000
    );
    if days == 0 {
        // A time of day without a date (time-only number formats).
        return Val::Time(time);
    }
    let day = if date1904 {
        days_from_civil(1904, 1, 1) + days
    } else if days == 60 {
        return Val::Special(
            json!({"$excel_serial": serial, "$invalid_date": "1900-02-29"}).to_string(),
        );
    } else {
        days_from_civil(1899, 12, 31) + days - i64::from(days > 60)
    };
    let (y, m, d) = civil_from_days(day);
    let date = format!("{y:04}-{m:02}-{d:02}");
    if of_day == 0 {
        Val::Date(date)
    } else {
        Val::Timestamp(format!("{date} {time}"))
    }
}

fn label(val: &Val) -> String {
    match val {
        Val::Int(i) => i.to_string(),
        Val::Float(f) => f.to_string(),
        Val::Text(s) | Val::Date(s) | Val::Timestamp(s) | Val::Time(s) | Val::Special(s) => {
            s.clone()
        }
        Val::Bool(b) => b.to_string(),
    }
}

pub fn column_letters(mut col: u32) -> String {
    let mut out = Vec::new();
    loop {
        out.push(b'A' + (col % 26) as u8);
        if col < 26 {
            break;
        }
        col = col / 26 - 1;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Reading one worksheet
// ---------------------------------------------------------------------------

struct Collector<'a> {
    cells: Vec<(u32, u32, Val)>,
    min: (u32, u32),
    max: (u32, u32),
    text: usize,
    policy: &'a ResourcePolicy,
    started: Instant,
}
impl Collector<'_> {
    /// Rejects the sheet as soon as its used bounding box exceeds the budget,
    /// before any grid-sized structure exists.
    fn push(&mut self, (row, col): (u32, u32), val: Val) -> AppResult<()> {
        // Readers report raw positions (XLSB columns are unchecked u32s);
        // anything outside Excel's grid is a damaged file, not a large sheet.
        if row > 1_048_575 || col > 16_383 {
            return Err(damaged(format!(
                "cell position row {}, column {} is outside the Excel grid",
                row as u64 + 1,
                col as u64 + 1
            )));
        }
        if self.cells.is_empty() {
            self.min = (row, col);
        }
        self.min = (self.min.0.min(row), self.min.1.min(col));
        self.max = (self.max.0.max(row), self.max.1.max(col));
        let area = (self.max.0 as u64 - self.min.0 as u64 + 1)
            * (self.max.1 as u64 - self.min.1 as u64 + 1);
        if area > self.policy.excel_cells as u64 {
            return Err(AppError::limit(format!(
                "The used range reaches {}{}; that is more than the {} cells allowed for one sheet",
                column_letters(self.max.1),
                self.max.0 + 1,
                self.policy.excel_cells
            )));
        }
        if let Val::Text(s) | Val::Special(s) = &val {
            self.text += s.len();
            if self.text as u64 > self.policy.excel_entry_bytes {
                return Err(AppError::limit(
                    "The sheet's text exceeds the workbook budget",
                ));
            }
        }
        self.cells.push((row, col, val));
        if self.cells.len().is_multiple_of(4096)
            && self.started.elapsed() > Duration::from_millis(self.policy.timeout_ms)
        {
            return Err(AppError::new(
                ErrorCode::Timeout,
                "Reading this worksheet exceeded the open time budget",
            ));
        }
        Ok(())
    }
}

/// Non-empty cells as (row, column, value) in stream order.
type SheetCells = Vec<(u32, u32, Val)>;

fn read_sheet(
    book: &mut Book,
    sheet: &str,
    policy: &ResourcePolicy,
) -> AppResult<(SheetCells, usize)> {
    let date1904 = book.date1904();
    let mut out = Collector {
        cells: Vec::new(),
        min: (0, 0),
        max: (0, 0),
        text: 0,
        policy,
        started: Instant::now(),
    };
    let mut no_cache = 0usize;
    guarded(|| {
        match book {
            Book::Xlsx(b) => {
                let mut reader = b.worksheet_cells_reader(sheet).map_err(damaged)?;
                while let Some(cell) = reader.next_cell_with_formula().map_err(damaged)? {
                    let formula = cell.formula.as_deref().filter(|f| !f.is_empty());
                    if let Some(val) = convert(Raw::from(&cell.value), formula, date1904) {
                        no_cache += usize::from(matches!(cell.value, DataRef::Empty));
                        out.push(cell.pos, val)?;
                    }
                }
            }
            Book::Xlsb(b) => {
                let mut reader = b.worksheet_cells_reader(sheet).map_err(damaged)?;
                while let Some(cell) = reader.next_cell().map_err(damaged)? {
                    if let Some(val) = convert(Raw::from(cell.get_value()), None, date1904) {
                        out.push(cell.get_position(), val)?;
                    }
                }
            }
            Book::Xls(b) => {
                // Already parsed and bounded by the XLS preflight.
                let range = b.worksheet_range(sheet).map_err(damaged)?;
                let (r0, c0) = range.start().unwrap_or((0, 0));
                for (r, c, value) in range.used_cells() {
                    if let Some(val) = convert(Raw::from(value), None, date1904) {
                        out.push((r0 + r as u32, c0 + c as u32), val)?;
                    }
                }
            }
        }
        Ok(())
    })?;
    Ok((out.cells, no_cache))
}

#[derive(Default)]
struct Kinds {
    int: bool,
    big: bool,
    float: bool,
    text: bool,
    bool: bool,
    date: bool,
    timestamp: bool,
    time: bool,
    special: bool,
}
impl Kinds {
    fn observe(&mut self, val: &Val) {
        match val {
            Val::Int(i) => {
                self.int = true;
                self.big |= i.unsigned_abs() > EXACT_INT as u64;
            }
            Val::Float(_) => self.float = true,
            Val::Text(_) => self.text = true,
            Val::Bool(_) => self.bool = true,
            Val::Date(_) => self.date = true,
            Val::Timestamp(_) => self.timestamp = true,
            Val::Time(_) => self.time = true,
            Val::Special(_) => self.special = true,
        }
    }
    /// One native type per column; anything mixed or special keeps tagged
    /// JSON text instead of being coerced.
    fn data_type(&self) -> String {
        let groups = [
            self.int || self.float,
            self.text,
            self.bool,
            self.date || self.timestamp,
            self.time,
        ]
        .iter()
        .filter(|g| **g)
        .count();
        if self.special || groups > 1 || (self.float && self.big) {
            sql::JSON_VALUE
        } else if self.float {
            "DOUBLE"
        } else if self.int {
            "BIGINT"
        } else if self.bool {
            "BOOLEAN"
        } else if self.timestamp {
            "TIMESTAMP"
        } else if self.date {
            "DATE"
        } else if self.time {
            "TIME"
        } else {
            "VARCHAR"
        }
        .into()
    }
}

fn text_for(val: &Val, data_type: &str) -> String {
    if data_type != sql::JSON_VALUE {
        return label(val);
    }
    match val {
        Val::Int(i) => i.to_string(),
        Val::Float(f) => f.to_string(),
        Val::Text(s) => serde_json::Value::from(s.as_str()).to_string(),
        Val::Bool(b) => b.to_string(),
        Val::Date(d) => json!({ "$excel_date": d }).to_string(),
        Val::Timestamp(t) => json!({ "$excel_datetime": t }).to_string(),
        Val::Time(t) => json!({ "$excel_time": t }).to_string(),
        Val::Special(s) => s.clone(),
    }
}

pub fn open(
    path: &Path,
    options: &ExcelOptions,
    object_id: String,
    scratch: SessionScratch,
    policy: &ResourcePolicy,
) -> AppResult<DuckDbAdapter> {
    let fingerprint = Arc::new(Fingerprint::read(path)?);
    let (mut book, variant) = open_book(path, policy)?;
    let workbook = book.info(&variant);
    let sheet = workbook
        .sheets
        .iter()
        .find(|s| s.name == options.sheet)
        .ok_or_else(|| {
            AppError::new(
                ErrorCode::InvalidObject,
                "That sheet is not in this workbook. Choose a sheet again.",
            )
        })?;
    if !sheet.selectable {
        return Err(AppError::new(
            ErrorCode::InvalidObject,
            "Only worksheets can be opened; chart, dialog and macro sheets have no cell table",
        ));
    }
    let hidden = sheet.visibility != SheetVisibility::Visible;
    let (cells, no_cache) = read_sheet(&mut book, &options.sheet, policy)?;
    drop(book);
    if cells.is_empty() {
        return Err(AppError::invalid("This worksheet has no cells"));
    }
    let header = match options.header_row {
        Some(0) => return Err(AppError::invalid("Header rows are numbered from 1")),
        Some(row) => Some(row - 1),
        None => None,
    };
    let (above, cells): (Vec<_>, Vec<_>) = cells
        .into_iter()
        .partition(|(row, _, _)| header.is_some_and(|h| *row < h));
    if cells.is_empty() {
        return Err(AppError::invalid(
            "No cells are at or below the chosen header row",
        ));
    }
    let first_col = cells.iter().map(|c| c.1).min().unwrap_or(0);
    let last_col = cells.iter().map(|c| c.1).max().unwrap_or(0);
    let width = (last_col - first_col + 1) as usize;
    if width > policy.max_columns {
        return Err(AppError::limit(
            "The worksheet is wider than the column budget",
        ));
    }
    let first_row = header.map_or_else(|| cells.iter().map(|c| c.0).min().unwrap_or(0), |h| h + 1);
    let last_row = cells.iter().map(|c| c.0).max().unwrap_or(0);
    let rows = if last_row >= first_row {
        (last_row - first_row + 1) as usize
    } else {
        0
    };
    let mut labels: Vec<String> = (first_col..=last_col).map(column_letters).collect();
    if header.is_some() {
        labels.iter_mut().for_each(String::clear);
    }
    // The used bounding box already passed the cell budget, so this grid is bounded.
    let mut grid: Vec<Option<Val>> = vec![None; rows * width];
    for (row, col, val) in cells {
        let c = (col - first_col) as usize;
        if Some(row) == header {
            labels[c] = label(&val);
        } else {
            grid[(row - first_row) as usize * width + c] = Some(val);
        }
    }
    let mut kinds: Vec<Kinds> = (0..width).map(|_| Kinds::default()).collect();
    for (i, val) in grid.iter().enumerate() {
        if let Some(val) = val {
            kinds[i % width].observe(val);
        }
    }
    let types: Vec<String> = kinds.iter().map(Kinds::data_type).collect();
    let conn = connection(&scratch, policy)?;
    with_deadline(&conn, policy, || {
        let mut appender = stage::create(&conn, &scratch, width)?;
        for (r, row) in grid.chunks(width.max(1)).enumerate() {
            let values = row
                .iter()
                .zip(&types)
                .map(|(v, ty)| v.as_ref().map(|v| text_for(v, ty)))
                .collect();
            stage::append(&mut appender, r as i64, None, values)?;
            if r % 4096 == 4095 {
                stage::check_size(&scratch, policy)?;
            }
        }
        appender.flush().map_err(AppError::query)?;
        drop(appender);
        stage::create_view(&conn, &types, false)
    })?;
    fingerprint.check(path)?;
    let mut nulls = vec![false; width];
    for (i, val) in grid.iter().enumerate() {
        nulls[i % width] |= val.is_none();
    }
    let columns: Vec<ColumnDescriptor> = labels
        .into_iter()
        .zip(types)
        .enumerate()
        .map(|(i, (name, ty))| stage::column(i, name, ty, nulls[i] || rows == 0))
        .collect();
    let mut warnings = vec![
        "Excel stores numbers as binary doubles. Values show the shortest decimal that round-trips, which can differ from Excel's 15-digit display (for example 0.30000000000000004).".to_string(),
        format!("Dates use the workbook's {} date system and are shown to the millisecond. Formulas are not recalculated; cells show values cached when the workbook was saved.", if workbook.date1904 { "1904" } else { "1900" }),
    ];
    let mixed: Vec<_> = columns
        .iter()
        .filter(|c| c.data_type == sql::JSON_VALUE)
        .map(|c| {
            if c.name.is_empty() {
                c.id.clone()
            } else {
                c.name.clone()
            }
        })
        .collect();
    if !mixed.is_empty() {
        warnings.push(format!("mixed_types: {} mix value types or hold errors/special values. They keep tagged JSON text such as {{\"$excel_error\":\"#DIV/0!\"}} instead of being coerced.", mixed.join(", ")));
    }
    if no_cache > 0 {
        warnings.push(format!("no_cached_value: {no_cache} formula cells have no cached result and are tagged {{\"$no_cached_value\":true}}; Sivro does not evaluate formulas."));
    }
    if !above.is_empty() {
        warnings.push(format!(
            "{} non-empty cells above header row {} are not shown.",
            above.len(),
            options.header_row.unwrap_or(1)
        ));
    }
    if hidden {
        warnings.push(
            "This sheet is hidden in the workbook. Opening it does not change the workbook.".into(),
        );
    }
    if crate::dataset::ambiguous_labels(&columns) {
        warnings.push("Duplicate/blank labels are retained. JSON row copy and exports use column IDs as keys/headers.".into());
    }
    let label = options.sheet.clone();
    let row_count = rows as u64;
    Ok(DuckDbAdapter {
        format: FormatKind::Excel,
        delimited: None,
        excel: Some(ExcelSelection {
            sheet: options.sheet.clone(),
            header_row: options.header_row,
            workbook,
        }),
        conn,
        path: path.to_path_buf(),
        fingerprint,
        object: ObjectDescriptor {
            id: object_id,
            label,
            kind: ObjectKind::Table,
            columns,
            row_count: CountState::known(row_count),
            capabilities: Capabilities {
                page: true,
                filter: true,
                sort: true,
                profile: true,
                exact_count: true,
                copy: true,
                exports: vec![
                    "parquet".into(),
                    "csv".into(),
                    "tsv".into(),
                    "json".into(),
                    "jsonl".into(),
                ],
            },
            warnings,
        },
        row_count: Some(row_count),
        row_groups: vec![],
        created_by: None,
        format_version: 0,
        counts: HashMap::new(),
        profiles: HashMap::new(),
        raw_records: false,
        _scratch: scratch,
    })
}

#[cfg(test)]
pub(crate) fn datetime_for_test(serial: f64, date1904: bool) -> String {
    match excel_datetime(serial, false, date1904) {
        Val::Date(s) | Val::Timestamp(s) | Val::Time(s) | Val::Special(s) => s,
        other => format!("{other:?}"),
    }
}
