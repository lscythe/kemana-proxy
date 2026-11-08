# kemana-proxy

A Rust service that normalizes upstream transit data (static schedules and real-time vehicle positions) into a single, cleanly typed API for client apps.

## Static bundle

`build-bundle` turns a GTFS Schedule zip into an offline SQLite bundle that follows `contracts/bundle/schema.sql`. Every id is namespaced as `<operator>:<source id>`, and times are seconds since the start of the service day (past 24 hours for trips after midnight). A malformed row fails the whole build with its file and line.

```sh
cargo run -- build-bundle --operator demo --gtfs feed.zip --out bundles
cargo run -- serve --bundles bundles --addr 127.0.0.1:8080
```

The output directory holds one `<version>.sqlite.zst` per source feed and a `manifest.pb` for the latest build. The version is `<schema version>-<first 16 hex digits of the feed's sha256>`, so building an unchanged feed again is skipped.

One output directory serves one operator: building a feed whose bundle already exists under a different `--operator` fails. The same source is never rebuilt unless its bundle file is deleted.

| Endpoint | Response |
| --- | --- |
| `GET /v1/bundle/manifest` | `kemana.bundle.v1.BundleManifest` as protobuf; `sha256` and `size_bytes` describe the compressed download |
| `GET /v1/bundle/{version}` | The zstd-compressed SQLite bundle, or 404 for an unknown version |

## Requirements

- [mise](https://mise.jdx.dev), which installs the Rust toolchain, Python, hk and the other tools at the versions pinned in `mise.toml`, runs the tasks and enables the git hooks
- The `contracts` submodule: clone with `--recurse-submodules`, or run `git submodule update --init`

## Development

```sh
mise install        # install the pinned tools and enable the git hooks

mise run format     # format sources
mise run check      # rustfmt check and clippy with warnings denied
mise run test       # script tests and cargo tests
mise run e2e        # build, serve and download a bundle from the demo feed
mise run build      # release build
mise run coverage   # write lcov.info
```

## Contributing

See [CONTRIBUTING.md](.github/CONTRIBUTING.md). Report vulnerabilities as described in [SECURITY.md](.github/SECURITY.md).

## License

Apache License 2.0. See [LICENSE](LICENSE).
