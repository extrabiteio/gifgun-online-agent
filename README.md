# GifGun Online Agent

GifGun Online Agent is a native local bridge for [GifGun Online](https://gifgun.online). It lets a shell-capable agent inspect and operate an editor tab after the user approves the connection.

The browser remains authoritative for editor state, media validation, project data, preview, and rendering. The native process supplies a local command and file-transfer boundary.

## Security model

- The bridge listens only on the loopback interface.
- The browser checks the exact expected origin.
- A user must approve each browser connection.
- Browser and command clients use separate credentials.
- Pairing instructions expire.
- Commands contain structured data. They do not contain media bytes, project bytes, local file paths, or browser object URLs.
- The embedded editor contract must match the contract in the open browser tab.

See [SECURITY.md](SECURITY.md) to report a vulnerability.

## Requirements

- Rust 1.90
- A supported GifGun Online editor tab

## Build from source

```sh
cargo build --locked
```

The debug executable is in `target/debug/gifgun-agent`.

For normal use, follow the Agent instructions shown in GifGun Online. Do not create or reuse a pairing instruction manually.

To inspect the command-line interface:

```sh
cargo run --locked -- --help
```

## Validate a change

```sh
cargo fmt -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

## Editor contract

The browser owns the editor contract. The files in `contract/` are generated browser artifacts:

- `agent-contract.json` defines the protocol and capability schemas embedded in the executable.
- `conformance-fixtures.json` verifies native validation behavior.

Do not edit these files by hand. Update both files together from the matching GifGun Online browser contract. The native version and browser release pin must also stay synchronized.

## Releases

This repository currently provides source code only. It does not publish executable files.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Licensed under the [Apache License 2.0](LICENSE).
