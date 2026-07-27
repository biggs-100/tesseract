// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! Tesseract VQL interactive REPL (Read-Eval-Print Loop).
//!
//! Usage: vql [--data-dir <path>] [--dim <n>] [--cold-dir <path>]
//!
//! Prompts for VQL queries, then displays the AST, the plan tree,
//! execution results, and timing breakdown.

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;

use tesseract_core::embedding::NoopEmbeddingService;
use tesseract_core::episodic::EpisodicMemory;
use tesseract_storage::engine::StorageEngine;
use tesseract_storage::types::*;
use tesseract_vql::executor::{QueryExecutor, QueryTimings, ScoredResult};
use tesseract_vql::parser::parse;
use tesseract_vql::planner::{PlannerConfig, QueryPlanner};

// ---------------------------------------------------------------------------
// CLI arguments
// ---------------------------------------------------------------------------

struct Args {
    data_dir: PathBuf,
    dim: usize,
    cold_dir: PathBuf,
}

fn parse_args() -> Args {
    let raw: Vec<String> = std::env::args().collect();
    let mut i = 1;
    let mut data_dir = PathBuf::from("./vql-data");
    let mut dim = 384usize;
    let mut cold_dir: Option<PathBuf> = None;

    while i < raw.len() {
        match raw[i].as_str() {
            "--data-dir" => {
                i += 1;
                data_dir = PathBuf::from(
                    raw.get(i)
                        .cloned()
                        .expect("--data-dir requires a path argument"),
                );
            }
            "--dim" => {
                i += 1;
                dim = raw
                    .get(i)
                    .cloned()
                    .expect("--dim requires a number argument")
                    .parse()
                    .expect("--dim must be an integer");
            }
            "--cold-dir" => {
                i += 1;
                cold_dir = Some(PathBuf::from(
                    raw.get(i)
                        .cloned()
                        .expect("--cold-dir requires a path argument"),
                ));
            }
            "--help" | "-h" => {
                eprintln!("Usage: vql [--data-dir <path>] [--dim <n>] [--cold-dir <path>]");
                std::process::exit(0);
            }
            other => {
                eprintln!("Unknown argument: {other}");
                eprintln!("Usage: vql [--data-dir <path>] [--dim <n>] [--cold-dir <path>]");
                std::process::exit(1);
            }
        }
        i += 1;
    }

    let cold_dir = cold_dir.unwrap_or_else(|| data_dir.join("cold"));
    Args { data_dir, dim, cold_dir }
}

// ---------------------------------------------------------------------------
// Display helpers
// ---------------------------------------------------------------------------

/// Check if a parse error is due to incomplete input (multi-line mode).
fn is_incomplete(err: &tesseract_common::error::Error) -> bool {
    err.to_string().contains("Incomplete")
}

/// Print results in a table format.
fn print_results(results: &[ScoredResult]) {
    if results.is_empty() {
        println!("  (no results)");
        return;
    }

    // Column widths
    let id_w = results.iter().map(|r| r.id.to_string().len()).max().unwrap_or(2).max(2);
    let score_w = 8usize;

    // Header
    println!(
        "| {:<id_w$} | {:<score_w$} | metadata                           |",
        "id", "score"
    );
    println!(
        "|-{}-|-{}-|{}|",
        "-".repeat(id_w),
        "-".repeat(score_w),
        "-".repeat(37),
    );

    for r in results {
        let meta = r
            .metadata
            .as_ref()
            .map(|m| serde_json::to_string(m).unwrap_or_else(|_| "(invalid)".into()))
            .unwrap_or_else(|| "(none)".into());
        println!(
            "| {:>id_w$} | {:>score_w$.4} | {:<37} |",
            r.id, r.score, meta
        );
    }
}

/// Print timing information.
fn print_timings(t: &QueryTimings) {
    println!(
        "parse: {:.1}ms \u{00b7} plan: {:.1}ms \u{00b7} embed: {:.1}ms \u{00b7} search: {:.1}ms \u{00b7} total: {:.1}ms",
        t.parse_ms, t.plan_ms, t.embed_ms, t.search_ms, t.total_ms
    );
}

/// Print the welcome banner.
fn print_welcome() {
    println!(
        r"  _   _  __ _      _   _  __ _      _   _  __ _      _   _  __ _"
    );
    println!(
        r" | | | |/ /| |    | | | |/ /| |    | | | |/ /| |    | | | |/ /| |"
    );
    println!(
        r" | |_| | / /| |    | |_| | / /| |    | |_| | / /| |    | |_| | / /| |"
    );
    println!(
        r"  \__,_/_/  \_|     \__,_/_/  \_|     \__,_/_/  \_|     \__,_/_/  \_|"
    );
    println!();
    println!("  VQL REPL v0.1.0");
    println!("  Type :help for available commands");
    println!();
}

/// Print the help text.
fn print_help() {
    println!("Commands:");
    println!("  :quit | :q   Exit the REPL");
    println!("  :help        Show this help");
    println!("  :plan <vql>  Show plan tree without executing");
    println!("  :ast <vql>   Show AST without planning or executing");
    println!("  :load <path> Load and execute queries from a file");
    println!();
    println!("Multi-line input:");
    println!("  If a query does not form a complete VQL statement,");
    println!("  the REPL will prompt with '...> ' to continue.");
    println!();
    println!("Press Ctrl+C or type :quit to exit.");
}

// ---------------------------------------------------------------------------
// Command handlers
// ---------------------------------------------------------------------------

/// Parse and print the AST.
fn handle_ast(vql: &str) {
    match parse(vql) {
        Ok(query) => println!("{query}"),
        Err(e) => eprintln!("Parse error: {e}"),
    }
}

/// Parse, plan, and print the plan tree.
fn handle_plan(vql: &str, planner: &QueryPlanner) {
    let query = match parse(vql) {
        Ok(q) => q,
        Err(e) => {
            eprintln!("Parse error: {e}");
            return;
        }
    };
    match planner.plan_to_tree(&query) {
        Ok(plan) => println!("{plan}"),
        Err(e) => eprintln!("Plan error: {e}"),
    }
}

/// Parse, plan, execute, and print results + timings.
async fn handle_query(vql: &str, executor: &QueryExecutor, planner: &QueryPlanner) {
    // 1. Parse + Display AST
    let query = match parse(vql) {
        Ok(q) => {
            println!("── AST ──────────────────────────────────────────────");
            println!("{q}");
            println!();
            q
        }
        Err(e) => {
            eprintln!("Parse error: {e}");
            return;
        }
    };

    // 2. Plan + Display plan tree
    match planner.plan_to_tree(&query) {
        Ok(ref p) => {
            println!("── Plan ─────────────────────────────────────────────");
            println!("{p}");
            println!();
        }
        Err(e) => {
            eprintln!("Plan error: {e}");
            return;
        }
    }

    // 3. Execute
    match executor.execute(vql, None).await {
        Ok(result) => {
            println!("── Results ({}) ───────────────────────────────────", result.results.len());
            print_results(&result.results);
            println!();
            print_timings(&result.timings);
        }
        Err(e) => {
            eprintln!("Execution error: {e}");
        }
    }
}

/// Execute queries from a file, one per line.
async fn handle_load(path: &str, executor: &QueryExecutor, planner: &QueryPlanner) {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("Error opening '{path}': {e}");
            return;
        }
    };

    let reader = io::BufReader::new(file);
    let mut line_num = 0usize;

    for line in reader.lines() {
        line_num += 1;
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[line {line_num}] Read error: {e}");
                continue;
            }
        };

        let trimmed = line.trim().to_string();
        if trimmed.is_empty()
            || trimmed.starts_with("--")
            || trimmed.starts_with("//")
            || trimmed.starts_with('#')
        {
            continue;
        }

        println!("[{line_num}] > {trimmed}");
        handle_query(&trimmed, executor, planner).await;
        println!();
    }
}

// ---------------------------------------------------------------------------
// REPL main loop
// ---------------------------------------------------------------------------

async fn repl_loop(executor: &QueryExecutor, planner: &QueryPlanner) {
    let mut input_buf = String::new();
    let mut multi_line = false;

    loop {
        let prompt = if multi_line { "...> " } else { "vql> " };
        print!("{prompt}");
        if let Err(e) = io::stdout().flush() {
            eprintln!("stdout error: {e}");
            break;
        }

        let mut line = String::new();
        match io::stdin().lock().read_line(&mut line) {
            Ok(0) => {
                // EOF (Ctrl+D on Unix, Ctrl+Z on Windows)
                println!();
                break;
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {
                // Ctrl+C on some systems — exit
                println!();
                break;
            }
            Err(e) => {
                eprintln!("stdin error: {e}");
                break;
            }
        }

        let trimmed = line.trim().to_string();

        // Empty line at top-level → just re-prompt
        if !multi_line && trimmed.is_empty() {
            continue;
        }

        // === Command handling (top-level only) ===
        if !multi_line && trimmed.starts_with(':') {
            let cmd = trimmed.as_str();
            match cmd {
                ":quit" | ":q" => break,
                ":help" => print_help(),
                _ if cmd.starts_with(":plan ") => handle_plan(&cmd[6..], planner),
                _ if cmd.starts_with(":ast ") => handle_ast(&cmd[5..]),
                _ if cmd.starts_with(":load ") => handle_load(&cmd[6..], executor, planner).await,
                _ => eprintln!("Unknown command: {cmd}. Type :help for available commands."),
            }
            continue;
        }

        // === Query accumulation ===
        if multi_line {
            input_buf.push(' ');
            input_buf.push_str(&trimmed);
        } else {
            input_buf = trimmed;
        }

        if input_buf.is_empty() {
            multi_line = false;
            continue;
        }

        // Try to parse — if incomplete, keep reading
        match parse(&input_buf) {
            Ok(_) => {
                let full = std::mem::take(&mut input_buf);
                multi_line = false;
                handle_query(&full, executor, planner).await;
            }
            Err(ref e) if is_incomplete(e) => {
                multi_line = true;
                // Continue gathering input
            }
            Err(e) => {
                eprintln!("Error: {e}");
                input_buf.clear();
                multi_line = false;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Entrypoint
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    let args = parse_args();

    // Ensure data directories exist
    if let Err(e) = std::fs::create_dir_all(&args.data_dir) {
        eprintln!("Failed to create data directory '{}': {e}", args.data_dir.display());
        std::process::exit(1);
    }
    if let Err(e) = std::fs::create_dir_all(&args.cold_dir) {
        eprintln!("Failed to create cold directory '{}': {e}", args.cold_dir.display());
        std::process::exit(1);
    }

    // Build storage configuration (modelled after tesseract-api)
    let storage_config = StorageConfig {
        wal: WalConfig {
            wal_dir: args.data_dir.join("wal"),
            segment_size: 1024 * 1024,
            fsync_interval_ms: 1000,
            fsync_interval_ops: 10000,
        },
        hot: HotStoreConfig { max_records: 10_000 },
        cold: ColdStoreConfig {
            data_dir: args.cold_dir.clone(),
            zstd_level: 0,
            max_rows_per_file: 100,
        },
        skeleton: SkeletonConfig { wake_threshold: 0.15 },
        cache: PageCacheConfig { capacity: 100 },
        index: IndexConfig {
            enabled: true,
            dim: args.dim,
            hnsw: Default::default(),
            path: args.data_dir.join("index.hnsw"),
        },
        lifecycle: LifecycleConfig::default(),
        topological: TopologicalConfig::default(),
        merkle: Default::default(),
        shutdown: ShutdownConfig::default(),
    };

    let storage = match StorageEngine::open(storage_config).await {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("Failed to open storage engine: {e}");
            std::process::exit(1);
        }
    };

    let embedder = Arc::new(NoopEmbeddingService) as Arc<dyn tesseract_core::embedding::EmbeddingService>;
    let episodic = Arc::new(EpisodicMemory::new());

    let planner_config = PlannerConfig {
        dim: args.dim,
        ..Default::default()
    };

    let executor = QueryExecutor::new(storage, embedder, episodic, planner_config.clone());
    let planner = QueryPlanner::new(planner_config);

    print_welcome();
    repl_loop(&executor, &planner).await;
}
