# kemana-proxy

A Rust service that normalizes upstream transit data (static schedules and real-time vehicle positions) into a single, cleanly typed API for client apps.

The repository currently holds the project skeleton and tooling only.

## Requirements

- [mise](https://mise.jdx.dev), which installs the Rust toolchain, Python, hk and the other tools at the versions pinned in `mise.toml`, runs the tasks and enables the git hooks

## Development

```sh
mise install        # install the pinned tools and enable the git hooks

mise run format     # format sources
mise run check      # rustfmt check and clippy with warnings denied
mise run test       # script tests and cargo tests
mise run build      # release build
mise run coverage   # write lcov.info
```

Run the binary with `cargo run`.

## Contributing

See [CONTRIBUTING.md](.github/CONTRIBUTING.md). Report vulnerabilities as described in [SECURITY.md](.github/SECURITY.md).

## License

Apache License 2.0. See [LICENSE](LICENSE).
