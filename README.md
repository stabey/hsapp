# Hearth Advisor

Hearth Advisor is a macOS-first, open-source Hearthstone deck tracker and
decision-support overlay written in Rust. It reads the local logs exposed by
the game client, reconstructs the visible game state, and presents suggestions
without clicking, injecting into, or automating the game.

> [!WARNING]
> This project is under active development. It does not yet provide a
> Developer ID-signed and notarized release for general installation.

## What works today

- Deterministic parsing and replay of `Power.log` and `Decks.log`.
- Live deck tracking, match history, and a versioned local SQLite card store.
- A native GPUI main window and transparent macOS overlay.
- Strict legal-action projection from server-provided options.
- Deterministic simulation and advice with lethal search, alternative lines,
  confidence, rule coverage, and search-completeness evidence.
- Reproducible macOS `.app` assembly through the repository's `xtask`.

The architecture keeps observed facts, simulated outcomes, and uncertain
beliefs separate. The rationale is recorded in
[ADR-0003](docs/adr/0003-observed-sim-belief-separation.md).

## Platform and prerequisites

The desktop application currently supports macOS 13 or newer on Apple Silicon
and Intel Macs. The core crates are platform-neutral where practical, but the
Windows and Linux desktop integrations are not implemented.

You need the Rust toolchain selected by `rust-toolchain.toml`. A full Xcode
installation is not required because GPUI uses runtime shader compilation in
this repository.

## Build and run

```bash
cargo build --workspace --locked
cargo run --locked -p desktop
```

Application data is stored in
`~/Library/Application Support/HearthAdvisor`. Set `HS_DATA_DIR` to move the
whole data directory. Rules are loaded in this order:

1. The non-empty path in `HS_RULES_DIR`.
2. `Contents/Resources/rules` inside a bundled application.
3. `rules/` beneath the current working directory during development.

Do not edit resources inside a signed `.app`; doing so invalidates its code
signature.

## Build a macOS application

```bash
cargo xtask bundle
cargo xtask bundle --release
```

The resulting application is written beneath `target/debug/bundle` or
`target/release/bundle`. Local bundles use an ad-hoc signature. Gatekeeper can
therefore block a bundle downloaded by another person; Developer ID signing
and notarization are intentionally not part of the current release process.
See [ADR-0006](docs/adr/0006-distribution-signing-notarization.md).

## Quality checks

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --locked
```

## Repository map

| Path | Purpose |
| --- | --- |
| `apps/desktop` | Native desktop application, main window, and overlay |
| `crates/hs-log-*` | Log recording, tailing, parsing, and normalization |
| `crates/hs-observed-state` | Replayable model of facts visible in the logs |
| `crates/hs-card-*`, `crates/hs-decks` | Card data, deck import, and local storage |
| `crates/hs-effect-ir`, `crates/hs-pools`, `crates/hs-sim` | Rules, card pools, and deterministic simulation |
| `crates/hs-belief`, `crates/hs-planner`, `crates/hs-advisor-api` | Uncertainty, search, scoring, and advice contracts |
| `rules` | Versioned card-effect and pool definitions |
| `tests` | Synthetic and identity-sanitized replay fixtures |
| `docs/adr` | Architecture decision records |

## Contributing and security

Contributions are welcome. Read [CONTRIBUTING.md](CONTRIBUTING.md) before
opening a pull request, especially the rules for sanitizing game logs. Report
security issues privately as described in [SECURITY.md](SECURITY.md).

## License and third-party material

Original Hearth Advisor source code is licensed under the
[MIT License](LICENSE). That license does not relicense third-party code,
generated material, game data, card text, artwork, logs, names, or trademarks.
Their sources and terms are recorded in
[THIRD_PARTY_NOTICES.html](THIRD_PARTY_NOTICES.html).

Hearth Advisor is an independent community project. It is not affiliated with,
endorsed by, sponsored by, or approved by Blizzard Entertainment. Hearthstone,
Blizzard, and related names, card text, game data, artwork, and trademarks are
the property of their respective owners. No Blizzard game content is granted
under this repository's MIT License.
