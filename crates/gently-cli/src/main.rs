//! `gently` - capture OTel traces from coding-harness hooks, ship them to a
//! Cloudflare collector, and query them from the CLI or over MCP.

mod cmd_export;
mod cmd_hook;
mod cmd_init;
mod cmd_mcp;
mod cmd_query;
mod cmd_raw;
mod cmd_status;
mod collector;
mod config;
mod json_fidelity;
mod local_raw;
mod logging;
mod mcp_jq;
#[cfg(unix)]
mod query_broker;
mod query_client;
mod waterfall;

use clap::{Parser, Subcommand, ValueEnum};
use cmd_query::{Format, TraceFormat};
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
    Export {
        /// Keep draining new hook events after one secret-store unlock.
        #[arg(long)]
        watch: bool,
        /// Serve read-only queries for tokenless local CLI and desktop MCP
        /// clients over an owner-only Unix socket. Requires --watch.
        #[arg(long, requires = "watch")]
        serve_queries: bool,
        /// Keep queued envelopes even above outbox_cap; do not trim history
        /// before exporting. Queue storage can grow without a limit.
        #[arg(long)]
        preserve_backlog: bool,
        /// Seconds between drains in watch mode.
        #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u64).range(1..=60))]
        interval_secs: u64,
        /// Retry retained ciphertext previously rejected by the collector.
        #[arg(long)]
        retry_raw_quarantine: bool,
    },
    /// Manage encrypted raw-value reader enrollment and public trust policy.
    Raw {
        #[command(subcommand)]
        command: cmd_raw::RawCommand,
    },
    /// Show local capture, recipient-policy and export health.
    Status {
        /// Print health as machine-readable JSON without contacting the collector.
        #[arg(long)]
        json: bool,
    },
    /// Print the resolved public configuration for setup and local launchers.
    Config {
        #[arg(long, required_unless_present = "check", conflicts_with = "check")]
        json: bool,
        /// Validate public policy, configured paths and an existing state schema.
        #[arg(long, required_unless_present = "json", conflicts_with = "json")]
        check: bool,
    },
    /// Run the MCP stdio server exposing trace queries.
    Mcp,
    /// Install gently's hooks and MCP server into a harness.
    Init {
        /// Install into Claude Code (`~/.claude`).
        #[arg(long, conflicts_with = "codex")]
        claude: bool,
        /// Install into Codex (`~/.codex/config.toml`).
        #[arg(long, default_value_t = false)]
        codex: bool,
        /// Allow MCP queries to decrypt raw prompt/tool values on this reader.
        #[arg(long, default_value_t = false)]
        resolve_raw_values: bool,
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
    /// Show one trace as a span tree, JSON, or a timing waterfall.
    Trace {
        trace_id: String,
        #[arg(long, conflicts_with = "waterfall")]
        json: bool,
        /// Render a timing waterfall and trace integrity checks.
        #[arg(long)]
        waterfall: bool,
    },
    /// Render a JSON span array from stdin as a waterfall. No collector needed.
    Waterfall,
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
        Command::Export {
            watch,
            serve_queries,
            preserve_backlog,
            interval_secs,
            retry_raw_quarantine,
        } => {
            if watch {
                cmd_export::watch(
                    interval_secs,
                    serve_queries,
                    preserve_backlog,
                    retry_raw_quarantine,
                )
            } else {
                cmd_export::run(preserve_backlog, retry_raw_quarantine)
            }
        }
        Command::Raw { command } => cmd_raw::run(command),
        Command::Status { json } => cmd_status::run(json),
        Command::Config { check, .. } => {
            if check {
                config::check_setup()
            } else {
                config::print_setup_json()
            }
        }
        Command::Mcp => cmd_mcp::run(),
        Command::Init {
            claude: _,
            codex,
            resolve_raw_values,
        } => {
            if codex {
                cmd_init::run_codex(resolve_raw_values)
            } else {
                cmd_init::run_claude(resolve_raw_values)
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
        Command::Trace {
            trace_id,
            json,
            waterfall,
        } => cmd_query::trace(
            trace_id,
            if waterfall {
                TraceFormat::Waterfall
            } else if json {
                TraceFormat::Json
            } else {
                TraceFormat::Tree
            },
        ),
        Command::Waterfall => cmd_query::waterfall(),
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
