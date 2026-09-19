# Security Policy

## Report a vulnerability

Use a private GitHub Security Advisory for this repository. Do not open a public issue for a vulnerability.

Include:

- The affected version or commit
- The affected operating system
- Steps that reproduce the problem
- The security result that you expected
- The security result that occurred

Do not include active credentials, pairing instructions, session files, media, projects, or other private data. Use synthetic values in the report.

## Security boundaries

Changes must preserve these boundaries:

- Loopback-only network binding
- Exact browser-origin checks
- Explicit browser approval
- Separate browser and command credentials
- Expiring pairing instructions
- Exact editor-contract compatibility
- Private session storage
- No media bytes, project bytes, local file paths, credentials, or browser object URLs in command JSON or diagnostics
