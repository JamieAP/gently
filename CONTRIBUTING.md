# Contributing

Focused bug reports and small pull requests are welcome. Describe the behaviour
you want to change and include a reproducible example where possible.

## Development setup

From the repository root:

```sh
cargo test
cargo clippy --all-targets -- -D warnings
```

For the collector:

```sh
cd worker
npm ci
npm test
```

The Rust workspace is in `crates/`, the collector in `worker/`, and rendering
helpers in `scripts/`. See the [architecture guide](docs/concepts/architecture.md)
for the capture, storage, export, and query paths.

Test behaviour changes in the affected component. For documentation changes,
check the commands and links. Keep configuration examples consistent with the
CLI and collector.

## Bug reports

Include the Gently version, agent and version, operating system, reproduction
steps, expected behaviour, and observed behaviour. Synthetic hook payloads are
useful for reproducing adapter problems.

Session records and debug captures can contain private content. Review anything
you attach to an issue, and use invented values for credentials, local paths,
prompts, and tool output. See [data and privacy](README.md#data-and-privacy).

## License

Gently is licensed under [MIT](LICENSE).
