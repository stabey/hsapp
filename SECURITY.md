# Security Policy

## Supported versions

Hearth Advisor is in active development and has not published a stable release.
Security fixes are made on the latest `main` branch only.

## Reporting a vulnerability

Please use GitHub's **Report a vulnerability** form on the repository's
Security tab. This creates a private security advisory that only the reporter
and maintainers can read.

Include, when available:

- the affected commit or version;
- the operating system and architecture;
- reproduction steps or a minimal proof of concept;
- the impact you observed; and
- any suggested mitigation.

Do not include secrets, real BattleTags, `GameAccountId` values, unsanitized
game logs, or unrelated local files. If private vulnerability reporting is
temporarily unavailable, open a public issue containing no exploit or private
details and ask the maintainer to establish a private contact channel.

You should receive an acknowledgement within seven days. Timelines for a fix
and disclosure depend on severity and reproducibility. Please allow a
reasonable remediation period before public disclosure.

## Scope notes

Useful reports include vulnerabilities in log handling, local file access,
SQLite migrations, downloaded card data, update or packaging scripts, and
GitHub Actions workflows. General gameplay-policy questions and ordinary bugs
without a security impact belong in the public issue tracker.
