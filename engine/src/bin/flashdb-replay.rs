//! Experimental JSONL replay command. It opens an existing, pre-seeded engine
//! and never creates or truncates a WAL. The first argument is either a WAL file
//! (opened with `Engine::open`, tiles ignored) or an engine **directory** (opened
//! tile-aware with `Engine::open_discover`, so published tiles are consulted
//! before the memtable). Output is one JSON result per request, with a final
//! corpus hash on stderr only after the entire trace succeeds.

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Write};

use flashdb_engine::engine::Engine;
use flashdb_engine::replay::replay_jsonl;
use serde::Serialize;
use serde_json::Value;

#[derive(Serialize)]
struct Output<'a> {
    line: u64,
    id: &'a Value,
    blocks: &'a flashdb_engine::request::RequestResult,
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() != 3 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput,
            "usage: flashdb-replay <existing-wal-or-dir> <trace.jsonl>").into());
    }
    // A directory is the tile-aware path; a file keeps the historical WAL-only
    // open so the existing callers (and their recovery expectations) are intact.
    let path = std::path::Path::new(&args[1]);
    let mut engine = if path.is_dir() { Engine::open_discover(path)? } else { Engine::open(path)? };
    let trace = BufReader::new(File::open(&args[2])?);
    let stdout = io::stdout();
    let mut writer = BufWriter::new(stdout.lock());
    let summary = replay_jsonl(&mut engine, trace, |line, request, blocks| {
        serde_json::to_writer(&mut writer, &Output { line, id: &request["id"], blocks })?;
        writer.write_all(b"\n")
    })?;
    writer.flush()?;
    eprintln!("requests={} sha256={}", summary.requests, summary.sha256);
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("replay failed: {e}");
        std::process::exit(1);
    }
}
