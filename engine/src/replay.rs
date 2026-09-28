//! Bounded JSONL replay over the v0 generator's four supported request shapes.
//!
//! Trace bytes (including LF) are hashed as read. Outcomes are emitted via
//! callback instead of retained for the whole corpus. The parser rejects
//! duplicate object keys before conversion to Value. Every failure names its
//! line and stage; nothing is silently skipped. This is experimental replay,
//! not a general wire protocol or production API.

use std::fmt;
use std::io::{self, BufRead, ErrorKind, Read};

use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::Deserialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::engine::Engine;
use crate::request::{execute_request, RequestResult};

const MAX_LINE: u64 = 65_537; // request JSON <= 64 KiB plus newline

#[derive(Debug, PartialEq, Eq)]
pub struct ReplaySummary {
    pub requests: u64,
    pub sha256: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplayStage { Read, Parse, Execute, Output }

#[derive(Debug)]
pub struct ReplayError {
    pub line: u64,
    pub stage: ReplayStage,
    source: io::Error,
}

impl ReplayError {
    fn new(line: u64, stage: ReplayStage, source: io::Error) -> Self { Self { line, stage, source } }
    pub fn kind(&self) -> ErrorKind { self.source.kind() }
}

impl fmt::Display for ReplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "trace line {} [{}]: {}", self.line, match self.stage {
            ReplayStage::Read => "read", ReplayStage::Parse => "parse",
            ReplayStage::Execute => "execute", ReplayStage::Output => "output",
        }, self.source)
    }
}

impl std::error::Error for ReplayError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> { Some(&self.source) }
}

// serde_json::Value silently keeps the last member of a duplicate-key object.
// Recursively reject duplicates while building the same Value representation.
struct UniqueValue(Value);

impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct UniqueVisitor;
        impl<'de> Visitor<'de> for UniqueVisitor {
            type Value = UniqueValue;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("JSON value with unique object keys") }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Self::Value, E> { Ok(UniqueValue(Value::Bool(v))) }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> { Ok(UniqueValue(Value::from(v))) }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> { Ok(UniqueValue(Value::from(v))) }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
                let n = serde_json::Number::from_f64(v).ok_or_else(|| E::custom("non-finite number"))?;
                Ok(UniqueValue(Value::Number(n)))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> { Ok(UniqueValue(Value::String(v.to_owned()))) }
            fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> { Ok(UniqueValue(Value::String(v))) }
            fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> { Ok(UniqueValue(Value::Null)) }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> { Ok(UniqueValue(Value::Null)) }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut out = Vec::new();
                while let Some(value) = seq.next_element::<UniqueValue>()? { out.push(value.0); }
                Ok(UniqueValue(Value::Array(out)))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut out = Map::new();
                while let Some((key, value)) = map.next_entry::<String, UniqueValue>()? {
                    if out.contains_key(&key) {
                        return Err(de::Error::custom(format!("duplicate object key {key}")));
                    }
                    out.insert(key, value.0);
                }
                Ok(UniqueValue(Value::Object(out)))
            }
        }
        d.deserialize_any(UniqueVisitor)
    }
}

/// Replay into an already initialized engine. The callback is invoked once per
/// executed request and may stream result JSONL. Every record must end in LF.
/// A failure stops replay with a stage and line; earlier blocks remain committed.
/// Use a fresh WAL (or intentional dedup replay) for comparable runs. A corpus
/// SHA-256 is returned only after the entire trace and output succeed.
pub fn replay_jsonl(
    engine: &mut Engine,
    mut reader: impl BufRead,
    mut on_result: impl FnMut(u64, &Value, &RequestResult) -> io::Result<()>,
) -> Result<ReplaySummary, ReplayError> {
    let mut hash = Sha256::new();
    let mut count = 0u64;
    loop {
        let line_number = count.checked_add(1).ok_or_else(|| ReplayError::new(count, ReplayStage::Read,
            io::Error::new(ErrorKind::InvalidData, "trace line counter exhausted")))?;
        let mut line = Vec::new();
        let n = (&mut reader).take(MAX_LINE + 1).read_until(b'\n', &mut line)
            .map_err(|e| ReplayError::new(line_number, ReplayStage::Read, e))?;
        if n == 0 { break; }
        if n as u64 > MAX_LINE || line.last() != Some(&b'\n') {
            return Err(ReplayError::new(line_number, ReplayStage::Parse,
                io::Error::new(ErrorKind::InvalidData, "line too long or missing newline")));
        }
        let request = serde_json::from_slice::<UniqueValue>(&line[..line.len() - 1])
            .map_err(|e| ReplayError::new(line_number, ReplayStage::Parse,
                io::Error::new(ErrorKind::InvalidData, e)))?.0;
        hash.update(&line);
        let result = execute_request(engine, &request)
            .map_err(|e| ReplayError::new(line_number, ReplayStage::Execute, e))?;
        on_result(line_number, &request, &result)
            .map_err(|e| ReplayError::new(line_number, ReplayStage::Output, e))?;
        count = line_number;
    }
    Ok(ReplaySummary { requests: count, sha256: format!("{:x}", hash.finalize()) })
}
