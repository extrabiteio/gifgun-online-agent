# Agent Instructions

## Written English

Use short, direct sentences for new or changed English text. Use one term for one concept. Do not change exact identifiers, protocol values, third-party names, or required legal text.

## Security boundaries

- Keep the server bound to loopback.
- Keep exact browser-origin checks and explicit browser approval.
- Keep browser and command credentials separate.
- Keep pairing instructions short-lived.
- Keep session data private on disk.
- Do not put credentials, media bytes, project bytes, local file paths, browser object URLs, or user data in command JSON, logs, errors, tests, or documentation.
- Use synthetic credentials and data in tests.

## Browser contract

- The browser owns editor state, validation, preview, projects, rendering, cancellation, and results.
- Treat `contract/agent-contract.json` and `contract/conformance-fixtures.json` as generated browser artifacts.
- Do not edit generated contract files by hand.
- Require an exact contract digest match.
- Update the native version and browser release pin with each contract change.

## Numeric thresholds

Do not add an arbitrary interval, delay, retry window, limit, or heuristic. Use lifecycle state and actual outcomes. Add a fixed boundary only when its product or protocol meaning is approved.

## Validation

Run the smallest relevant test during implementation. Before transfer, run:

```sh
cargo fmt -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

Run platform checks when native lifecycle, process, file, or transport behavior changes.

## Releases

Do not commit executable files. Publish only versioned executables built from a reviewed commit. Verify the native version and exact contract digest before publication.
