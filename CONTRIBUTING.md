# Contributing

Thank you for improving GifGun Online Agent.

## Before you start

Open an issue before you make a large change. Use a GitHub Security Advisory instead of an issue for a vulnerability.

Do not include credentials, pairing instructions, session files, media, projects, local paths, or personal information in an issue, test, commit, or log.

## Make a change

1. Create a branch from `main`.
2. Keep the change focused.
3. Add or update tests for changed behavior.
4. Use short, direct text in errors, documentation, and comments.
5. Run the local validation commands.
6. Open a pull request that explains the user outcome and security effect.

## Validate the change

```sh
cargo fmt -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

Test platform-specific behavior on each affected operating system. If dependencies change, regenerate and review the third-party license report as described in `docs/RELEASING.md`.

## Contract changes

The browser owns the editor contract. Do not edit files in `contract/` by hand.

A contract update must include:

- Both generated files in `contract/`
- Native conformance tests
- A compatible native version
- The matching browser release pin

Keep media bytes, project bytes, local file paths, credentials, and browser object URLs out of command JSON and diagnostics.

## License

By submitting a contribution, you agree that it is licensed under the Apache License 2.0.
