//! Local HDF5 exploration. Only read-only positioned I/O reaches the parser;
//! no native libraries, plugins, external links or external raw storage.
mod output;
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
    scratch::{free_space, SessionScratch},
};
use hdf5_pure::{
    AttrValue, Dataset, Datatype as Ty, DatatypeByteOrder as Order, File as HFile, FormatError,
    Source,
};
pub use output::{export, ExportSource};
use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Instant,
};

fn error(e: impl std::fmt::Display) -> AppError {
    let message = e.to_string();
    if message.contains("invalid data layout version") {
        return AppError::new(ErrorCode::UnsupportedFormat, "This HDF5 layout version is unsupported by the bundled reader. Choose another dataset or a copy saved with layout v3, v4 or v5.");
    }
    AppError::new(
        if message.contains("budget") {
            ErrorCode::ResourceLimit
        } else if message.contains("time limit") {
            ErrorCode::Timeout
        } else {
            ErrorCode::FormatMismatch
        },
        format!("Could not read HDF5: {message}"),
    )
}
fn guarded<T>(f: impl FnOnce() -> AppResult<T>) -> AppResult<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .unwrap_or_else(|_| Err(error("the parser rejected an inconsistent structure")))
}
#[derive(Clone)]
struct BoundedSource {
    file: Arc<Mutex<File>>,
    size: u64,
    policy: ResourcePolicy,
    started: Instant,
    bytes: Arc<AtomicU64>,
}
impl BoundedSource {
    fn check(&self, len: usize) -> Result<(), FormatError> {
        if self.started.elapsed().as_millis() > self.policy.timeout_ms as u128 {
            return Err(FormatError::Source("HDF5 time limit exceeded".into()));
        }
        if len as u64 > self.policy.hdf5_read_bytes
            || self
                .bytes
                .fetch_add(len as u64, Ordering::Relaxed)
                .saturating_add(len as u64)
                > self.policy.scratch_bytes.saturating_mul(4)
        {
            return Err(FormatError::Source("HDF5 read budget exceeded".into()));
        }
        Ok(())
    }
}
impl Source for BoundedSource {
    fn len(&self) -> u64 {
        self.size
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), FormatError> {
        self.check(buf.len())?;
        if offset
            .checked_add(buf.len() as u64)
            .is_none_or(|n| n > self.size)
        {
            return Err(FormatError::Source("read outside the HDF5 file".into()));
        }
        let mut file = self
            .file
            .lock()
            .map_err(|_| FormatError::Source("file lock unavailable".into()))?;
        file.seek(SeekFrom::Start(offset))
            .and_then(|_| file.read_exact(buf))
            .map_err(|e| FormatError::Source(e.to_string()))
    }
    fn read_exact_at(&self, offset: u64, len: usize) -> Result<Vec<u8>, FormatError> {
        if len as u64 > self.policy.hdf5_read_bytes
            || offset.checked_add(len as u64).is_none_or(|n| n > self.size)
        {
            return Err(FormatError::Source(
                "HDF5 allocation budget or file bounds exceeded".into(),
            ));
        }
        let mut out = vec![0; len];
        self.read_at(offset, &mut out)?;
        Ok(out)
    }
    fn read_metadata_at(&self, offset: u64, len: usize) -> Result<Vec<u8>, FormatError> {
        if len > self.policy.max_page_bytes {
            return Err(FormatError::Source("HDF5 metadata budget exceeded".into()));
        }
        self.read_exact_at(offset, len)
    }
}
struct Reader {
    file: HFile,
    source: BoundedSource,
}
impl Reader {
    fn open(path: &Path, policy: &ResourcePolicy) -> AppResult<Self> {
        let file = File::open(path).map_err(AppError::io)?;
        let size = file.metadata().map_err(AppError::io)?.len();
        if size > policy.hdf5_file_bytes {
            return Err(AppError::limit("HDF5 file exceeds the input budget"));
        }
        let source = BoundedSource {
            file: Arc::new(Mutex::new(file)),
            size,
            policy: policy.clone(),
            started: Instant::now(),
            bytes: Arc::new(AtomicU64::new(0)),
        };
        let file = HFile::from_source(source.clone()).map_err(error)?;
        Ok(Self { file, source })
    }
    fn heap(&self, raw: &[u8], base: &Ty) -> AppResult<Vec<u8>> {
        let os = self.file.superblock().offset_size as usize;
        let ls = self.file.superblock().length_size as usize;
        if raw.len() < 8 + os {
            return Err(error("truncated variable-length reference"));
        }
        let count = uint(&raw[..4], Order::LittleEndian)?;
        let len = count
            .checked_mul(if matches!(base, Ty::String { .. }) {
                1
            } else {
                base.type_size() as u128
            })
            .ok_or_else(|| error("variable length overflow"))?;
        if len > self.source.policy.hdf5_read_bytes as u128 {
            return Err(AppError::limit(
                "HDF5 variable-length payload exceeds the read budget",
            ));
        }
        if len == 0 {
            return Ok(vec![]);
        }
        let addr = uint(&raw[4..4 + os], Order::LittleEndian)? as u64;
        let index = uint(&raw[4 + os..8 + os], Order::LittleEndian)? as u64;
        let start = addr
            .checked_add(self.file.superblock().base_address.get())
            .ok_or_else(|| error("heap address overflow"))?;
        let header = self.source.read_exact_at(start, 8 + ls).map_err(error)?;
        if &header[..4] != b"GCOL" || header[4] != 1 {
            return Err(error("invalid global heap"));
        }
        let collection = uint(&header[8..], Order::LittleEndian)? as u64;
        let end = start
            .checked_add(collection)
            .filter(|n| *n <= self.source.size)
            .ok_or_else(|| error("heap outside file"))?;
        let mut pos = start + (8 + ls) as u64;
        while pos < end {
            let h = self.source.read_exact_at(pos, 8 + ls).map_err(error)?;
            let id = u16::from_le_bytes([h[0], h[1]]) as u64;
            if id == 0 {
                break;
            }
            let size = uint(&h[8..], Order::LittleEndian)? as u64;
            let data = pos + (8 + ls) as u64;
            let next = size
                .checked_add(7)
                .map(|n| n & !7)
                .and_then(|padded| data.checked_add(padded))
                .filter(|n| *n <= end)
                .ok_or_else(|| error("invalid heap object size"))?;
            if id == index {
                if len > size as u128 {
                    return Err(error("variable length exceeds heap object"));
                }
                return self.source.read_exact_at(data, len as usize).map_err(error);
            }
            pos = next;
        }
        Err(error("variable-length heap object not found"))
    }
}
fn path_ok(path: &str) -> AppResult<()> {
    if !path.starts_with('/')
        || path.len() > 4096
        || path.split('/').count() > 64
        || path
            .split('/')
            .any(|s| s == "." || s == ".." || s.contains('\0'))
    {
        Err(AppError::invalid(
            "Choose a bounded absolute HDF5 object path",
        ))
    } else {
        Ok(())
    }
}
fn child(group: &str, name: &str) -> String {
    format!("{}/{}", group.trim_end_matches('/'), name)
}
fn attributes(
    types: HashMap<String, Ty>,
    values: HashMap<String, AttrValue>,
    policy: &ResourcePolicy,
) -> AppResult<Vec<Hdf5Attribute>> {
    let mut attrs = types
        .into_iter()
        .map(|(name, ty)| Hdf5Attribute {
            value: values.get(&name).map(|v| format!("{v:?}")),
            name,
            data_type: ty.to_string(),
        })
        .collect::<Vec<_>>();
    attrs.sort_by(|a, b| a.name.cmp(&b.name));
    if attrs.len() > policy.max_columns
        || serde_json::to_vec(&attrs)
            .map_err(|_| AppError::internal())?
            .len()
            > policy.max_page_bytes
    {
        return Err(AppError::limit(
            "HDF5 attributes exceed the metadata budget",
        ));
    }
    Ok(attrs)
}
fn object_info(
    path: String,
    name: String,
    ds: &Dataset,
    policy: &ResourcePolicy,
) -> AppResult<Hdf5Object> {
    let shape = ds.shape().map_err(error)?;
    let ty = ds.datatype().map_err(error)?;
    let layout = ds.layout();
    let reason = check_dataset(ds, policy).err().map(|e| e.message);
    Ok(Hdf5Object {
        path,
        name,
        kind: "dataset".into(),
        shape: shape.iter().map(ToString::to_string).collect(),
        data_type: ty.to_string(),
        layout: layout
            .map(|l| format!("{l:?}"))
            .unwrap_or_else(|e| e.to_string()),
        chunk_shape: ds
            .chunk_shape()
            .ok()
            .flatten()
            .map(|s| s.iter().map(ToString::to_string).collect()),
        filters: ds.filters(),
        attributes: attributes(
            ds.attr_datatypes().map_err(error)?,
            ds.attrs().map_err(error)?,
            policy,
        )?,
        selectable: reason.is_none(),
        reason,
    })
}
fn inspect_reader(reader: &Reader, group: &str, policy: &ResourcePolicy) -> AppResult<Hdf5Info> {
    path_ok(group)?;
    let g = reader.file.group(group).map_err(error)?;
    let mut objects = vec![];
    for name in g.groups().map_err(error)? {
        objects.push(Hdf5Object {
            path: child(group, &name),
            name,
            kind: "group".into(),
            shape: vec![],
            data_type: String::new(),
            layout: String::new(),
            chunk_shape: None,
            filters: vec![],
            attributes: vec![],
            selectable: false,
            reason: None,
        });
    }
    for (name, ds) in g.iter_datasets().map_err(error)? {
        if objects.len() >= policy.max_columns {
            return Err(AppError::limit("HDF5 group exceeds the object budget"));
        }
        objects.push(
            object_info(child(group, &name), name.clone(), &ds, policy).unwrap_or_else(|e| {
                Hdf5Object {
                    path: child(group, &name),
                    name,
                    kind: "dataset".into(),
                    shape: ds
                        .shape()
                        .unwrap_or_default()
                        .iter()
                        .map(ToString::to_string)
                        .collect(),
                    data_type: "Unsupported datatype or metadata".into(),
                    layout: String::new(),
                    chunk_shape: None,
                    filters: ds.filters(),
                    attributes: vec![],
                    selectable: false,
                    reason: Some(e.message),
                }
            }),
        );
    }
    for name in g.named_datatypes().map_err(error)? {
        objects.push(Hdf5Object {
            path: child(group, &name),
            data_type: g
                .named_datatype(&name)
                .map(|t| t.to_string())
                .unwrap_or_else(|e| e.to_string()),
            name,
            kind: "datatype".into(),
            shape: vec![],
            layout: String::new(),
            chunk_shape: None,
            filters: vec![],
            attributes: vec![],
            selectable: false,
            reason: Some("Committed datatype metadata; choose a dataset to view values".into()),
        });
    }
    if objects.len() > policy.max_columns {
        return Err(AppError::limit("HDF5 group exceeds the object budget"));
    }
    objects.sort_by(|a, b| a.name.cmp(&b.name));
    let datasets = objects
        .iter()
        .filter(|o| o.kind == "dataset")
        .collect::<Vec<_>>();
    let columns_selectable = !datasets.is_empty()
        && datasets
            .iter()
            .all(|d| d.selectable && d.shape.len() == 1 && d.shape == datasets[0].shape);
    let info=Hdf5Info{group:group.into(),attributes:attributes(g.attr_datatypes().map_err(error)?,g.attrs().map_err(error)?,policy)?,objects,columns_selectable,warnings:vec!["Only hard links inside this file are browsed. Soft/external links, external raw storage and virtual datasets are not followed; object references remain bytes.".into(),"A selected slice or row window becomes an independent table. Refresh keeps that selection. Exports write the table, not the source hierarchy.".into()]};
    if serde_json::to_vec(&info)
        .map_err(|_| AppError::internal())?
        .len()
        > policy.max_page_bytes
    {
        return Err(AppError::limit(
            "HDF5 group metadata exceeds the response budget",
        ));
    }
    Ok(info)
}
pub fn inspect(path: &Path, group: &str, policy: &ResourcePolicy) -> AppResult<Hdf5Info> {
    guarded(|| {
        let fingerprint = Fingerprint::read(path)?;
        let reader = Reader::open(path, policy)?;
        let info = inspect_reader(&reader, group, policy)?;
        fingerprint.check(path)?;
        Ok(info)
    })
}
fn product(shape: &[u64]) -> AppResult<u64> {
    shape.iter().try_fold(1u64, |n, v| {
        n.checked_mul(*v)
            .ok_or_else(|| AppError::limit("HDF5 dimensions exceed the resource budget"))
    })
}
fn check_dataset(ds: &Dataset, p: &ResourcePolicy) -> AppResult<()> {
    let shape = ds.shape().map_err(error)?;
    if shape.len() > 32 {
        return Err(AppError::limit("HDF5 rank exceeds 32"));
    }
    let size = ds.element_size().map_err(error)?;
    let row = product(shape.get(1..).unwrap_or(&[]))?
        .checked_mul(size)
        .ok_or_else(|| AppError::limit("HDF5 row size overflow"))?;
    if row > p.hdf5_read_bytes {
        return Err(AppError::limit(
            "HDF5 leading-dimension row exceeds the read budget",
        ));
    }
    if let Some(chunks) = ds.chunk_shape().map_err(error)? {
        if product(&chunks)?
            .checked_mul(size)
            .is_none_or(|n| n > p.hdf5_read_bytes)
        {
            return Err(AppError::limit(
                "Decoded HDF5 chunk exceeds the read budget",
            ));
        }
    }
    if ds
        .filters()
        .iter()
        .any(|f| !matches!(*f, 1 | 2 | 3 | 5 | 6 | 32000))
    {
        return Err(AppError::new(
            ErrorCode::UnsupportedFormat,
            "This HDF5 filter is unsupported (no external filter plugins are loaded)",
        ));
    }
    if matches!(ds.layout().map_err(error)?, hdf5_pure::Layout::Virtual) {
        return Err(AppError::new(
            ErrorCode::UnsupportedFormat,
            "Virtual HDF5 datasets are not followed",
        ));
    }
    ds.read_raw_rows(0, 0).map_err(error)?;
    Ok(())
}
fn uint(bytes: &[u8], order: Order) -> AppResult<u128> {
    if bytes.is_empty() || bytes.len() > 16 {
        return Err(error("unsupported integer width"));
    }
    let mut out = [0u8; 16];
    match order {
        Order::LittleEndian => out[..bytes.len()].copy_from_slice(bytes),
        Order::BigEndian => {
            for (i, b) in bytes.iter().rev().enumerate() {
                out[i] = *b;
            }
        }
        _ => return Err(error("unsupported numeric byte order")),
    };
    Ok(u128::from_le_bytes(out))
}
fn standard_float(ty: &Ty) -> bool {
    matches!(ty, Ty::FloatingPoint { size: 4, layout, .. } if *layout == hdf5_pure::FloatingPointLayout::IEEE754_BINARY32)
        || matches!(ty, Ty::FloatingPoint { size: 8, layout, .. } if *layout == hdf5_pure::FloatingPointLayout::IEEE754_BINARY64)
        || matches!(ty, Ty::FloatingPoint { size: 2, layout, .. } if layout.bit_offset == 0 && layout.bit_precision == 16 && layout.exponent_location == 10 && layout.exponent_size == 5 && layout.mantissa_location == 0 && layout.mantissa_size == 10 && layout.exponent_bias == 15)
}

fn numeric(ty: &Ty, raw: &[u8]) -> AppResult<String> {
    match ty {
        Ty::FixedPoint {
            byte_order, layout, ..
        } => {
            let bits = layout.bit_precision as u32;
            if bits == 0 || bits > 128 || layout.bit_offset as u32 + bits > raw.len() as u32 * 8 {
                return Err(error("invalid integer bit layout"));
            }
            let mut n = uint(raw, *byte_order)? >> layout.bit_offset;
            if bits < 128 {
                n &= (1u128 << bits) - 1;
            }
            Ok(if layout.signed {
                if bits == 128 {
                    (n as i128).to_string()
                } else {
                    (((n << (128 - bits)) as i128) >> (128 - bits)).to_string()
                }
            } else {
                n.to_string()
            })
        }
        Ty::FloatingPoint {
            size,
            byte_order,
            layout,
        } => {
            let bits = uint(raw, *byte_order)?;
            let n = match *size {
                2 if standard_float(ty) => {
                    let sign = if bits & 0x8000 == 0 { 1.0 } else { -1.0 };
                    let exp = ((bits >> 10) & 31) as i32;
                    let frac = (bits & 1023) as f64;
                    sign * if exp == 31 {
                        if frac == 0.0 {
                            f64::INFINITY
                        } else {
                            f64::NAN
                        }
                    } else if exp == 0 {
                        frac * 2f64.powi(-24)
                    } else {
                        (1.0 + frac / 1024.0) * 2f64.powi(exp - 15)
                    }
                }

                4 if *layout == hdf5_pure::FloatingPointLayout::IEEE754_BINARY32 => {
                    f32::from_bits(bits as u32) as f64
                }
                8 if *layout == hdf5_pure::FloatingPointLayout::IEEE754_BINARY64 => {
                    f64::from_bits(bits as u64)
                }
                _ => return Err(error("nonstandard floating-point layout")),
            };
            Ok(n.to_string())
        }
        Ty::Enumeration { base_type, .. } => numeric(base_type, raw),
        _ => Err(error("not numeric")),
    }
}
fn sql_type(ty: &Ty) -> String {
    match ty {
        Ty::FixedPoint { size, layout, .. } if *size <= 16 => if layout.signed {
            if *size <= 8 {
                "BIGINT"
            } else {
                "HUGEINT"
            }
        } else if *size <= 8 {
            "UBIGINT"
        } else {
            "UHUGEINT"
        }
        .into(),
        Ty::FloatingPoint { .. } if standard_float(ty) => "DOUBLE".into(),
        Ty::Enumeration {
            base_type, members, ..
        } => {
            if members.len() == 2
                && members
                    .iter()
                    .all(|m| m.name == "FALSE" || m.name == "TRUE")
            {
                "BOOLEAN".into()
            } else {
                sql_type(base_type)
            }
        }
        Ty::String { .. }
        | Ty::VariableLength {
            is_string: true, ..
        } => "VARCHAR".into(),
        Ty::Compound { .. }
        | Ty::Array { .. }
        | Ty::VariableLength {
            is_string: false, ..
        } => sql::JSON_VALUE.into(),
        _ => "BLOB".into(),
    }
}
fn json_number(s: &str) -> AppResult<serde_json::Value> {
    serde_json::from_str(s).map_err(error)
}
fn value(reader: &Reader, ty: &Ty, raw: &[u8], depth: usize) -> AppResult<String> {
    if depth > 32 {
        return Err(AppError::limit("HDF5 value nesting exceeds the budget"));
    }
    match ty {
        Ty::FixedPoint { size, .. } if *size <= 16 => numeric(ty, raw),
        Ty::FloatingPoint { .. } if sql_type(ty) == "DOUBLE" => numeric(ty, raw),
        Ty::Enumeration {
            base_type, members, ..
        } => {
            let n = numeric(base_type, raw)?;
            if sql_type(ty) == "BOOLEAN" {
                let member = members
                    .iter()
                    .find(|m| numeric(base_type, &m.value).ok().as_ref() == Some(&n))
                    .ok_or_else(|| error("value outside boolean enumeration"))?;
                Ok((member.name == "TRUE").to_string())
            } else {
                Ok(n)
            }
        }
        Ty::String { padding, .. } => {
            let bytes = match padding {
                hdf5_pure::StringPadding::NullTerminate => {
                    &raw[..raw.iter().position(|b| *b == 0).unwrap_or(raw.len())]
                }
                hdf5_pure::StringPadding::NullPad => raw.trim_ascii_end_null(),
                hdf5_pure::StringPadding::SpacePad => {
                    &raw[..raw.iter().rposition(|b| *b != b' ').map_or(0, |i| i + 1)]
                }
            };
            String::from_utf8(bytes.to_vec()).map_err(error)
        }
        Ty::VariableLength {
            is_string,
            base_type,
            ..
        } => {
            let bytes = reader.heap(raw, base_type)?;
            if *is_string {
                String::from_utf8(bytes).map_err(error)
            } else {
                let width = base_type.type_size() as usize;
                let array = bytes
                    .chunks_exact(width)
                    .map(|b| json_value(reader, base_type, b, depth + 1))
                    .collect::<AppResult<Vec<_>>>()?;
                serde_json::to_string(&array).map_err(error)
            }
        }
        Ty::Array {
            base_type,
            dimensions,
        } => {
            if dimensions.len() > 32 {
                return Err(AppError::limit("HDF5 array rank exceeds 32"));
            }
            let n = dimensions.iter().try_fold(1usize, |a, b| {
                a.checked_mul(*b as usize)
                    .ok_or_else(|| error("array size overflow"))
            })?;
            let width = base_type.type_size() as usize;
            if n.checked_mul(width) != Some(raw.len()) {
                return Err(error("array storage does not match its dimensions"));
            }
            let array = raw
                .chunks_exact(width)
                .map(|b| json_value(reader, base_type, b, depth + 1))
                .collect::<AppResult<Vec<_>>>()?;
            serde_json::to_string(&reshape(&mut array.into_iter(), dimensions)?).map_err(error)
        }
        Ty::Compound { members, .. } => {
            let mut object = serde_json::Map::new();
            for member in members {
                let start = usize::try_from(member.byte_offset).map_err(error)?;
                let end = start
                    .checked_add(member.datatype.type_size() as usize)
                    .ok_or_else(|| error("compound offset overflow"))?;
                object.insert(
                    member.name.clone(),
                    json_value(
                        reader,
                        &member.datatype,
                        raw.get(start..end)
                            .ok_or_else(|| error("compound field outside record"))?,
                        depth + 1,
                    )?,
                );
            }
            serde_json::to_string(&object).map_err(error)
        }
        _ => Ok(hex(raw)),
    }
}
fn reshape(
    values: &mut std::vec::IntoIter<serde_json::Value>,
    dimensions: &[u32],
) -> AppResult<serde_json::Value> {
    if let Some((n, rest)) = dimensions.split_first() {
        Ok(serde_json::Value::Array(
            (0..*n)
                .map(|_| reshape(values, rest))
                .collect::<AppResult<Vec<_>>>()?,
        ))
    } else {
        values.next().ok_or_else(|| error("truncated fixed array"))
    }
}

trait NullTrim {
    fn trim_ascii_end_null(&self) -> &[u8];
}
impl NullTrim for [u8] {
    fn trim_ascii_end_null(&self) -> &[u8] {
        let end = self.iter().rposition(|b| *b != 0).map_or(0, |n| n + 1);
        &self[..end]
    }
}
fn json_value(r: &Reader, t: &Ty, b: &[u8], d: usize) -> AppResult<serde_json::Value> {
    let v = value(r, t, b, d)?;
    match sql_type(t).as_str() {
        "BIGINT" | "UBIGINT" | "HUGEINT" | "UHUGEINT" | "BOOLEAN" => json_number(&v),
        "DOUBLE" => json_number(&v).or(Ok(serde_json::Value::String(v))),
        sql::JSON_VALUE => serde_json::from_str(&v).map_err(error),
        "BLOB" => Ok(serde_json::json!({"$binary_hex":v,"byteCount":b.len().to_string()})),
        _ => Ok(serde_json::Value::String(v)),
    }
}
fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 15) as usize] as char);
    }
    out
}
fn string_attr(attrs: &HashMap<String, AttrValue>, name: &str) -> Option<String> {
    match attrs.get(name)? {
        AttrValue::String(s) | AttrValue::VarLenString(s) | AttrValue::AsciiString(s) => {
            Some(s.clone())
        }
        _ => None,
    }
}
fn logical_type(ty: &str) -> Option<String> {
    if matches!(
        ty,
        "VARCHAR"
            | "BLOB"
            | "BOOLEAN"
            | "BIGINT"
            | "UBIGINT"
            | "HUGEINT"
            | "UHUGEINT"
            | "DOUBLE"
            | "FLOAT"
            | "DATE"
            | "TIME"
            | "TIMESTAMP"
            | "TIMESTAMP_S"
            | "TIMESTAMP_MS"
            | "TIMESTAMP_NS"
            | "TIMESTAMP WITH TIME ZONE"
            | sql::JSON_NUMBER
            | sql::JSON_VALUE
    ) {
        return Some(ty.into());
    }
    if let Some(body) = ty
        .strip_prefix("DECIMAL(")
        .and_then(|s| s.strip_suffix(')'))
    {
        let parts = body
            .split(',')
            .map(|s| s.parse::<u8>())
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        if parts.len() == 2 && (1..=38).contains(&parts[0]) && parts[1] <= parts[0] {
            return Some(format!("DECIMAL({},{})", parts[0], parts[1]));
        }
    }
    None
}

fn unhex(value: &str) -> AppResult<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        return Err(error("invalid hex-encoded HDF5 value"));
    }
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| {
            let digit = |v: u8| -> AppResult<u8> {
                match v {
                    b'0'..=b'9' => Ok(v - b'0'),
                    b'a'..=b'f' => Ok(v - b'a' + 10),
                    b'A'..=b'F' => Ok(v - b'A' + 10),
                    _ => Err(error("invalid hex-encoded HDF5 value")),
                }
            };
            Ok(digit(b[0])? * 16 + digit(b[1])?)
        })
        .collect()
}
fn validate_hint(ty: &str, value: &str) -> AppResult<()> {
    if ty == "BOOLEAN" && !matches!(value, "0" | "1" | "true" | "false") {
        return Err(error("invalid HDF5 boolean value"));
    }
    if ty == "BLOB" {
        unhex(value)?;
    }
    if ty == sql::JSON_NUMBER && !sql::is_json_number(value) {
        return Err(error("invalid exact JSON_NUMBER annotation"));
    }
    if ty == sql::JSON_VALUE {
        serde_json::from_str::<serde_json::Value>(value).map_err(error)?;
    }
    if let Some(body) = ty
        .strip_prefix("DECIMAL(")
        .and_then(|s| s.strip_suffix(')'))
    {
        let parts = body
            .split(',')
            .map(|s| s.parse::<usize>().map_err(error))
            .collect::<AppResult<Vec<_>>>()?;
        let v = value
            .strip_prefix('-')
            .or_else(|| value.strip_prefix('+'))
            .unwrap_or(value);
        let mut digits = v.split('.');
        let whole = digits.next().unwrap_or("");
        let fraction = digits.next().unwrap_or("");
        if parts.len() != 2
            || digits.next().is_some()
            || whole.is_empty()
            || !whole
                .bytes()
                .chain(fraction.bytes())
                .all(|b| b.is_ascii_digit())
            || whole.trim_start_matches('0').len() > parts[0] - parts[1]
            || fraction.trim_end_matches('0').len() > parts[1]
        {
            return Err(error(
                "decimal annotation would round or overflow an HDF5 value",
            ));
        }
    }
    if ty.starts_with("TIMESTAMP") || ty.starts_with("TIME") {
        let precision = if ty.ends_with("_NS") {
            9
        } else if ty.ends_with("_MS") {
            3
        } else if ty.ends_with("_S") {
            0
        } else {
            6
        };
        if value.split_once('.').is_some_and(|(_, rest)| {
            rest.bytes()
                .take_while(u8::is_ascii_digit)
                .enumerate()
                .any(|(i, b)| i >= precision && b != b'0')
        }) {
            return Err(error("temporal annotation would truncate an HDF5 value"));
        }
    }
    Ok(())
}

struct Column {
    key: String,
    name: String,
    ty: String,
    dataset: Dataset,
    datatype: Ty,
    member: Option<(usize, usize)>,
    validity: Option<Vec<u8>>,
    hex_text: bool,
    index: Option<usize>,
}
fn columns(
    reader: &Reader,
    options: &Hdf5Options,
    policy: &ResourcePolicy,
) -> AppResult<(Vec<Column>, Vec<u64>, Hdf5Info)> {
    if options.columns {
        let info = inspect_reader(reader, &options.object, policy)?;
        if !info.columns_selectable {
            return Err(AppError::invalid(
                "Choose a group of equally sized one-dimensional datasets",
            ));
        }
        let g = reader.file.group(&options.object).map_err(error)?;
        let marker = matches!(
            g.attrs().map_err(error)?.get("sivro.table"),
            Some(AttrValue::I64(1))
        );
        let mut out = vec![];
        let mut shape = vec![];
        for o in info.objects.iter().filter(|o| o.kind == "dataset") {
            let ds = reader.file.dataset(&o.path).map_err(error)?;
            shape = ds.shape().map_err(error)?;
            let dt = ds.datatype().map_err(error)?;
            let attrs = ds.attrs().map_err(error)?;
            let ty = if marker {
                string_attr(&attrs, "sivro.type")
                    .and_then(|t| logical_type(&t))
                    .unwrap_or_else(|| sql_type(&dt))
            } else {
                sql_type(&dt)
            };
            if marker
                && string_attr(&attrs, "sivro.type")
                    .and_then(|t| logical_type(&t))
                    .is_some()
            {
                let string = matches!(
                    dt,
                    Ty::String { .. }
                        | Ty::VariableLength {
                            is_string: true,
                            ..
                        }
                );
                let compatible = match ty.as_str() {
                    "BIGINT" | "UBIGINT" | "HUGEINT" | "UHUGEINT" => {
                        matches!(dt, Ty::FixedPoint { .. })
                    }
                    "FLOAT" | "DOUBLE" => standard_float(&dt),
                    "BOOLEAN" => matches!(dt, Ty::FixedPoint { .. } | Ty::Enumeration { .. }),
                    _ => string,
                };
                if !compatible {
                    return Err(error(
                        "HDF5 type annotation disagrees with its physical storage",
                    ));
                }
            }
            let validity = if marker {
                if let Some(path) = string_attr(&attrs, "sivro.validity") {
                    path_ok(&path)?;
                    let mask = reader.file.dataset(&path).map_err(error)?;
                    check_dataset(&mask, policy)?;
                    if mask.shape().map_err(error)? != shape
                        || mask.datatype().map_err(error)? != hdf5_pure::make_u8_type()
                    {
                        return Err(error("invalid HDF5 validity mask"));
                    }
                    if shape[0] > policy.hdf5_cells as u64 {
                        return Err(AppError::limit(
                            "HDF5 validity mask exceeds the cell budget",
                        ));
                    }
                    let mask = mask.read_u8().map_err(error)?;
                    if mask.iter().any(|b| *b > 1) {
                        return Err(error("invalid HDF5 validity byte"));
                    }
                    Some(mask)
                } else {
                    None
                }
            } else {
                None
            };
            out.push(Column {
                key: o.path.clone(),
                name: if marker {
                    string_attr(&attrs, "sivro.name").unwrap_or_else(|| o.name.clone())
                } else {
                    o.name.clone()
                },
                ty,
                dataset: ds,
                datatype: dt,
                member: None,
                hex_text: marker
                    && string_attr(&attrs, "sivro.encoding").as_deref() == Some("utf8_hex"),
                validity,
                index: None,
            });
        }
        Ok((out, shape, info))
    } else {
        let ds = reader.file.dataset(&options.object).map_err(error)?;
        check_dataset(&ds, policy)?;
        let shape = ds.shape().map_err(error)?;
        let dt = ds.datatype().map_err(error)?;
        let parent = options
            .object
            .rsplit_once('/')
            .map(|(p, _)| if p.is_empty() { "/" } else { p })
            .unwrap_or("/");
        let info = Hdf5Info {
            group: parent.into(),
            attributes: vec![],
            objects: vec![object_info(
                options.object.clone(),
                options.object.rsplit('/').next().unwrap_or("Data").into(),
                &ds,
                policy,
            )?],
            columns_selectable: false,
            warnings: vec![],
        };
        let mut out = vec![];
        if let Ty::Compound { members, .. } = &dt {
            if options.column_axis.is_some() {
                return Err(AppError::invalid(
                    "Compound records use their named members as columns; choose no column axis",
                ));
            }
            for m in members {
                out.push(Column {
                    key: options.object.clone(),
                    name: m.name.clone(),
                    ty: sql_type(&m.datatype),
                    dataset: ds.clone(),
                    datatype: m.datatype.clone(),
                    member: Some((m.byte_offset as usize, m.datatype.type_size() as usize)),
                    hex_text: false,
                    validity: None,
                    index: None,
                });
            }
        } else {
            let count = options
                .column_axis
                .map(|axis| {
                    shape
                        .get(axis)
                        .copied()
                        .ok_or_else(|| AppError::invalid("Column axis is outside the dataset"))
                })
                .transpose()?
                .unwrap_or(1);
            if count > policy.max_columns as u64 {
                return Err(AppError::limit(
                    "HDF5 column axis exceeds the column budget",
                ));
            }
            for i in 0..count {
                out.push(Column {
                    key: options.object.clone(),
                    name: if options.column_axis.is_some() {
                        format!("[{i}]")
                    } else {
                        "value".into()
                    },
                    ty: sql_type(&dt),
                    dataset: ds.clone(),
                    datatype: dt.clone(),
                    member: None,
                    hex_text: false,
                    validity: None,
                    index: options.column_axis.map(|_| i as usize),
                });
            }
        }
        Ok((out, shape, info))
    }
}

pub fn open(
    path: &Path,
    options: &Hdf5Options,
    object_id: String,
    scratch: SessionScratch,
    policy: &ResourcePolicy,
) -> AppResult<DuckDbAdapter> {
    guarded(|| {
        path_ok(&options.object)?;
        let fingerprint = Arc::new(Fingerprint::read(path)?);
        let reader = Reader::open(path, policy)?;
        let (cols, shape, info) = columns(&reader, options, policy)?;
        if cols.is_empty() || cols.len() > policy.max_columns {
            return Err(AppError::limit(
                "HDF5 selection has no columns or exceeds the column budget",
            ));
        }
        if options.row_limit == Some(0) {
            return Err(AppError::invalid("Choose a positive HDF5 row limit"));
        }
        let rank = shape.len();
        let row_axis = options.row_axis;
        if rank > 0 && (row_axis >= rank || options.column_axis == Some(row_axis)) {
            return Err(AppError::invalid(
                "Row and column axes must be distinct and inside the dataset",
            ));
        }
        if options.columns && (row_axis != 0 || options.column_axis.is_some()) {
            return Err(AppError::invalid(
                "Column groups use axis 0 for rows and named datasets for columns",
            ));
        }
        if !options.indices.is_empty() && options.indices.len() != rank {
            return Err(AppError::invalid(
                "Provide one fixed index per HDF5 dimension",
            ));
        }
        let mut indices = vec![0u64; rank];
        for (i, index) in options.indices.iter().enumerate() {
            indices[i] = index.parse().map_err(|_| {
                AppError::invalid("Slice indices must be nonnegative exact integers")
            })?;
        }
        for (i, size) in shape.iter().enumerate() {
            if i != row_axis && Some(i) != options.column_axis && indices[i] >= *size {
                return Err(AppError::invalid("A slice index is outside the dataset"));
            }
        }
        let start = if options.row_start.is_empty() {
            0
        } else {
            options
                .row_start
                .parse::<u64>()
                .map_err(|_| AppError::invalid("Row start must be a nonnegative integer"))?
        };
        let n = shape.get(row_axis).copied().unwrap_or(1);
        if start > n {
            return Err(AppError::invalid("Row start is outside the dataset"));
        }
        let rows = options.row_limit.unwrap_or(n - start).min(n - start);
        if rows
            .checked_mul(cols.len() as u64)
            .is_none_or(|n| n > policy.hdf5_cells as u64)
        {
            return Err(AppError::limit(
                "HDF5 slice exceeds the cell budget; choose a smaller row window",
            ));
        }
        let conn = connection(&scratch, policy)?;
        let mut count = 0u64;
        let types = cols.iter().map(|c| c.ty.clone()).collect::<Vec<_>>();
        with_deadline(&conn, policy, || {
            let mut app = stage::create(&conn, &scratch, cols.len())?;
            let mut cached: HashMap<String, (u64, Vec<u8>)> = HashMap::new();
            let mut payload = 0u64;
            for row in start..start + rows {
                reader.source.check(0).map_err(error)?;
                if rank > 0 {
                    indices[row_axis] = row;
                }
                let leading = indices.first().copied().unwrap_or(0);
                let mut values = Vec::with_capacity(cols.len());
                let mut null_scalar = false;
                for c in &cols {
                    if c.validity
                        .as_ref()
                        .is_some_and(|m| m.get(row as usize) == Some(&0))
                    {
                        values.push(None);
                        continue;
                    }
                    let key = c.key.clone();
                    if cached.get(&key).is_none_or(|(r, _)| *r != leading) {
                        let bytes = c.dataset.read_raw_rows(leading, 1).map_err(error)?;
                        cached.insert(key.clone(), (leading, bytes));
                    }
                    let raw = &cached[&key].1;
                    if rank == 0 && raw.is_empty() {
                        null_scalar = true;
                        break;
                    }
                    let mut element = 0u64;
                    for i in 1..rank {
                        let index = if Some(i) == options.column_axis {
                            c.index.unwrap_or(0) as u64
                        } else {
                            indices[i]
                        };
                        element = element
                            .checked_mul(shape[i])
                            .and_then(|v| v.checked_add(index))
                            .ok_or_else(|| error("slice offset overflow"))?;
                    }
                    let record_size = c.dataset.element_size().map_err(error)? as usize;
                    let offset = usize::try_from(element)
                        .map_err(error)?
                        .checked_mul(record_size)
                        .ok_or_else(|| error("record offset overflow"))?;
                    // When axis 0 supplies columns, each column lives in a different leading row.
                    let owned = if options.column_axis == Some(0) {
                        Some(
                            c.dataset
                                .read_raw_rows(c.index.unwrap_or(0) as u64, 1)
                                .map_err(error)?,
                        )
                    } else {
                        None
                    };
                    let raw = owned.as_deref().unwrap_or(raw);
                    let record = raw
                        .get(offset..offset + record_size)
                        .ok_or_else(|| error("dataset row is shorter than its shape"))?;
                    let field = if let Some((offset, size)) = c.member {
                        record
                            .get(offset..offset + size)
                            .ok_or_else(|| error("compound field outside record"))?
                    } else {
                        record
                    };
                    let mut v = value(&reader, &c.datatype, field, 0)?;
                    if c.hex_text {
                        v = String::from_utf8(unhex(&v)?).map_err(error)?;
                    }
                    validate_hint(&c.ty, &v)?;
                    payload = payload.saturating_add(v.len() as u64);
                    if payload > policy.scratch_bytes / 2 {
                        return Err(AppError::limit(
                            "Decoded HDF5 values exceed the staging budget",
                        ));
                    }
                    values.push(Some(v));
                }
                if null_scalar {
                    break;
                }
                stage::append(&mut app, count as i64, None, values)?;
                count += 1;
                if count.is_multiple_of(256) {
                    stage::check_size(&scratch, policy)?;
                    free_space(&scratch.path, policy, 0)?;
                }
            }
            app.flush().map_err(AppError::query)?;
            drop(app);
            stage::check_size(&scratch, policy)?;
            for (i, ty) in types
                .iter()
                .enumerate()
                .filter(|(_, t)| t.as_str() == "DATE" || t.starts_with("TIME"))
            {
                let field = format!("field_{i}");
                let changed: bool = conn.query_row(&format!("SELECT EXISTS(SELECT 1 FROM sivro_stage.records WHERE {field} IS NOT NULL AND CAST({field} AS {ty})::VARCHAR <> {field})"), [], |r| r.get(0)).map_err(AppError::query)?;
                if changed {
                    return Err(error("temporal annotation would change an HDF5 value"));
                }
            }
            let projection = types
                .iter()
                .enumerate()
                .map(|(i, ty)| {
                    let name = format!("field_{i}");
                    if ty == "BLOB" {
                        format!("from_hex({name}) AS {name}")
                    } else if matches!(ty.as_str(), "VARCHAR" | sql::JSON_VALUE | sql::JSON_NUMBER)
                    {
                        name
                    } else {
                        format!("CAST({name} AS {ty}) AS {name}")
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            conn.execute_batch(&format!("CREATE TEMP VIEW {} AS SELECT {projection}, ordinal AS {} FROM sivro_stage.records",sql::VIEW,sql::ORDINAL)).map_err(AppError::query)?;
            Ok(())
        })?;
        fingerprint.check(path)?;
        let columns = cols
            .iter()
            .enumerate()
            .map(|(i, c)| stage::column(i, c.name.clone(), c.ty.clone(), c.validity.is_some()))
            .collect();
        let warnings=vec!["HDF5 values are staged from the selected axes, fixed indices and row window; exports do not reproduce the source hierarchy.".into(),"Enums retain their numeric values (FALSE/TRUE enums become booleans); nested records/arrays use exact JSON text, opaque values and references use bytes. Links and references are never followed.".into(),format!("Selected source rows {start}..{} on axis {row_axis}; {count} rows staged.",start+rows)];
        Ok(DuckDbAdapter {
            arrow: None,
            hdf5: Some(Hdf5Selection {
                options: options.clone(),
                info,
            }),
            format: FormatKind::Hdf5,
            delimited: None,
            excel: None,
            conn,
            path: path.into(),
            fingerprint,
            object: ObjectDescriptor {
                id: object_id,
                label: options.object.clone(),
                kind: ObjectKind::Table,
                columns,
                row_count: CountState::known(count),
                capabilities: Capabilities {
                    page: true,
                    filter: true,
                    sort: true,
                    profile: true,
                    exact_count: true,
                    copy: true,
                    exports: [
                        "parquet",
                        "arrow",
                        "arrow_stream",
                        "hdf5",
                        "csv",
                        "tsv",
                        "json",
                        "jsonl",
                    ]
                    .map(String::from)
                    .to_vec(),
                },
                warnings,
            },
            row_count: Some(count),
            row_groups: vec![],
            created_by: None,
            format_version: 0,
            counts: HashMap::new(),
            profiles: HashMap::new(),
            raw_records: false,
            _scratch: scratch,
        })
    })
}
