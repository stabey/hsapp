# Contributing to Hearth Advisor

Thank you for helping improve Hearth Advisor. Small, focused changes with
tests and a clear explanation are the easiest to review.

## Before opening a pull request

1. Open an issue first for substantial features or architecture changes.
2. Keep observed facts, simulated outcomes, and uncertain beliefs in their
   existing layers; do not make UI code a second source of game state.
3. Add or update tests for behavior changes.
4. Run the repository quality checks:

   ```bash
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
   cargo test --workspace --locked
   ```

5. Update documentation and third-party notices when behavior, dependencies,
   generated data, or attribution changes.

## Never submit an unsanitized game log

`Power.log` and `Decks.log` files can contain persistent player identifiers,
BattleTags, account IDs, local paths, deck names, and other private data. A log
must be sanitized before it is attached to an issue or committed as a test
fixture.

- Replace every player name and BattleTag with stable synthetic values.
- Replace every `GameAccountId` component with stable synthetic numbers.
- Remove local usernames, paths, deck names, authentication data, and unrelated
  sessions.
- Scan the complete file after editing; changing only the first occurrence is
  not sufficient.
- State the source and license of an upstream fixture. Do not assume that a
  previously public log is safe to republish without sanitizing identities.
- Preserve line endings and event order when a fixture is used for replay or
  differential tests.

When in doubt, reproduce the behavior with a small synthetic fixture instead
of sharing a real log.

## Third-party and game material

Do not submit copied proprietary artwork, audio, fonts, or other game assets.
Code or data derived from another project must retain its provenance and
license notice. The repository's MIT License applies only to original Hearth
Advisor contributions and does not override third-party or Blizzard rights.

## Contribution license

Any contribution you intentionally submit for inclusion in Hearth Advisor is
provided under the repository's MIT License. By submitting a contribution, you
confirm that you have the right to license it on those terms. Material offered
under different terms will not be accepted unless the maintainers agree in
writing before submission and record the applicable terms explicitly.

## Security reports

Do not disclose a suspected vulnerability in a public issue. Follow
[SECURITY.md](SECURITY.md) instead.
