use crate::{
    api::{OpenOptions, ProbeResult},
    dataset::FormatKind,
    errors::{AppError, AppResult, ErrorCode},
};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::SystemTime,
};

pub fn extension_format(path: &Path) -> Option<FormatKind> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "parquet" | "pq" => Some(FormatKind::Parquet),
        "csv" => Some(FormatKind::Csv),
        "tsv" => Some(FormatKind::Tsv),
        "json" => Some(FormatKind::Json),
        "jsonl" | "ndjson" => Some(FormatKind::Jsonl),
        "xlsx" | "xls" | "xlsm" | "xlsb" => Some(FormatKind::Excel),
        "sqlite" | "sqlite3" | "db" => Some(FormatKind::Sqlite),
        _ => None,
    }
}

pub fn probe(path: &Path) -> AppResult<ProbeResult> {
    let expected = extension_format(path).ok_or_else(|| {
        AppError::new(
            ErrorCode::UnsupportedFormat,
            "This extension is outside the seven supported input families",
        )
    })?;
    if !std::fs::metadata(path).map_err(AppError::io)?.is_file() {
        return Err(AppError::invalid("Choose a regular local file"));
    }
    let mut file = File::open(path).map_err(AppError::io)?;
    let meta = file.metadata().map_err(AppError::io)?;
    if !meta.is_file() {
        return Err(AppError::invalid("Choose a regular local file"));
    }
    let mut head = [0u8; 16];
    let read = meta.len().min(16) as usize;
    file.read_exact(&mut head[..read]).map_err(AppError::io)?;
    let zip = read >= 4 && (&head[..4] == b"PK\x03\x04" || &head[..4] == b"PK\x05\x06");
    let cfb = read >= 8 && head[..8] == [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
    let detected = if read >= 4 && &head[..4] == b"PAR1" {
        Some(FormatKind::Parquet)
    } else if read == 16 && &head == b"SQLite format 3\0" {
        Some(FormatKind::Sqlite)
    } else {
        None
    };
    if expected == FormatKind::Excel {
        let xls = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("xls"));
        if xls && !cfb {
            return Err(AppError::new(ErrorCode::FormatMismatch, "An .xls workbook must be a legacy compound file. If this is a newer workbook, rename it .xlsx."));
        }
        if !xls && cfb {
            return Err(AppError::new(ErrorCode::FormatMismatch, "This workbook is a compound file: it is either password-protected (unsupported) or a legacy .xls workbook with the wrong extension."));
        }
        if !xls && !zip {
            return Err(AppError::new(
                ErrorCode::FormatMismatch,
                "This .xlsx/.xlsm/.xlsb file is not a ZIP-based workbook",
            ));
        }
    }
    if detected.is_some_and(|format| format != expected) {
        return Err(AppError::new(ErrorCode::FormatMismatch, "The file signature does not match its extension. Reopen a correctly named file; no reinterpretation was performed."));
    }
    if expected == FormatKind::Parquet {
        let mut tail = [0; 4];
        if detected != Some(FormatKind::Parquet) || meta.len() < 12 {
            return Err(AppError::new(
                ErrorCode::FormatMismatch,
                "This file does not have a valid Parquet signature",
            ));
        }
        file.seek(SeekFrom::End(-4)).map_err(AppError::io)?;
        file.read_exact(&mut tail).map_err(AppError::io)?;
        if &tail != b"PAR1" {
            return Err(AppError::new(
                ErrorCode::FormatMismatch,
                "The Parquet footer is missing or truncated",
            ));
        }
    }
    if expected == FormatKind::Sqlite && detected != Some(FormatKind::Sqlite) {
        return Err(AppError::new(
            ErrorCode::FormatMismatch,
            "A .db/.sqlite file must have a valid SQLite header",
        ));
    }
    let text = matches!(
        expected,
        FormatKind::Csv | FormatKind::Tsv | FormatKind::Json | FormatKind::Jsonl
    );
    if text {
        file.seek(SeekFrom::Start(0)).map_err(AppError::io)?;
        let mut prefix = vec![0; meta.len().min(65_536) as usize];
        file.read_exact(&mut prefix).map_err(AppError::io)?;
        let invalid_utf8 = std::str::from_utf8(&prefix)
            .err()
            .is_some_and(|e| e.error_len().is_some() || meta.len() == prefix.len() as u64);
        if prefix.is_empty() || invalid_utf8 || prefix.contains(&0) {
            return Err(AppError::new(ErrorCode::FormatMismatch, "Text formats require nonempty UTF-8 (optional UTF-8 BOM); UTF-16, legacy encodings and NUL bytes are unsupported."));
        }
    }
    Ok(ProbeResult {
        format: expected,
        supported: matches!(
            expected,
            FormatKind::Parquet
                | FormatKind::Csv
                | FormatKind::Tsv
                | FormatKind::Json
                | FormatKind::Jsonl
                | FormatKind::Excel
                | FormatKind::Sqlite
        ),
        message: if expected == FormatKind::Parquet {
            "Parquet signature verified"
        } else if matches!(expected, FormatKind::Csv | FormatKind::Tsv) {
            "UTF-8 prefix verified; confirm a strict delimited parsing preview before opening"
        } else if matches!(expected, FormatKind::Json | FormatKind::Jsonl) {
            "UTF-8 prefix verified; the whole input is validated while opening"
        } else if expected == FormatKind::Excel {
            "Workbook container verified; choose a worksheet and header row"
        } else if expected == FormatKind::Sqlite {
            "SQLite header verified; choose a table or view"
        } else {
            "This format family is planned; its reader is not implemented yet"
        }
        .into(),
    })
}

pub fn resolve(path: &str, options: &OpenOptions) -> AppResult<PathBuf> {
    if path.is_empty() || path.len() > 32_768 {
        return Err(AppError::invalid("Choose a bounded local file path"));
    }
    let canonical = Path::new(path).canonicalize().map_err(AppError::io)?;
    let result = probe(&canonical)?;
    if options
        .format
        .is_some_and(|format| format != result.format && !json_reopen(result.format, format))
    {
        return Err(AppError::new(
            ErrorCode::FormatMismatch,
            "The requested format does not match the probed file",
        ));
    }
    if !result.supported {
        return Err(AppError::new(ErrorCode::UnsupportedFormat, result.message));
    }
    Ok(canonical)
}

fn json_reopen(probed: FormatKind, requested: FormatKind) -> bool {
    matches!(
        (probed, requested),
        (FormatKind::Json, FormatKind::Jsonl) | (FormatKind::Jsonl, FormatKind::Json)
    )
}

/// The probed family, or an explicit JSON <-> JSON Lines reopen chosen by the
/// user after a format-mismatch error. No other reinterpretation is allowed.
pub fn effective_format(path: &Path, options: &OpenOptions) -> AppResult<FormatKind> {
    let probed = extension_format(path)
        .ok_or_else(|| AppError::new(ErrorCode::UnsupportedFormat, "Unsupported input format"))?;
    match options.format {
        Some(requested) if json_reopen(probed, requested) => Ok(requested),
        _ => Ok(probed),
    }
}

/// Holds the original read-only file identity open; comparisons also detect
/// replacement with a same-sized file and ordinary in-place modifications.
pub struct Fingerprint {
    pub size: u64,
    modified: SystemTime,
    identity: same_file::Handle,
    /// SQLite: data changes are live (and WAL checkpoints move size/mtime),
    /// so only replacement of the file itself is a stale source.
    identity_only: bool,
}
impl Fingerprint {
    pub fn read(path: &Path) -> AppResult<Self> {
        if !std::fs::metadata(path).map_err(AppError::io)?.is_file() {
            return Err(AppError::invalid("Choose a regular local file"));
        }
        let file = File::open(path).map_err(AppError::io)?;
        let meta = file.metadata().map_err(AppError::io)?;
        if !meta.is_file() {
            return Err(AppError::invalid("Choose a regular file"));
        }
        Ok(Self {
            size: meta.len(),
            modified: meta.modified().map_err(AppError::io)?,
            identity: same_file::Handle::from_file(file).map_err(AppError::io)?,
            identity_only: false,
        })
    }
    pub fn read_identity(path: &Path) -> AppResult<Self> {
        Ok(Self {
            identity_only: true,
            ..Self::read(path)?
        })
    }
    pub fn check(&self, path: &Path) -> AppResult<()> {
        let now = Self::read(path).map_err(|_| {
            AppError::new(
                ErrorCode::StaleDataset,
                "The source is unavailable or changed. Refresh to reopen it.",
            )
        })?;
        let same = self.identity == now.identity
            && (self.identity_only || (self.size == now.size && self.modified == now.modified));
        if !same {
            return Err(AppError::new(
                ErrorCode::StaleDataset,
                "The source changed. Refresh before reading more data.",
            ));
        }
        Ok(())
    }
}
