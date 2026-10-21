//! Container checks that run before Calamine allocates anything. ZIP-based
//! workbooks are checked from the central directory (zip 9 also enforces
//! each declared size while reading). XLS is scanned with a minimal
//! read-only CFB/BIFF walker that mirrors Calamine's allocations: it reserves
//! each sheet's declared DIMENSIONS and builds every sheet's range at open.
use crate::{
    errors::{AppError, AppResult, ErrorCode},
    policy::ResourcePolicy,
};
use std::{fs::File, io::Read, path::Path};

/// Archives with more entries than any reasonable workbook are refused before
/// the central directory is walked further.
const MAX_ENTRIES: usize = 10_000;
/// Deflated XML is often 10-100x smaller; far beyond that is a bomb pattern.
const MAX_RATIO: u64 = 1_000;

fn malformed(detail: impl std::fmt::Display) -> AppError {
    AppError::new(
        ErrorCode::FormatMismatch,
        format!("This workbook is damaged or not a supported Excel file: {detail}"),
    )
}

pub fn zip(path: &Path, size: u64, policy: &ResourcePolicy) -> AppResult<()> {
    if size > policy.excel_workbook_bytes {
        return Err(AppError::limit(format!(
            "Workbooks are limited to {} MiB",
            policy.excel_workbook_bytes >> 20
        )));
    }
    let mut archive =
        zip::ZipArchive::new(File::open(path).map_err(AppError::io)?).map_err(malformed)?;
    if archive.len() > MAX_ENTRIES {
        return Err(AppError::limit("The workbook archive has too many entries"));
    }
    let mut total = 0u64;
    for i in 0..archive.len() {
        let entry = archive.by_index_raw(i).map_err(malformed)?;
        if entry.encrypted() {
            return Err(AppError::new(
                ErrorCode::UnsupportedFormat,
                "Encrypted workbook entries are not supported",
            ));
        }
        let (unpacked, packed) = (entry.size(), entry.compressed_size());
        total = total.saturating_add(unpacked);
        if unpacked > policy.excel_entry_bytes || total > policy.excel_payload_bytes {
            return Err(AppError::limit(format!(
                "Decompressed workbook parts exceed the {} MiB per-part or {} MiB total limit",
                policy.excel_entry_bytes >> 20,
                policy.excel_payload_bytes >> 20
            )));
        }
        if unpacked > 1 << 20 && unpacked / packed.max(1) > MAX_RATIO {
            return Err(AppError::limit(
                "A workbook part has a suspicious compression ratio and was not decompressed",
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// XLS: compound file (CFB) and BIFF records
// ---------------------------------------------------------------------------

// Calamine 0.37 `cfb.rs` constants.
const RESERVED_SECTORS: u32 = 0xFFFF_FFFA;
const DIFSECT: u32 = 0xFFFF_FFFC;
const END_OF_CHAIN: u32 = 0xFFFF_FFFE;

fn u16_at(b: &[u8], at: usize) -> AppResult<u16> {
    b.get(at..at + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
        .ok_or_else(|| malformed("truncated structure"))
}
fn u32_at(b: &[u8], at: usize) -> AppResult<u32> {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| malformed("truncated structure"))
}
fn u32s(b: &[u8]) -> AppResult<Vec<u32>> {
    // Calamine asserts this; a ragged sector would panic there.
    if !b.len().is_multiple_of(4) {
        return Err(malformed("ragged sector"));
    }
    (0..b.len() / 4).map(|i| u32_at(b, i * 4)).collect()
}

/// Sector access exactly as Calamine's `Sectors`: ids above `maxid` fail and
/// a sector cut by end of file is returned short.
struct Sectors<'a> {
    data: &'a [u8],
    size: usize,
    maxid: u64,
    /// Calamine reads a mini sector past the loaded mini stream from an
    /// arbitrary file position; such chains are refused.
    strict: bool,
}
impl Sectors<'_> {
    fn get(&self, id: u32) -> AppResult<&[u8]> {
        if id as u64 > self.maxid {
            return Err(malformed("sector beyond end of file"));
        }
        let start = (id as usize).saturating_mul(self.size);
        if self.strict && start.saturating_add(self.size) > self.data.len() {
            return Err(malformed("mini sector outside the mini stream"));
        }
        let end = start.saturating_add(self.size).min(self.data.len());
        Ok(self.data.get(start..end).unwrap_or(&[]))
    }
    /// Calamine's `get_chain` has no cycle guard and can loop or grow without
    /// bound; any chain longer than the sector count is refused here instead.
    fn chain(&self, mut id: u32, fat: &[u32], len: usize) -> AppResult<Vec<u8>> {
        let mut out = Vec::new();
        let mut steps = 0u64;
        while id != END_OF_CHAIN {
            steps += 1;
            if steps > self.maxid + 1 {
                return Err(malformed("cyclic or overlong sector chain"));
            }
            let sector = self.get(id)?;
            let take = sector.len().min(len - out.len());
            out.extend_from_slice(&sector[..take]);
            if out.len() >= len {
                break;
            }
            id = *fat
                .get(id as usize)
                .ok_or_else(|| malformed("sector chain leaves the FAT"))?;
        }
        Ok(out)
    }
}

/// Returns the bytes Calamine 0.37 would parse as the workbook stream,
/// following its CFB reader step for step (`Cfb::new`, `get_stream` and
/// `Xls::parse_workbook`'s name order), with bounds Calamine lacks.
fn workbook_stream(bytes: &[u8]) -> AppResult<Vec<u8>> {
    if bytes.len() < 512 || bytes[..8] != [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1] {
        return Err(malformed("missing compound-file header"));
    }
    let version = u16_at(bytes, 26)?;
    let size = match u16_at(bytes, 30)? {
        9 => 512usize,
        12 if bytes.len() >= 4096 => 4096,
        _ => return Err(malformed("unsupported sector size")),
    };
    if u16_at(bytes, 32)? != 6 {
        return Err(malformed("unsupported mini sector size"));
    }
    let dir_start = u32_at(bytes, 48)?;
    let mini_fat_start = u32_at(bytes, 60)?;
    let mini_fat_len = u32_at(bytes, 64)? as usize;
    // Sector 0 starts after the header block (512 bytes, or 4096 for v4).
    let sectors = Sectors {
        data: &bytes[size..],
        size,
        maxid: (bytes.len() / size) as u64,
        strict: false,
    };
    let mut difat = u32s(&bytes[76..512])?;
    let mut id = u32_at(bytes, 68)?;
    let mut steps = 0u64;
    while id < RESERVED_SECTORS {
        steps += 1;
        if steps > sectors.maxid + 1 {
            return Err(malformed("cyclic DIFAT chain"));
        }
        difat.extend(u32s(sectors.get(id)?)?);
        id = difat.pop().ok_or_else(|| malformed("empty DIFAT"))?;
    }
    let mut fat = Vec::new();
    for id in difat.into_iter().filter(|id| *id < DIFSECT) {
        fat.extend(u32s(sectors.get(id)?)?);
    }
    let directory = sectors.chain(dir_start, &fat, usize::MAX)?;
    struct Entry {
        name: String,
        start: u32,
        len: usize,
    }
    let entries = directory
        .chunks(128)
        .map(|e| {
            if e.len() < 128 {
                return Err(malformed("truncated directory entry"));
            }
            let units: Vec<u16> = (0..32)
                .map(|i| u16::from_le_bytes([e[i * 2], e[i * 2 + 1]]))
                .collect();
            let mut name = String::from_utf16_lossy(&units);
            if let Some(nul) = name.as_bytes().iter().position(|b| *b == 0) {
                name.truncate(nul);
            }
            let len = if size == 512 {
                u32_at(e, 120)? as u64
            } else {
                u32_at(e, 120)? as u64 | (u32_at(e, 124)? as u64) << 32
            };
            Ok(Entry {
                name,
                start: u32_at(e, 116)?,
                len: usize::try_from(len).map_err(|_| malformed("stream length"))?,
            })
        })
        .collect::<AppResult<Vec<_>>>()?;
    let root = entries
        .first()
        .ok_or_else(|| malformed("empty directory"))?;
    if version != 3 && root.start == END_OF_CHAIN {
        return Err(malformed("empty root directory"));
    }
    if entries.iter().any(|e| e.name == "EncryptedPackage") {
        return Err(AppError::new(
            ErrorCode::UnsupportedFormat,
            "Password-protected workbooks are not supported",
        ));
    }
    let (mini_fat, mini_stream) = if mini_fat_len > 0 {
        let stream = sectors.chain(root.start, &fat, root.len)?;
        let table = sectors.chain(mini_fat_start, &fat, mini_fat_len.saturating_mul(size))?;
        (u32s(&table)?, stream)
    } else {
        (Vec::new(), Vec::new())
    };
    let stream = ["Workbook", "Book", "WORKBOOK", "BOOK"]
        .iter()
        .find_map(|name| entries.iter().find(|e| e.name == *name))
        .ok_or_else(|| malformed("no Workbook stream"))?;
    if stream.len < 4096 {
        let mini = Sectors {
            data: &mini_stream,
            size: 64,
            maxid: sectors.maxid * 8,
            strict: true,
        };
        mini.chain(stream.start, &mini_fat, stream.len)
    } else {
        sectors.chain(stream.start, &fat, stream.len)
    }
}

/// BIFF records with following CONTINUE records absorbed, as Calamine does.
fn records(mut stream: &[u8]) -> impl Iterator<Item = AppResult<(u16, &[u8])>> {
    std::iter::from_fn(move || {
        if stream.is_empty() {
            return None;
        }
        let record = (|| {
            let typ = u16_at(stream, 0)?;
            let len = u16_at(stream, 2)? as usize;
            let data = stream
                .get(4..4 + len)
                .ok_or_else(|| malformed("truncated BIFF record"))?;
            stream = &stream[4 + len..];
            while stream.len() > 4 && u16_at(stream, 0)? == 0x003C {
                let len = u16_at(stream, 2)? as usize;
                stream = stream
                    .get(4 + len..)
                    .ok_or_else(|| malformed("truncated CONTINUE record"))?;
            }
            Ok((typ, data))
        })();
        if record.is_err() {
            stream = &[];
        }
        Some(record)
    })
}

/// Cells Calamine allocates for one sheet: the larger of its DIMENSIONS
/// reservation and the bounding box of value records.
#[derive(Default)]
struct SheetCells {
    reserve: u64,
    min: Option<(u32, u32)>,
    max: (u32, u32),
}
impl SheetCells {
    fn cell(&mut self, row: u32, col: u32) {
        let min = self.min.get_or_insert((row, col));
        *min = (min.0.min(row), min.1.min(col));
        self.max = (self.max.0.max(row), self.max.1.max(col));
    }
    fn allocated(&self) -> u64 {
        let range = self.min.map_or(0, |min| {
            (self.max.0 as u64 - min.0 as u64 + 1) * (self.max.1 as u64 - min.1 as u64 + 1)
        });
        range.max(self.reserve)
    }
}

/// Mirrors calamine 0.37 `parse_dimensions` and the `cells.reserve` that
/// follows, including its wrapping u32 arithmetic in release builds.
fn dimensions_reserve(data: &[u8]) -> AppResult<u64> {
    let (rf, rl, mut cf, cl) = match data.len() {
        10 => (
            u16_at(data, 0)? as u32,
            u16_at(data, 2)? as u32,
            u16_at(data, 4)? as u32,
            u16_at(data, 6)? as u32,
        ),
        14 | 16 => (
            u32_at(data, 0)?,
            u32_at(data, 4)?,
            u16_at(data, 8)? as u32,
            u16_at(data, 10)? as u32,
        ),
        _ => return Err(malformed("invalid DIMENSIONS record")),
    };
    if 0xFF < cf || cl < cf {
        cf = 0;
    }
    let (start, end) = if rl >= 1 && cl >= 1 {
        ((rf, cf), (rl - 1, cl - 1))
    } else {
        ((rf, cf), (rf, cf))
    };
    let rows = end.0.wrapping_sub(start.0).wrapping_add(1) as u64;
    let cols = end.1.wrapping_sub(start.1).wrapping_add(1) as u64;
    Ok(rows.saturating_mul(cols))
}

pub fn xls(path: &Path, size: u64, policy: &ResourcePolicy) -> AppResult<()> {
    if size > policy.xls_workbook_bytes {
        return Err(AppError::limit(format!(
            ".xls workbooks are limited to {} MiB",
            policy.xls_workbook_bytes >> 20
        )));
    }
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(AppError::io)?
        .take(policy.xls_workbook_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(AppError::io)?;
    let stream = workbook_stream(&bytes)?;
    // Globals end at the first EOF; BoundSheet8 records give each sheet's
    // stream offset, exactly as Calamine reads them.
    let mut offsets = Vec::new();
    for record in records(&stream) {
        let (typ, data) = record?;
        match typ {
            0x002F if u16_at(data, 0)? != 0 => {
                return Err(AppError::new(
                    ErrorCode::UnsupportedFormat,
                    "Password-protected workbooks are not supported",
                ))
            }
            0x0085 => offsets.push(u32_at(data, 0)? as usize),
            0x000A => break,
            _ => {}
        }
    }
    let mut sheets = Vec::with_capacity(offsets.len());
    for offset in offsets {
        let mut sheet = SheetCells::default();
        let (mut seen_bof, mut nested) = (false, 0usize);
        let body = stream
            .get(offset..)
            .ok_or_else(|| malformed("sheet offset outside the workbook stream"))?;
        for record in records(body) {
            let (typ, data) = record?;
            if nested > 0 {
                match typ {
                    0x0809 => nested += 1,
                    0x000A => nested -= 1,
                    _ => {}
                }
                continue;
            }
            match typ {
                0x0809 if seen_bof => nested += 1,
                0x0809 => seen_bof = true,
                0x0200 => sheet.reserve = sheet.reserve.max(dimensions_reserve(data)?),
                0x0203 | 0x0204 | 0x00D6 | 0x0205 | 0x027E | 0x00FD | 0x0006 => {
                    sheet.cell(u16_at(data, 0)? as u32, u16_at(data, 2)? as u32)
                }
                0x00BD => {
                    let row = u16_at(data, 0)? as u32;
                    let first = u16_at(data, 2)? as u32;
                    let last = u16_at(data, data.len().saturating_sub(2))? as u32;
                    sheet.cell(row, first);
                    sheet.cell(row, last.max(first));
                }
                0x000A => break,
                _ => {}
            }
        }
        sheets.push(sheet);
    }
    // Calamine parses every sheet at open, so the cell budget covers the
    // whole workbook rather than only the selected sheet.
    let total = sheets
        .iter()
        .fold(0u64, |sum, s| sum.saturating_add(s.allocated()));
    if total > policy.excel_cells as u64 {
        return Err(AppError::limit(format!(
            "This .xls workbook declares or uses {total} cells across its sheets; .xls opens every sheet at once, so the whole workbook must stay within {} cells",
            policy.excel_cells
        )));
    }
    Ok(())
}
