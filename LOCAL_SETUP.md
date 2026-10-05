# Local collector

The full guide is [local collector setup](docs/getting-started/local-collector.md).
It covers dependencies, local D1, agent hooks, credentials and verification.

The launchers accept an inherited `GENTLY_TOKEN` from any authorized secret
provider. Preflight before unlocking credentials:

```sh
./scripts/collector-local --check
```

Then, in a foreground process with the token inherited:

```sh
./scripts/collector-local
```

For the separately installed macOS helper, wrap it explicitly:

```sh
agent-secrets run default -- ./scripts/collector-local
```

Leave the terminal open and stop with Ctrl+C. To attach an exporter to an
already-running collector, preflight `./scripts/export-local --check`, then
run `./scripts/export-local` through the same provider.

The default local tenant/device is `personal`/`local`. A watcher authenticates
exports, not separate CLI/MCP queries. For Cloudflare, follow the
[quick start](docs/getting-started/quickstart.md). Raw encryption is a separate
[device enrollment workflow](docs/guides/encrypted-raw-values.md).
