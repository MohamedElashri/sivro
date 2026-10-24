//! JSON documents and JSON/JSONL record tables. serde_json with
//! `arbitrary_precision` keeps numeric tokens exact. Records are staged as text
//! in session scratch, so DuckDB never parses (and rounds) a JSON number;
//! exact SQL types are applied only where every value fits them.
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
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use std::{
    cell::{Cell as Flag, RefCell},
    collections::HashMap,
    fmt,
    fs::File,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

/// serde_json reports `arbitrary_precision` numbers as a one-entry map with
/// this key. Like serde_json's own Value, an object whose only key is this
/// literal and whose value is a number-shaped string is indistinguishable.
const NUMBER_TOKEN: &str = "$serde_json::private::Number";

/// Ordered JSON tree: keeps source key order, duplicate keys and exact
/// number tokens. String escapes are decoded and re-encoded on output.
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<Node>),
    Object(Vec<(String, Node)>),
}

impl Node {
    pub fn kind(&self) -> NodeKind {
        match self {
            Self::Null => NodeKind::Null,
            Self::Bool(_) => NodeKind::Boolean,
            Self::Number(_) => NodeKind::Number,
            Self::String(_) => NodeKind::String,
            Self::Array(_) => NodeKind::Array,
            Self::Object(_) => NodeKind::Object,
        }
    }
    fn len(&self) -> Option<usize> {
        match self {
            Self::Array(items) => Some(items.len()),
            Self::Object(pairs) => Some(pairs.len()),
            _ => None,
        }
    }
    fn child(&self, index: u64) -> Option<(Option<&str>, &Node)> {
        let index = usize::try_from(index).ok()?;
        match self {
            Self::Array(items) => items.get(index).map(|n| (None, n)),
            Self::Object(pairs) => pairs.get(index).map(|(k, n)| (Some(k.as_str()), n)),
            _ => None,
        }
    }
}

/// Source bytes per tree node allowed by the value budget. A node costs
/// roughly 32-64 bytes in memory, so a byte limit alone does not bound a
/// tree of tiny values (`[0,0,...]`); the budget keeps it near the source size.
const BYTES_PER_NODE: u64 = 32;

#[derive(Clone, Copy)]
struct NodeSeed<'a> {
    depth: usize,
    max: usize,
    nodes: &'a Flag<u64>,
}
impl<'a> NodeSeed<'a> {
    fn root(max: usize, nodes: &'a Flag<u64>) -> Self {
        Self {
            depth: 0,
            max,
            nodes,
        }
    }
    fn count<E: de::Error>(self) -> Result<(), E> {
        match self.nodes.get().checked_sub(1) {
            Some(left) => {
                self.nodes.set(left);
                Ok(())
            }
            None => Err(E::custom("too many values for the document size budget")),
        }
    }
    fn enter<E: de::Error>(self) -> Result<Self, E> {
        if self.depth >= self.max {
            return Err(E::custom(format!(
                "nesting exceeds the depth limit of {}",
                self.max
            )));
        }
        Ok(Self {
            depth: self.depth + 1,
            ..self
        })
    }
}
impl<'de> DeserializeSeed<'de> for NodeSeed<'_> {
    type Value = Node;
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Node, D::Error> {
        d.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for NodeSeed<'_> {
    type Value = Node;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a JSON value")
    }
    fn visit_bool<E: de::Error>(self, v: bool) -> Result<Node, E> {
        self.count()?;
        Ok(Node::Bool(v))
    }
    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Node, E> {
        self.count()?;
        Ok(Node::Number(v.to_string()))
    }
    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Node, E> {
        self.count()?;
        Ok(Node::Number(v.to_string()))
    }
    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Node, E> {
        // Only reachable without arbitrary_precision, which would have rounded.
        Err(E::custom("a JSON number would lose precision"))
    }
    fn visit_str<E: de::Error>(self, v: &str) -> Result<Node, E> {
        self.count()?;
        Ok(Node::String(v.to_owned()))
    }
    fn visit_string<E: de::Error>(self, v: String) -> Result<Node, E> {
        self.count()?;
        Ok(Node::String(v))
    }
    fn visit_unit<E: de::Error>(self) -> Result<Node, E> {
        self.count()?;
        Ok(Node::Null)
    }
    fn visit_none<E: de::Error>(self) -> Result<Node, E> {
        self.visit_unit()
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Node, A::Error> {
        self.count()?;
        let child = self.enter()?;
        let mut items = Vec::new();
        while let Some(item) = seq.next_element_seed(child)? {
            items.push(item);
        }
        Ok(Node::Array(items))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Node, A::Error> {
        let Some(first) = map.next_key::<String>()? else {
            self.count()?;
            self.enter()?;
            return Ok(Node::Object(vec![]));
        };
        let mut pairs = Vec::new();
        if first == NUMBER_TOKEN {
            // arbitrary_precision numbers arrive as this one-entry map; the
            // token is a leaf at this depth, not a nested object.
            let value = map.next_value_seed(self)?;
            match map.next_key::<String>()? {
                None => {
                    if let Node::String(token) = &value {
                        if sql::is_json_number(token) {
                            return Ok(Node::Number(token.clone()));
                        }
                    }
                    self.count()?;
                    self.enter()?;
                    return Ok(Node::Object(vec![(first, value)]));
                }
                Some(key) => {
                    pairs.push((first, value));
                    self.count()?;
                    let child = self.enter()?;
                    let value = map.next_value_seed(child)?;
                    pairs.push((key, value));
                }
            }
        } else {
            self.count()?;
            let child = self.enter()?;
            let value = map.next_value_seed(child)?;
            pairs.push((first, value));
        }
        let child = self.enter()?;
        while let Some(key) = map.next_key::<String>()? {
            let value = map.next_value_seed(child)?;
            pairs.push((key, value));
        }
        Ok(Node::Object(pairs))
    }
}

/// Serializes with exact tokens, refusing output beyond `limit` bytes.
fn write_json(
    node: &Node,
    out: &mut Vec<u8>,
    indent: Option<usize>,
    limit: usize,
) -> Result<(), ()> {
    fn newline(out: &mut Vec<u8>, indent: Option<usize>) {
        if let Some(level) = indent {
            out.push(b'\n');
            out.extend(std::iter::repeat_n(b' ', level * 2));
        }
    }
    fn string(out: &mut Vec<u8>, value: &str) -> Result<(), ()> {
        serde_json::to_writer(&mut *out, value).map_err(|_| ())
    }
    match node {
        Node::Null => out.extend_from_slice(b"null"),
        Node::Bool(v) => out.extend_from_slice(if *v { b"true" } else { b"false" }),
        Node::Number(token) => out.extend_from_slice(token.as_bytes()),
        Node::String(value) => string(out, value)?,
        Node::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                newline(out, indent.map(|l| l + 1));
                write_json(item, out, indent.map(|l| l + 1), limit)?;
            }
            if !items.is_empty() {
                newline(out, indent);
            }
            out.push(b']');
        }
        Node::Object(pairs) => {
            out.push(b'{');
            for (i, (key, value)) in pairs.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                newline(out, indent.map(|l| l + 1));
                string(out, key)?;
                out.extend_from_slice(if indent.is_some() { b": " } else { b":" });
                write_json(value, out, indent.map(|l| l + 1), limit)?;
            }
            if !pairs.is_empty() {
                newline(out, indent);
            }
            out.push(b'}');
        }
    }
    if out.len() > limit {
        return Err(());
    }
    Ok(())
}

fn compact(node: &Node) -> String {
    let mut out = Vec::new();
    // Records are already bounded by the per-record byte limit.
    let _ = write_json(node, &mut out, None, usize::MAX);
    String::from_utf8(out).unwrap_or_default()
}

fn strip_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes)
}
fn is_json_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n')
}
fn trim_json_space(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|b| !is_json_space(*b))
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|b| !is_json_space(*b))
        .map_or(start, |i| i + 1);
    &bytes[start..end]
}

fn parse_error(context: &str, error: serde_json::Error) -> AppError {
    if error.to_string().contains("document size budget") {
        return AppError::limit(format!(
            "{context}: {error}. Very many small values need far more memory than their file size; open a smaller document."
        ));
    }
    AppError::invalid(format!(
        "{context}: {error}. No malformed JSON is skipped; fix the source or choose another file."
    ))
}

// ---------------------------------------------------------------------------
// Exact type inference over every record
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Field {
    key: String,
    present: u64,
    nulls: bool,
    bools: bool,
    strings: bool,
    nested: bool,
    numbers: bool,
    raw_number: bool,
    fraction: bool,
    wide: bool,
    whole: usize,
    scale: usize,
}
impl Field {
    fn observe(&mut self, node: &Node) {
        self.present += 1;
        match node {
            Node::Null => self.nulls = true,
            Node::Bool(_) => self.bools = true,
            Node::String(_) => self.strings = true,
            Node::Array(_) | Node::Object(_) => self.nested = true,
            Node::Number(token) => {
                self.numbers = true;
                let unsigned = token.strip_prefix('-').unwrap_or(token);
                if unsigned.contains(['e', 'E']) {
                    self.raw_number = true;
                    return;
                }
                let (whole, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
                self.whole = self.whole.max(whole.trim_start_matches('0').len());
                if fraction.is_empty() {
                    if token.parse::<i64>().is_err() {
                        self.wide = true;
                        if token.parse::<i128>().is_err() {
                            self.raw_number = true;
                        }
                    }
                } else {
                    self.fraction = true;
                    self.scale = self.scale.max(fraction.len());
                }
            }
        }
    }
    /// Exact SQL types only; anything else keeps exact source text.
    fn data_type(&self) -> String {
        let kinds = usize::from(self.bools) + usize::from(self.strings) + usize::from(self.numbers);
        if self.nested || kinds > 1 {
            return sql::JSON_VALUE.into();
        }
        if self.bools {
            return "BOOLEAN".into();
        }
        if !self.numbers {
            return "VARCHAR".into();
        }
        if self.raw_number {
            return sql::JSON_NUMBER.into();
        }
        if !self.fraction {
            return if self.wide { "HUGEINT" } else { "BIGINT" }.into();
        }
        // DuckDB prints DECIMAL(s,s) without a leading zero (".5"), which
        // is not a JSON number; always reserve one integer digit.
        let precision = self.whole.max(1) + self.scale;
        if precision > 38 {
            sql::JSON_NUMBER.into()
        } else {
            format!("DECIMAL({precision},{})", self.scale)
        }
    }
}

#[derive(Default)]
struct Schema {
    fields: Vec<Field>,
    /// Key -> field index for each repetition of that key within one record.
    index: HashMap<String, Vec<usize>>,
    objects: Option<bool>,
    records: u64,
    overflow: bool,
    duplicates: bool,
}
impl Schema {
    /// Returns an error message when record kinds mix objects and values.
    fn observe(&mut self, node: &Node, max_columns: usize) -> Result<(), &'static str> {
        let object = matches!(node, Node::Object(_));
        if *self.objects.get_or_insert(object) != object {
            return Err(if object {
                "an object, but earlier records are scalar/array values"
            } else {
                "a scalar/array value, but earlier records are objects"
            });
        }
        self.records += 1;
        let Node::Object(pairs) = node else {
            if self.fields.is_empty() {
                self.fields.push(Field {
                    key: "value".into(),
                    ..Field::default()
                });
            }
            self.fields[0].observe(node);
            return Ok(());
        };
        let mut seen: HashMap<&str, usize> = HashMap::new();
        for (key, value) in pairs {
            let occurrence = seen.entry(key.as_str()).or_default();
            let slots = self.index.entry(key.clone()).or_default();
            if *occurrence > 0 {
                self.duplicates = true;
            }
            if slots.len() == *occurrence {
                if self.fields.len() >= max_columns {
                    self.overflow = true;
                    *occurrence += 1;
                    continue;
                }
                slots.push(self.fields.len());
                self.fields.push(Field {
                    key: key.clone(),
                    ..Field::default()
                });
            }
            self.fields[slots[*occurrence]].observe(value);
            *occurrence += 1;
        }
        Ok(())
    }
    fn values(&self, node: &Node, types: &[String]) -> Vec<Option<String>> {
        let text = |node: &Node, ty: &str| match node {
            _ if ty == sql::JSON_VALUE => Some(compact(node)),
            Node::Null => None,
            Node::Bool(v) => Some(v.to_string()),
            Node::Number(token) => Some(token.clone()),
            Node::String(value) => Some(value.clone()),
            other => Some(compact(other)),
        };
        let mut out = vec![None; self.fields.len()];
        match node {
            Node::Object(pairs) => {
                let mut seen: HashMap<&str, usize> = HashMap::new();
                for (key, value) in pairs {
                    let occurrence = seen.entry(key.as_str()).or_default();
                    if let Some(&i) = self.index.get(key).and_then(|s| s.get(*occurrence)) {
                        out[i] = text(value, &types[i]);
                    }
                    *occurrence += 1;
                }
            }
            other => out[0] = text(other, &types[0]),
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Bounded record scanners
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Stop {
    TooLarge,
    Timeout,
}
struct Budget {
    started: Instant,
    limit: Duration,
}
impl Budget {
    fn check(&self) -> AppResult<()> {
        if self.started.elapsed() > self.limit {
            return Err(AppError::new(
                ErrorCode::Timeout,
                "Reading this JSON source exceeded the open time budget. Choose a smaller input.",
            ));
        }
        Ok(())
    }
}

/// Counts bytes since the current record began; refuses an oversized record
/// before serde_json can buffer it all. Granularity is the buffer size.
struct Guarded {
    file: File,
    read: Rc<Flag<u64>>,
    mark: Rc<Flag<u64>>,
    stop: Rc<Flag<Option<Stop>>>,
    record_limit: u64,
    started: Instant,
    limit: Duration,
}
impl Read for Guarded {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.started.elapsed() > self.limit {
            self.stop.set(Some(Stop::Timeout));
            return Err(std::io::Error::other("time budget exceeded"));
        }
        if self.read.get() - self.mark.get() > self.record_limit {
            self.stop.set(Some(Stop::TooLarge));
            return Err(std::io::Error::other("record too large"));
        }
        let n = self.file.read(buffer)?;
        self.read.set(self.read.get() + n as u64);
        Ok(n)
    }
}

enum Scan {
    Complete,
    /// The callback declined a record (it is not an object).
    Stopped,
}

/// Streams the elements of a root JSON array, one bounded element at a time.
fn scan_array(
    path: &Path,
    policy: &ResourcePolicy,
    budget: &Budget,
    on_record: &mut dyn FnMut(u64, Node) -> AppResult<bool>,
) -> AppResult<Scan> {
    let mut file = File::open(path).map_err(AppError::io)?;
    let mut bom = [0u8; 3];
    let n = file.read(&mut bom).map_err(AppError::io)?;
    if bom[..n] != *b"\xEF\xBB\xBF" {
        use std::io::Seek;
        file.seek(std::io::SeekFrom::Start(0))
            .map_err(AppError::io)?;
    }
    let (read, mark, stop) = (
        Rc::new(Flag::new(0)),
        Rc::new(Flag::new(0)),
        Rc::new(Flag::new(None)),
    );
    let reader = BufReader::with_capacity(
        64 << 10,
        Guarded {
            file,
            read: read.clone(),
            mark: mark.clone(),
            stop: stop.clone(),
            record_limit: policy.jsonl_record_bytes,
            started: budget.started,
            limit: budget.limit,
        },
    );
    let failure: RefCell<Option<AppError>> = RefCell::new(None);
    let stopped = Flag::new(false);
    let nodes = Flag::new(0);
    struct Elements<'a> {
        seed: NodeSeed<'a>,
        per_record: u64,
        mark: &'a Flag<u64>,
        read: &'a Flag<u64>,
        on_record: &'a mut dyn FnMut(u64, Node) -> AppResult<bool>,
        failure: &'a RefCell<Option<AppError>>,
        stopped: &'a Flag<bool>,
    }
    impl<'de> Visitor<'de> for Elements<'_> {
        type Value = ();
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a JSON array")
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
            let child = self.seed.enter()?;
            let mut index = 0u64;
            loop {
                self.mark.set(self.read.get());
                self.seed.nodes.set(self.per_record);
                let Some(node) = seq.next_element_seed(child)? else {
                    return Ok(());
                };
                match (self.on_record)(index, node) {
                    Ok(true) => index += 1,
                    Ok(false) => {
                        self.stopped.set(true);
                        return Err(de::Error::custom("stopped"));
                    }
                    Err(error) => {
                        *self.failure.borrow_mut() = Some(error);
                        return Err(de::Error::custom("stopped"));
                    }
                }
            }
        }
    }
    let mut de = serde_json::Deserializer::from_reader(reader);
    let result = de::Deserializer::deserialize_seq(
        &mut de,
        Elements {
            seed: NodeSeed::root(policy.json_depth, &nodes),
            per_record: policy.jsonl_record_bytes / BYTES_PER_NODE,
            mark: &mark,
            read: &read,
            on_record,
            failure: &failure,
            stopped: &stopped,
        },
    )
    .and_then(|_| de.end());
    if let Some(error) = failure.into_inner() {
        return Err(error);
    }
    if stopped.get() {
        return Ok(Scan::Stopped);
    }
    match (result, stop.get()) {
        (Ok(()), _) => Ok(Scan::Complete),
        (Err(_), Some(Stop::Timeout)) => Err(AppError::new(
            ErrorCode::Timeout,
            "Reading this JSON source exceeded the open time budget. Choose a smaller input.",
        )),
        (Err(_), Some(Stop::TooLarge)) => Err(AppError::limit(format!(
            "An array element exceeds the {} MiB record limit",
            policy.jsonl_record_bytes >> 20
        ))),
        (Err(error), None) if error.is_syntax() && error.to_string().contains("trailing") => {
            Err(AppError::new(ErrorCode::FormatMismatch, format!("This .json file contains more than one top-level JSON value ({error}). If it is JSON Lines, reopen it as JSON Lines.")))
        }
        (Err(error), None) => Err(parse_error("Invalid JSON", error)),
    }
}

/// Streams JSON Lines: blank lines are skipped, every other physical line
/// must hold exactly one complete JSON value.
fn scan_lines(
    path: &Path,
    policy: &ResourcePolicy,
    budget: &Budget,
    on_record: &mut dyn FnMut(u64, Node, &str) -> AppResult<()>,
) -> AppResult<()> {
    let mut reader = BufReader::with_capacity(1 << 20, File::open(path).map_err(AppError::io)?);
    let limit = policy.jsonl_record_bytes;
    let mut buffer = Vec::new();
    let mut line = 0u64;
    let mut first_record = true;
    loop {
        buffer.clear();
        let n = (&mut reader)
            .take(limit + 2)
            .read_until(b'\n', &mut buffer)
            .map_err(AppError::io)?;
        if n == 0 {
            return Ok(());
        }
        line += 1;
        if line.is_multiple_of(4096) {
            budget.check()?;
        }
        let complete = buffer.last() == Some(&b'\n');
        let mut content = buffer.as_slice();
        if line == 1 {
            content = strip_bom(content);
        }
        let content = trim_json_space(content);
        if (!complete && n as u64 > limit) || content.len() as u64 > limit {
            return Err(AppError::limit(format!(
                "Line {line} exceeds the {} MiB JSON Lines record limit",
                limit >> 20
            )));
        }
        if content.is_empty() {
            continue;
        }
        let mut de = serde_json::Deserializer::from_slice(content);
        let nodes = Flag::new(limit / BYTES_PER_NODE);
        let node = match NodeSeed::root(policy.json_depth, &nodes).deserialize(&mut de) {
            Ok(node) => node,
            Err(error) if error.is_eof() && first_record => {
                return Err(AppError::new(ErrorCode::FormatMismatch, format!("Line {line} is an incomplete JSON value. This looks like a multi-line JSON document; reopen it as JSON. No line was skipped.")));
            }
            Err(error) => return Err(parse_error(&format!("Line {line}"), error)),
        };
        if de.end().is_err() {
            return Err(AppError::invalid(format!(
                "Line {line} contains more than one JSON value; JSON Lines requires exactly one value per line"
            )));
        }
        first_record = false;
        let raw = std::str::from_utf8(content)
            .map_err(|_| AppError::invalid(format!("Line {line} is not valid UTF-8")))?;
        on_record(line, node, raw)?;
    }
}

/// The first significant byte decides only which validator runs; each one
/// still reads and validates the entire input.
fn first_byte(path: &Path) -> AppResult<Option<u8>> {
    let mut reader = BufReader::new(File::open(path).map_err(AppError::io)?);
    let mut first = true;
    loop {
        let buffer = reader.fill_buf().map_err(AppError::io)?;
        if buffer.is_empty() {
            return Ok(None);
        }
        let chunk = if first { strip_bom(buffer) } else { buffer };
        first = false;
        if let Some(b) = chunk.iter().copied().find(|b| !is_json_space(*b)) {
            return Ok(Some(b));
        }
        let n = buffer.len();
        reader.consume(n);
    }
}

// ---------------------------------------------------------------------------
// Opening
// ---------------------------------------------------------------------------

enum Source {
    Array,
    Lines,
}

pub fn open(
    path: &Path,
    format: FormatKind,
    object_id: String,
    scratch: SessionScratch,
    policy: &ResourcePolicy,
) -> AppResult<super::Adapter> {
    let fingerprint = Arc::new(Fingerprint::read(path)?);
    let budget = Budget {
        started: Instant::now(),
        limit: Duration::from_millis(policy.timeout_ms),
    };
    let max_columns = policy.max_columns;
    let mut schema = Schema::default();
    let source = if format == FormatKind::Jsonl {
        scan_lines(path, policy, &budget, &mut |line, node, _| {
            schema.observe(&node, max_columns).map_err(|problem| {
                AppError::invalid(format!("Line {line} is {problem}. Sivro does not invent a table schema for mixed record kinds."))
            })
        })?;
        Source::Lines
    } else {
        let reason = match first_byte(path)? {
            None => return Err(AppError::invalid("The file contains no JSON value")),
            Some(b'[') => {
                let scan = scan_array(path, policy, &budget, &mut |_, node| {
                    Ok(matches!(node, Node::Object(_))
                        && schema.observe(&node, max_columns).is_ok())
                })?;
                match scan {
                    Scan::Stopped => "The root array contains values that are not objects",
                    Scan::Complete if schema.records == 0 => "The root array is empty",
                    Scan::Complete if schema.fields.is_empty() => {
                        "The root array's objects have no fields"
                    }
                    Scan::Complete => "",
                }
            }
            Some(b'{') => "The root is an object",
            Some(_) => "The root is a single value",
        };
        if !reason.is_empty() {
            return JsonDocument::open(path, fingerprint, format, object_id, reason, policy)
                .map(|d| super::Adapter::Document(Box::new(d)));
        }
        Source::Array
    };
    if schema.records == 0 {
        return Err(AppError::invalid("This JSON Lines file has no records"));
    }
    if schema.fields.is_empty() {
        return Err(AppError::invalid(
            "These records have no fields to show as columns",
        ));
    }
    stage(
        path,
        fingerprint,
        format,
        object_id,
        scratch,
        schema,
        source,
        &budget,
        policy,
    )
    .map(|a| super::Adapter::Table(Box::new(a)))
}

#[allow(clippy::too_many_arguments)]
fn stage(
    path: &Path,
    fingerprint: Arc<Fingerprint>,
    format: FormatKind,
    object_id: String,
    scratch: SessionScratch,
    schema: Schema,
    source: Source,
    budget: &Budget,
    policy: &ResourcePolicy,
) -> AppResult<DuckDbAdapter> {
    // Each record is stored twice (fields and source text).
    let estimate = fingerprint.size.saturating_mul(2);
    if estimate > policy.scratch_bytes {
        return Err(AppError::limit(format!(
            "Record tables are staged in session scratch; this source needs about {} MiB, above the {} MiB budget",
            estimate >> 20,
            policy.scratch_bytes >> 20
        )));
    }
    free_space(&scratch.path, policy, estimate)?;
    let types: Vec<String> = schema.fields.iter().map(Field::data_type).collect();
    let conn = connection(&scratch, policy)?;
    with_deadline(&conn, policy, || {
        let mut appender = stage::create(&conn, &scratch, types.len())?;
        let mut ordinal = 0i64;
        let mut append = |node: &Node, raw: String| -> AppResult<()> {
            stage::append(
                &mut appender,
                ordinal,
                Some(raw),
                schema.values(node, &types),
            )?;
            ordinal += 1;
            if ordinal % 4096 == 0 {
                budget.check()?;
                stage::check_size(&scratch, policy)?;
            }
            Ok(())
        };
        match source {
            Source::Lines => scan_lines(path, policy, budget, &mut |_, node, raw| {
                append(&node, raw.to_owned())
            })?,
            Source::Array => {
                if let Scan::Stopped = scan_array(path, policy, budget, &mut |_, node| {
                    append(&node, compact(&node)).map(|_| true)
                })? {
                    return Err(AppError::internal());
                }
            }
        }
        appender.flush().map_err(AppError::query)?;
        drop(appender);
        if ordinal as u64 != schema.records {
            // The source changed between the inference and staging passes.
            return Err(AppError::new(
                ErrorCode::StaleDataset,
                "The source changed while it was being read. Refresh to reopen it.",
            ));
        }
        stage::create_view(&conn, &types, true)
    })?;
    fingerprint.check(path)?;
    let columns: Vec<ColumnDescriptor> = schema
        .fields
        .iter()
        .zip(types)
        .enumerate()
        .map(|(i, (field, data_type))| {
            stage::column(
                i,
                field.key.clone(),
                data_type,
                field.present < schema.records || field.nulls,
            )
        })
        .collect();
    let mut warnings = vec![format!(
        "All {} records were read when opening; field types come from every record, not a sample. Each source record remains available in the row inspector.",
        schema.records
    )];
    if columns.iter().any(|c| c.data_type == sql::JSON_NUMBER) {
        warnings.push("Some numbers use exponents or more than 38 digits. They are shown as exact source tokens: equality compares exact text, and sorting, ranges and min/max are disabled. Parquet export refuses these columns.".into());
    }
    if columns.iter().any(|c| c.data_type == sql::JSON_VALUE) {
        warnings.push("Nested or mixed-type fields are exact JSON text; they support text matching and NULL checks only.".into());
    }
    if columns.iter().any(|c| c.data_type.starts_with("DECIMAL")) {
        warnings.push("Decimal fields use exact DECIMAL types; values keep their value but may gain trailing zeros. The source record keeps the original spelling.".into());
    }
    if schema
        .fields
        .iter()
        .zip(&columns)
        .any(|(f, c)| c.data_type != sql::JSON_VALUE && f.nulls && f.present < schema.records)
    {
        warnings.push("Missing keys and JSON null both appear as NULL in typed columns; the source record distinguishes them.".into());
    }
    if schema.duplicates {
        warnings.push("Some records repeat a key. Each repetition is a separate column with the same label; exports and row copies use column IDs.".into());
    }
    if schema.overflow {
        warnings.push(format!("Records contain more than {} distinct keys; additional keys appear only in the source record.", policy.max_columns));
    }
    Ok(DuckDbAdapter {
        arrow: None,
        format,
        delimited: None,
        excel: None,
        conn,
        path: path.to_path_buf(),
        fingerprint,
        object: ObjectDescriptor {
            id: object_id,
            label: "Records".into(),
            kind: ObjectKind::Table,
            columns,
            row_count: CountState::known(schema.records),
            capabilities: Capabilities {
                page: true,
                filter: true,
                sort: true,
                profile: true,
                exact_count: true,
                copy: true,
                exports: vec![
                    "parquet".into(),
                    "arrow".into(),
                    "arrow_stream".into(),
                    "csv".into(),
                    "tsv".into(),
                    "json".into(),
                    "jsonl".into(),
                ],
            },
            warnings,
        },
        row_count: Some(schema.records),
        row_groups: vec![],
        created_by: None,
        format_version: 0,
        counts: HashMap::new(),
        profiles: HashMap::new(),
        raw_records: true,
        _scratch: scratch,
    })
}

// ---------------------------------------------------------------------------
// Bounded document viewer
// ---------------------------------------------------------------------------

pub struct JsonDocument {
    pub path: PathBuf,
    pub fingerprint: Arc<Fingerprint>,
    format: FormatKind,
    root: Node,
    object: ObjectDescriptor,
    info: DocumentInfo,
}

impl JsonDocument {
    fn open(
        path: &Path,
        fingerprint: Arc<Fingerprint>,
        format: FormatKind,
        object_id: String,
        reason: &str,
        policy: &ResourcePolicy,
    ) -> AppResult<Self> {
        if fingerprint.size > policy.json_document_bytes {
            return Err(AppError::limit(format!(
                "{reason}, so it needs the document viewer, which accepts at most {} MiB. Only a root array of objects can open as a table at this size.",
                policy.json_document_bytes >> 20
            )));
        }
        let mut bytes = Vec::new();
        File::open(path)
            .map_err(AppError::io)?
            .take(policy.json_document_bytes + 1)
            .read_to_end(&mut bytes)
            .map_err(AppError::io)?;
        if bytes.len() as u64 > policy.json_document_bytes {
            return Err(AppError::new(
                ErrorCode::StaleDataset,
                "The source grew while it was being read. Refresh to reopen it.",
            ));
        }
        let mut de = serde_json::Deserializer::from_slice(strip_bom(&bytes));
        let nodes = Flag::new(policy.json_document_bytes / BYTES_PER_NODE);
        let root = NodeSeed::root(policy.json_depth, &nodes)
            .deserialize(&mut de)
            .map_err(|e| parse_error("Invalid JSON", e))?;
        de.end().map_err(|error| {
            AppError::new(ErrorCode::FormatMismatch, format!("This .json file contains more than one top-level JSON value ({error}). If it is JSON Lines, reopen it as JSON Lines."))
        })?;
        drop(bytes);
        fingerprint.check(path)?;
        let scalar = root.len().is_none();
        let info = DocumentInfo {
            root_kind: root.kind(),
            child_count: root.len().unwrap_or(0).to_string(),
            reason: format!("{reason}; it is shown as a document, not a table."),
        };
        Ok(Self {
            path: path.to_path_buf(),
            fingerprint,
            format,
            root,
            object: ObjectDescriptor {
                id: object_id,
                label: "Document".into(),
                kind: if scalar {
                    ObjectKind::Scalar
                } else {
                    ObjectKind::Document
                },
                columns: vec![],
                row_count: CountState::Unknown,
                capabilities: Capabilities {
                    page: false,
                    filter: false,
                    sort: false,
                    profile: false,
                    exact_count: false,
                    copy: true,
                    exports: vec![],
                },
                warnings: vec![
                    "Numbers are exact source tokens. Object keys keep source order, including repeated keys. Copy uses formatted JSON with the same values.".into(),
                    "Documents are inspected and copied; table export needs a root array of objects or JSON Lines records.".into(),
                ],
            },
            info,
        })
    }

    pub fn descriptor(
        &self,
        session_id: &str,
        generation: u64,
        policy: &ResourcePolicy,
    ) -> DatasetDescriptor {
        DatasetDescriptor {
            api_version: API_VERSION,
            session_id: session_id.into(),
            generation,
            format: self.format,
            delimited: None,
            path: self.path.to_string_lossy().into_owned(),
            file_name: self
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            file_size: self.fingerprint.size.to_string(),
            object: self.object.clone(),
            row_groups: vec![],
            created_by: None,
            format_version: 0,
            policy: policy.clone(),
            document: Some(self.info.clone()),
            excel: None,
            sqlite: None,
            arrow: None,
        }
    }

    pub fn read(
        &self,
        generation: u64,
        query: &DocumentQuery,
        policy: &ResourcePolicy,
    ) -> AppResult<DocumentResult> {
        if query.path.len() > policy.json_depth {
            return Err(AppError::limit("The node path exceeds the depth limit"));
        }
        if query.limit == 0 || query.limit > policy.json_children {
            return Err(AppError::limit(format!(
                "Request between 1 and {} children",
                policy.json_children
            )));
        }
        let mut node = &self.root;
        let mut key = None;
        for &index in &query.path {
            (key, node) = node
                .child(index)
                .ok_or_else(|| AppError::invalid("That document node does not exist"))?;
        }
        let summary = summarize(query.path.last().copied().unwrap_or(0), key, node, policy);
        let (children, has_more, text) = match query.mode {
            DocumentMode::Children => {
                let total = node.len().unwrap_or(0) as u64;
                let end = query.offset.saturating_add(query.limit as u64).min(total);
                let children = (query.offset.min(end)..end)
                    .filter_map(|i| {
                        node.child(i)
                            .map(|(k, child)| summarize(i, k, child, policy))
                    })
                    .collect();
                (children, end < total, None)
            }
            DocumentMode::Copy => {
                let mut out = Vec::new();
                write_json(node, &mut out, Some(0), policy.full_cell_bytes).map_err(|_| {
                    AppError::limit(
                        "This node exceeds the 1 MiB copy limit; copy a smaller child node",
                    )
                })?;
                (
                    vec![],
                    false,
                    Some(String::from_utf8(out).map_err(|_| AppError::internal())?),
                )
            }
        };
        Ok(DocumentResult {
            session_generation: generation,
            object_id: self.object.id.clone(),
            path: query.path.clone(),
            node: summary,
            children,
            offset: query.offset,
            has_more,
            text,
        })
    }
}

fn clip(value: &str, limit: usize) -> (String, bool) {
    let mut end = value.len().min(limit);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_owned(), end < value.len())
}

fn summarize(index: u64, key: Option<&str>, node: &Node, policy: &ResourcePolicy) -> DocumentNode {
    let (key, key_truncated) = match key {
        Some(key) => {
            let (key, truncated) = clip(key, policy.cell_preview_bytes);
            (Some(key), truncated)
        }
        None => (None, false),
    };
    let scalar = match node {
        Node::String(value) | Node::Number(value) => Some(value.as_str()),
        Node::Bool(true) => Some("true"),
        Node::Bool(false) => Some("false"),
        _ => None,
    };
    let (value, truncated) = match scalar {
        Some(value) => {
            let (value, truncated) = clip(value, policy.cell_preview_bytes);
            (Some(value), truncated)
        }
        None => (None, false),
    };
    DocumentNode {
        index,
        key,
        key_truncated,
        kind: node.kind(),
        value,
        byte_length: scalar.map_or(0, str::len).to_string(),
        truncated,
        child_count: node.len().map(|n| n.to_string()),
    }
}

#[cfg(test)]
pub(crate) fn parse_for_test(text: &str) -> Result<Node, serde_json::Error> {
    let nodes = Flag::new(u64::MAX);
    NodeSeed::root(64, &nodes).deserialize(&mut serde_json::Deserializer::from_str(text))
}
#[cfg(test)]
pub(crate) fn compact_for_test(node: &Node) -> String {
    compact(node)
}
