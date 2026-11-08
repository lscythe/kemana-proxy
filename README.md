# kemana-proxy

A Rust service that normalizes upstream transit data (static schedules and real-time vehicle positions) into a single, cleanly typed API for client apps.

## Static bundle

`build-bundle` turns a GTFS Schedule zip into an offline SQLite bundle that follows `contracts/bundle/schema.sql`. Every id is namespaced as `<operator>:<source id>`, and times are seconds since the start of the service day (past 24 hours for trips after midnight). A malformed row fails the whole build with its file and line.

Service calendars come from `calendar.txt` and `calendar_dates.txt`. Either may be absent, but a feed needs at least one. A service named only in `calendar_dates.txt` has no weekly pattern and runs only on the dates it adds.

Blank intermediate stop times are interpolated by straight-line distance between stops, from the departure before them to the arrival after them. The first and last stops of a trip must have a time.

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

Bundles are streamed from disk with a `Content-Length`. `HEAD` returns the same headers without a body.

| Route | `Cache-Control` | `ETag` |
| --- | --- | --- |
| Manifest | `no-cache` | sha256 of the manifest bytes, quoted |
| Bundle | `public, max-age=31536000, immutable` | the version, quoted |

A bundle's bytes never change for a version, so clients can keep it forever and only revalidate the manifest. Both routes answer a matching `If-None-Match` (a list, `*`, or a `W/` tag) with `304 Not Modified` and no body. A missing file is still 404, even with `If-None-Match`.

### Resuming a download

Bundle responses carry `Accept-Ranges: bytes`, so a client can resume an interrupted download. `GET` with a single `Range` header (`bytes=a-b`, `bytes=a-` or `bytes=-n`) returns `206 Partial Content` with a `Content-Range`, streamed from disk starting at the offset. An end past the last byte is clamped.

- A start at or past the end of the bundle (or `bytes=-0`) returns `416` with `Content-Range: bytes */<length>`.
- Several ranges, another unit, or a malformed or reversed range are ignored and the full bundle is returned with `200`.
- `If-Range` is honoured only when it is exactly the bundle's quoted `ETag`. A weak tag, a different tag or a date returns the full bundle, so a client holding a stale partial file restarts.
- A matching `If-None-Match` is answered with `304` before `Range` is considered. A missing bundle is still 404.
- `HEAD` and the manifest ignore `Range`.

To check a resumed file, compare the sha256 of the joined parts with the manifest.

`serve` stops on ctrl-c or SIGTERM. Requests in flight get up to 30 seconds to finish, then the process exits regardless, so a slow client cannot block a deploy.

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
