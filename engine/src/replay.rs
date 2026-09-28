//! Bounded JSONL replay over the v0 generator's four supported request shapes.
//!
//! Trace bytes (including newline) are hashed as read. Outcomes are emitted via
//! callback instead of retained for the whole corpus. A malformed line or
//! unsupported request fails with a line number; nothing is silently skipped.
//! This is experimental replay, not a general wire protocol or production API.

use std::io::{self, BufRead, ErrorKind, Read};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::engine::Engine;
use crate::request::{execute_request, RequestResult};

const MAX_LINE: u64 = 65_537; // request JSON <= 64 KiB plus newline

#[derive(Debug, PartialEq, Eq)]
pub struct ReplaySummary {
    pub requests: u64,
    pub sha256: String,
}

fn line_error(line: u64, kind: ErrorKind, message: impl std::fmt::Display) -> io::Error {
    io::Error::new(kind, format!("trace line {line}: {message}"))
}

/// Replay a JSONL corpus into an already initialized engine. The callback is
/// invoked once per successfully executed request and may stream result JSONL.
/// The input must end every record with '\n'; partial last records are rejected.
/// An execution error stops replay; earlier committed blocks remain committed.
/// Callers must use a fresh WAL (or intentional dedup replay) for repeated runs.
///
/// `sha256` is returned only after every byte was consumed successfully. No
/// claim of a comparable result may be made after a partial/failed run.
pub fn replay_jsonl(
    engine: &mut Engine,
    mut reader: impl BufRead,
    mut on_result: impl FnMut(u64, &Value, &RequestResult) -> io::Result<()>,
) -> io::Result<ReplaySummary> {
    let mut hash = Sha256::new();
    let mut count = 0u64;
    loop {
        let line_number = count.checked_add(1).ok_or_else(|| io::Error::new(ErrorKind::InvalidData, "trace line counter exhausted"))?;
        let mut line = Vec::new();
        let n = (&mut reader).take(MAX_LINE + 1).read_until(b'\n', &mut line)?;
        if n == 0 { break; }
        if n as u64 > MAX_LINE || line.last() != Some(&b'\n') {
            return Err(line_error(line_number, ErrorKind::InvalidData, "line too long or missing newline"));
        }
        let request: Value = serde_json::from_slice(&line[..line.len() - 1])
            .map_err(|e| line_error(line_number, ErrorKind::InvalidData, e))?;
        hash.update(&line);
        let result = execute_request(engine, &request)
            .map_err(|e| line_error(line_number, e.kind(), e))?;
        on_result(line_number, &request, &result)
            .map_err(|e| line_error(line_number, e.kind(), e))?;
        count = line_number;
    }
    Ok(ReplaySummary { requests: count, sha256: format!("{:x}", hash.finalize()) })
}
