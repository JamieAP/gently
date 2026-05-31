---
description: Distributed tracing for coding agents.
---

# Introduction

A coding agent is a distributed system you can't see into: prompts fan out to
tools, tools spawn subagents, work happens across processes that live for
milliseconds. **gently** makes that legible.

It hooks a coding harness, turns the session lifecycle - every prompt, tool call,
and subagent - into OpenTelemetry spans, ships them over QUIC to a Cloudflare edge
Worker backed by D1, and lets you query your own traces from the CLI or from
*inside the agent itself* over MCP.

```
  Claude Code
      │  hooks (every tool call)
      ▼
  gently hook ──► ~/.gently/state.db        local state write,
      │            (WAL outbox, durable)     never touches the network
      │
      └─ spawns ─► gently export ──QUIC/HTTP3──► gently-collector (Worker) ──► D1
                     (detached)    h2 fallback        bearer auth          (SQLite)
                                                            ▲
   gently traces│trace│spans│stats│status ── /v1/query ─────┤
   gently mcp  (stdio MCP, exposed to the agent) ───────────┘
```

## Why it's interesting

* **Deterministic ids** - spans reconstruct into a tree across separate, ephemeral
  hook processes, with no shared state and idempotent ingest.
* **The hot path is sacred** - the hook writes one local SQLite row and returns; all network/QUIC work happens in a detached, disposable exporter.
* **Just a hook, no daemon** - nothing long-lived to install or babysit; each
  exporter is a throwaway worker and the next hook is its supervisor.

## Scope

v1 wires **Claude Code**. The hook layer is a `Harness` trait with a `ClaudeCode`
adapter, so Codex and Cursor slot in as new adapters without touching the core.

> Start with the [Quick start](getting-started/quickstart.md), or read
> [Architecture](concepts/architecture.md) for how it fits together.
