//! `gently` - capture OTel traces from coding-harness hooks, ship them to a
//! Cloudflare collector, and query them from the CLI or over MCP.

mod cmd_export;
mod cmd_hook;
mod cmd_init;
mod cmd_mcp;
mod cmd_query;
mod config;
mod query_client;

use clap::{Parser, Subcommand};
use cmd_query::Format;
use query_client::SpanFilters;

#[derive(Parser)]
#[command(name = "gently", version, about = "OTel tracing for coding harnesses")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Harness hook entrypoint (reads the event JSON on stdin). Never writes
    /// stdout and always exits 0.
    Hook,
    /// Drain the local outbox to the collector.
    Export,
    /// Run the MCP stdio server exposing trace queries.
    Mcp,
    /// Install gently's hooks and MCP server into a harness.
    Init {
        /// Install into Claude Code (`~/.claude`).
        #[arg(long, default_value_t = true)]
        claude: bool,
    },
    /// List recent traces.
    Traces {
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long)]
        harness: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Show one trace as a span tree.
    Trace {
        trace_id: String,
        #[arg(long)]
        json: bool,
    },
    /// Search spans.
    Spans {
        #[arg(long)]
        trace_id: Option<String>,
        #[arg(long)]
        tool_name: Option<String>,
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long)]
        json: bool,
    },
    /// Per-tool rollup statistics.
    Stats {
        #[arg(long)]
        json: bool,
    },
}

fn main() {
    let cli = Cli::parse();

    // The hook path is special: it must never return a non-zero exit nor print
    // to stdout, so it is dispatched first and swallows all errors internally.
    if let Command::Hook = cli.command {
        cmd_hook::run();
        return;
    }

    if let Err(e) = dispatch(cli.command) {
        eprintln!("gently: {e:#}");
        std::process::exit(1);
    }
}

fn dispatch(command: Command) -> anyhow::Result<()> {
    match command {
        Command::Hook => unreachable!("handled in main"),
        Command::Export => cmd_export::run(),
        Command::Mcp => cmd_mcp::run(),
        Command::Init { claude: _ } => cmd_init::run_claude(),
        Command::Traces {
            limit,
            harness,
            json,
        } => cmd_query::traces(limit, harness, Format::from_json_flag(json)),
        Command::Trace { trace_id, json } => {
            cmd_query::trace(trace_id, Format::from_json_flag(json))
        }
        Command::Spans {
            trace_id,
            tool_name,
            status,
            since,
            limit,
            json,
        } => cmd_query::spans(
            SpanFilters {
                trace_id,
                tool_name,
                status,
                since,
                limit,
            },
            Format::from_json_flag(json),
        ),
        Command::Stats { json } => cmd_query::stats(Format::from_json_flag(json)),
    }
}
