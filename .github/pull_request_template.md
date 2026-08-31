## What changed

Describe the player-visible or developer-visible result. Link the related issue
when one exists.

## Validation

List the checks you ran and any important cases you verified.

## Checklist

- [ ] The change is focused and follows the existing architecture boundaries.
- [ ] I added or updated tests for behavior changes.
- [ ] `cargo fmt --all -- --check` passes.
- [ ] `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` passes.
- [ ] `cargo test --workspace --locked` passes.
- [ ] I updated documentation and third-party notices when needed.
- [ ] Every submitted log, screenshot, and fixture uses synthetic player names,
  BattleTags, account IDs, deck names, and local paths.
- [ ] I did not add secrets, proprietary game assets, or third-party material
  without provenance and compatible terms.
- [ ] I understand that my contribution is submitted under the repository's
  MIT License as described in `CONTRIBUTING.md`.
