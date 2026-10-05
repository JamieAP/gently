# Local collector

The full guide is [Local collector setup](docs/getting-started/local-collector.md).
It covers prerequisites, local D1, agent hooks, foreground unlock, verification
and troubleshooting.

The supplied launchers require the separate `agent-secrets` helper and a
configured `GENTLY_TOKEN`. They do not install that helper or vault. If those are
already configured, start from the repository root:

```sh
./scripts/collector-local
```

Leave the terminal running and approve the helper's unlock prompt. Stop with
Ctrl+C. To attach only an exporter to an already-running local collector:

```sh
./scripts/export-local
```

A watcher authenticates exports, not separate CLI/MCP query processes.
For a setup that uses a Cloudflare account, follow the
[quick start](docs/getting-started/quickstart.md).
