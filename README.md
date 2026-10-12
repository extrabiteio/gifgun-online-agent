# GifGun Online Agent

GifGun Online Agent is a native local bridge for [GifGun Online](https://gifgun.online). It lets a shell-capable agent inspect and operate an editor tab after the user approves the connection.

The browser remains authoritative for editor state, media validation, project data, preview, and rendering. The native process supplies a local command and file-transfer boundary.

## Managed workspaces

The downloaded executable can open a visible editor with `open`. Each launch creates a separate workspace and a separate local bridge. Sign in and approve the connection in the editor. No pairing instruction must be copied for this flow.

Use the returned `sessionId` with `--session` for every later command. Managed workspaces do not change or use the shared default session. Use the executable's `--help` and runtime `capabilities` response for the command interface.

Reload keeps the workspace and its local draft. Disconnect stops the bridge, not the shared browser. A closed browser cannot redirect commands to another workspace. The copied-instruction flow remains available for an existing editor tab.

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

Versioned GitHub releases provide unsigned executables for Apple Silicon and Intel macOS, x64 Linux, and x64 Windows. Each release also provides checksums, a machine-readable manifest, the project license, and third-party license texts. See the [release process](docs/RELEASING.md) for the reviewed manual publication procedure.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Licensed under the [Apache License 2.0](LICENSE).
