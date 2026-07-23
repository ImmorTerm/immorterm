//! Replay an asciinema cast file through immorterm-core::Terminal and dump
//! the resulting scrollback as JSON. Run with the same cols/rows the original
//! session used (or let resize records in the cast steer it).
//!
//! Usage: cargo run --example replay_cast --release -- <cast_path> [out.json]

use immorterm_core::Terminal;
use std::env;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: replay_cast <cast_path> [out.json]");
        std::process::exit(2);
    }
    let cast_path = &args[1];
    let out_path = args.get(2).cloned();

    let file = File::open(cast_path).expect("open cast");
    let mut lines = BufReader::new(file).lines();

    let header = lines.next().expect("header").expect("header read");
    let header_json: serde_json::Value = serde_json::from_str(&header).expect("parse header");
    let mut cols: usize = header_json.get("width").and_then(|v| v.as_u64()).unwrap_or(80) as usize;
    let mut rows: usize = header_json.get("height").and_then(|v| v.as_u64()).unwrap_or(24) as usize;
    eprintln!("header cols={cols} rows={rows}");

    let mut term = Terminal::new(cols, rows);
    let mut resize_count = 0usize;
    let mut output_count = 0usize;

    for line in lines.map_while(Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        let rec: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("skip line (parse err: {e}): {}", &line[..line.len().min(80)]);
                continue;
            }
        };
        let arr = match rec.as_array() {
            Some(a) => a,
            None => continue,
        };
        if arr.len() < 3 {
            continue;
        }
        let kind = arr[1].as_str().unwrap_or("");
        let payload = arr[2].as_str().unwrap_or("");
        match kind {
            "o" => {
                term.process(payload.as_bytes());
                output_count += 1;
            }
            "r" => {
                if let Some((c, r)) = payload.split_once('x') {
                    if let (Ok(nc), Ok(nr)) = (c.parse::<usize>(), r.parse::<usize>()) {
                        cols = nc;
                        rows = nr;
                        term.resize(cols, rows);
                        resize_count += 1;
                    }
                }
            }
            _ => {}
        }
    }
    eprintln!(
        "processed: output_events={output_count} resize_events={resize_count} \
         final cols={cols} rows={rows} scrollback_rows={sb} grid_rows={gr}",
        sb = term.scrollback.len(),
        gr = term.grid.row_count()
    );

    // Dump scrollback as JSON (sequence of rows as runs)
    let lines: Vec<immorterm_core::log::ScrollbackLine> = (0..term.scrollback.len())
        .filter_map(|i| {
            term.scrollback.get(i).map(|row| immorterm_core::log::ScrollbackLine {
                runs: immorterm_core::log::row_to_runs(row),
                wrapped: row.wrapped,
            })
        })
        .collect();
    let json = serde_json::to_string_pretty(&lines).expect("serialize");
    if let Some(p) = out_path {
        let mut f = File::create(&p).expect("create out");
        f.write_all(json.as_bytes()).expect("write out");
        eprintln!("wrote {} bytes to {p}", json.len());
    } else {
        println!("{json}");
    }
}
