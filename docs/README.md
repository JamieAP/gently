---
description: Distributed tracing for coding agents.
---

# Gently documentation

Gently records Claude Code and Codex sessions as OpenTelemetry traces. Hooks store
spans in a local SQLite outbox, a detached exporter sends them to a Cloudflare
Worker backed by D1, and CLI or MCP queries let you inspect the recorded activity.

## Getting started

1. Follow the [quick start](getting-started/quickstart.md) to deploy a collector
   and install the integration.
2. Set the collector URL and token using the
   [configuration guide](getting-started/configuration.md).
3. Use [CLI and MCP queries](guides/querying-and-mcp.md) to inspect traces.

Claude Code and Codex integrations are included. Install with
`gently init --claude` or `gently init --codex`; Codex hooks must also be trusted
inside Codex.

## How it works

The [architecture guide](concepts/architecture.md) describes the hook, local store,
exporter, and collector. The [trace model](concepts/trace-model.md) explains how
session, turn, tool, and subagent spans relate, including provisional spans and
updates with the same span ID.

## Privacy and access

Normal exports contain digests and metadata, including local paths and host
information. Selected raw values remain in local plaintext storage. Resolving
those values through CLI or MCP is opt-in; raw MCP results may enter the calling
agent's model-provider context. The collector's shared bearer token grants
access to all traces.

Read [security and privacy](concepts/security-and-privacy.md) for the full data
flow, permissions, debug-capture behaviour, and retention limits.

## License

Gently is [MIT licensed](../LICENSE). Dependencies retain their own licenses and
notices.
