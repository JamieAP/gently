//! `gently` - capture OTel traces from coding-harness hooks, ship them to a
//! Cloudflare collector, and query them from the CLI or over MCP.

mod cmd_export;
mod cmd_hook;
mod cmd_init;
mod cmd_mcp;
mod cmd_query;
mod cmd_status;
mod config;
mod local_raw;
mod logging;
mod mcp_jq;
mod query_client;

use clap::{Parser, Subcommand, ValueEnum};
use cmd_query::Format;
use query_client::{SpanFilters, TraceFilters};

/// Which coding harness produced the hook event. Selected explicitly because
/// Claude Code and Codex stdin payloads are too similar to distinguish reliably.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum HarnessKind {
    Claude,
    Codex,
}

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
    Hook {
        /// Which harness produced the event (selects the parser).
        #[arg(long, value_enum, default_value_t = HarnessKind::Claude)]
        harness: HarnessKind,
    },
    /// Drain the local outbox to the collector.
    Export,
    /// Show local exporter health and queue depth.
    Status,
    /// Run the MCP stdio server exposing trace queries.
    Mcp,
    /// Install gently's hooks and MCP server into a harness.
    Init {
        /// Install into Claude Code (`~/.claude`).
        #[arg(long, default_value_t = true)]
        claude: bool,
        /// Install into Codex (`~/.codex/config.toml`).
        #[arg(long, default_value_t = false)]
        codex: bool,
    },
    /// List recent traces.
    Traces {
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long)]
        harness: Option<String>,
        #[arg(long)]
        session_id: Option<String>,
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        until: Option<String>,
        #[arg(long)]
        order: Option<String>,
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
        session_id: Option<String>,
        #[arg(long)]
        harness: Option<String>,
        #[arg(long)]
        tool_name: Option<String>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        until: Option<String>,
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long)]
        order: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Per-tool rollup statistics.
    Stats {
        #[arg(long)]
        json: bool,
    },
    /// Resolve the live session id running in a tmux pane (e.g. `%42`), via the
    /// `gently.tmux_pane` the live session stamps on every event.
    Whoami {
        #[arg(long)]
        pane: String,
        #[arg(long)]
        json: bool,
    },
}

fn main() {
    let cli = Cli::parse();

    // The hook path is special: it must never return a non-zero exit nor print
    // to stdout, so it is dispatched first and swallows all errors internally.
    if let Command::Hook { harness } = cli.command {
        cmd_hook::run(harness);
        return;
    }

    if let Err(e) = dispatch(cli.command) {
        eprintln!("gently: {e:#}");
        std::process::exit(1);
    }
}

fn dispatch(command: Command) -> anyhow::Result<()> {
    match command {
        Command::Hook { .. } => unreachable!("handled in main"),
        Command::Export => cmd_export::run(),
        Command::Status => cmd_status::run(),
        Command::Mcp => cmd_mcp::run(),
        Command::Init { claude: _, codex } => {
            if codex {
                cmd_init::run_codex()
            } else {
                cmd_init::run_claude()
            }
        }
        Command::Traces {
            limit,
            harness,
            session_id,
            since,
            until,
            order,
            json,
        } => cmd_query::traces(
            TraceFilters {
                limit,
                harness,
                session_id,
                since,
                until,
                order,
            },
            Format::from_json_flag(json),
        ),
        Command::Trace { trace_id, json } => {
            cmd_query::trace(trace_id, Format::from_json_flag(json))
        }
        Command::Spans {
            trace_id,
            session_id,
            harness,
            tool_name,
            name,
            status,
            kind,
            since,
            until,
            limit,
            order,
            json,
        } => cmd_query::spans(
            SpanFilters {
                trace_id,
                session_id,
                harness,
                tool_name,
                name,
                status,
                kind,
                since,
                until,
                limit,
                order,
            },
            Format::from_json_flag(json),
        ),
        Command::Stats { json } => cmd_query::stats(Format::from_json_flag(json)),
        Command::Whoami { pane, json } => cmd_query::whoami(pane, Format::from_json_flag(json)),
    }
}
