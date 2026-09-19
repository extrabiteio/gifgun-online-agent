# Release Process

Do not start a release without approval to upload executable files.

## Prepare the source

1. Update the generated files in `contract/` from the matching browser contract.
2. Update the package version in `Cargo.toml`.
3. Run the local validation commands.
4. Generate the third-party license report with cargo-about 0.9.2.
5. Review and merge the source change.
6. Confirm that native checks pass on macOS, Linux, and Windows.

Generate the license report:

```sh
cargo install cargo-about --version 0.9.2 --locked --features cli
cargo about generate about.hbs --locked --fail --output-file THIRD_PARTY_LICENSES.html
```

The report is a local review file. The release workflow generates it again from the tagged source.

## Create the source tag

Use the separate repository identity. Create a lightweight tag so that the tag does not contain a second author record.

```sh
version=$(cargo metadata --locked --no-deps --format-version 1 | python3 -c 'import json, sys; print(json.load(sys.stdin)["packages"][0]["version"])')
git tag "v${version}"
git push origin "v${version}"
```

The tag does not publish executable files.

## Publish the release

1. Open the **Release native agents** workflow.
2. Select **Run workflow**.
3. Enter the existing version tag.
4. Review the completed jobs and the release assets.

The workflow verifies that the tag, package version, source commit, and embedded contract agree. It builds four native executables and publishes:

- macOS Apple silicon
- macOS Intel
- Linux x64 with musl
- Windows x64
- SHA-256 checksums
- A machine-readable release manifest
- The project license
- Third-party license texts

The workflow does not publish to the GifGun download domain. Update the browser release pin and distribution workflow only after the GitHub release passes verification.
