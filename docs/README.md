---
description: Capture and inspect coding-agent activity with Gently.
---

# Gently documentation

Gently records Claude Code and Codex sessions as OpenTelemetry traces. Hooks
queue events in local SQLite; an exporter sends them to a collector. Use the
CLI or MCP to inspect sessions, tool calls and delegated work.

## Start here

| Goal | Read |
| --- | --- |
| Install Gently and capture your first session | [Quick start](getting-started/quickstart.md) |
| Run the supplied localhost collector | [Local setup](getting-started/local-collector.md) |
| Choose settings and supply a collector token | [Configuration](getting-started/configuration.md) |
| Explore traces, waterfalls and MCP tools | [Querying and MCP](guides/querying-and-mcp.md) |
| Find missing spans or fix export/query failures | [Troubleshooting](guides/troubleshooting.md) |
| Understand what is stored or shared | [Security and privacy](concepts/security-and-privacy.md) |

The Cloudflare setup uses Wrangler and D1. Local launchers accept an inherited
token from your secret provider on Macs and Linux; a separately installed
macOS hardware-backed helper is optional. Raw reader enrollment is separate
from credential storage.

## What is supported

Claude Code and Codex coding agents are supported in the terminal and desktop.
Codex hooks require trusting the installed entries in Codex. Tokenless desktop
hooks queue for an export watcher; on Unix, `--serve-queries` also lets tokenless
CLI/MCP clients query through that watcher. See [compatibility](reference/compatibility.md)
for checked versions and the limits of hook fidelity.

Raw content capture is off by default. Ordinary exports still contain identifying
metadata such as paths, host information and session IDs. The collector's
host credentials grant tenant-scoped ingest/read access. Optional raw capture
is encrypted to enrolled devices, with separate sync and resolution opt-ins.
There is no automatic retention. Read the [data flow and controls](concepts/security-and-privacy.md)
before enabling capture or sharing a collector.

## How it works

- [Architecture](concepts/architecture.md): capture, local storage, export and query paths.
- [Trace model](concepts/trace-model.md): sessions, turns, tools, subagents and updated spans.
- [Reliability](concepts/reliability.md): buffering, retries, queue caps and quarantine.

## Reference

- [CLI](reference/cli.md): commands and flags.
- [Harness hooks](reference/hooks.md): the events and fields the adapters handle.
- [OTel format](reference/otel-format.md): the exported envelope and attributes.
- [Collector and Worker](reference/worker.md): endpoints, queries and database behavior.

For development and bug reports, see [Contributing](https://github.com/JamieAP/gently/blob/main/CONTRIBUTING.md).
Gently is [MIT licensed](https://github.com/JamieAP/gently/blob/main/LICENSE); dependencies retain their own licenses.
