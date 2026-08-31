# Regenerating third-party notices

The final checked-in notice must be rendered with `cargo-about` 0.9.2. The
version is fixed because license detection and built-in crate workarounds can
change between releases.

Install the reviewed version once:

```bash
cargo install --locked --version 0.9.2 --features cli cargo-about
```

After the dependency sources are present in the local Cargo cache, regenerate
without network access:

```bash
cargo about generate \
  --workspace \
  --all-features \
  --locked \
  --offline \
  --fail \
  --config tools/licenses/about.toml \
  --output-file THIRD_PARTY_NOTICES.html \
  tools/licenses/about.hbs
```

Review changes to package names, versions, selected licenses, and license text.
Do not globally accept a new license merely to make generation pass. A new or
changed license requires a dependency-level review and, when appropriate, a
crate-specific entry at the end of `about.toml`.

The generated file must be committed. The macOS bundler copies it into
`HearthAdvisor.app/Contents/Resources/Legal`.
