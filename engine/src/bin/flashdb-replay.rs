//! Experimental JSONL replay command. It opens an existing, pre-seeded WAL;
//! it never creates or truncates one. Output is one JSON result per request,
//! with a final corpus hash on stderr only after the entire trace succeeds.

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

fn run() -> io::Result<()> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() != 3 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput,
            "usage: flashdb-replay <existing-wal> <trace.jsonl>"));
    }
    let mut engine = Engine::open(&args[1])?;
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
