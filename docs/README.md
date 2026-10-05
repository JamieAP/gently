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

The Cloudflare setup uses Wrangler and D1. The supplied local launcher also
requires a separately installed `agent-secrets` helper; Gently does not install
that helper or its hardware-backed vault.

## What is supported

CLI hooks and token-configured queries are included for Claude Code and Codex.
Codex hook registration also requires trusting the installed entries in Codex.
Desktop hooks can queue without a token and use a separate export watcher.
That watcher does not authenticate CLI or MCP queries. Authenticated desktop
MCP access and Claude Chat/Cowork integration remain incomplete.

Raw content capture is off by default. Ordinary exports still contain identifying
metadata such as paths, host information and session IDs. The collector's
shared token grants access to all its traces; there is no tenant separation or
automatic retention. Read the [data flow and controls](concepts/security-and-privacy.md)
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
