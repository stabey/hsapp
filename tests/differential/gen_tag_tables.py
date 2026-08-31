#!/usr/bin/env python3
"""Regenerate hs-log-core's static GameTag lookup tables.

Run from the repository root:
    source tests/differential/venv/bin/activate
    python3 tests/differential/gen_tag_tables.py

Outputs:
    crates/hs-log-core/src/game_tag_table.rs        name -> GameTag value
    crates/hs-log-core/src/game_tag_value_table.rs  (GameTag, enum name) -> value

The source is the MIT-licensed `python-hearthstone` package used by
python-hslog. Generated files retain that third-party provenance; see
THIRD_PARTY_NOTICES.html.
"""

from pathlib import Path

import hearthstone
from hearthstone import enums

ROOT = Path(__file__).resolve().parents[2]
VERSION = hearthstone.__version__


def gen_name_table() -> None:
    pairs = sorted({(t.name, t.value) for t in enums.GameTag})
    lines = [
        "//! Complete GameTag name-to-value table. Generated; do not edit.",
        "//!",
        f"//! Source: MIT-licensed `python-hearthstone` {VERSION},",
        "//! `hearthstone.enums.GameTag`.",
        "//! Upstream: https://github.com/HearthSim/python-hearthstone",
        "//! This table is derived third-party material, not original Hearth Advisor code.",
        "//! See `THIRD_PARTY_NOTICES.html` for attribution and license terms.",
        "//! Regenerate: `python3 tests/differential/gen_tag_tables.py`.",
        "",
        "/// 按名称排序的 (name, id) 表,`lookup` 用二分查找。",
        f"pub(crate) static GAME_TAG_TABLE: [(&str, i32); {len(pairs)}] = [",
        *[f'    ("{name}", {value}),' for name, value in pairs],
        "];",
        "",
        "pub(crate) fn lookup(name: &str) -> Option<i32> {",
        "    GAME_TAG_TABLE",
        "        .binary_search_by(|(n, _)| n.cmp(&name))",
        "        .ok()",
        "        .map(|idx| GAME_TAG_TABLE[idx].1)",
        "}",
    ]
    path = ROOT / "crates/hs-log-core/src/game_tag_table.rs"
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"{path}: {len(pairs)} entries")


def gen_value_table() -> None:
    entries = set()
    for tag, enum_cls in enums.TAG_TYPES.items():
        if not hasattr(enum_cls, "__members__"):
            continue
        for name, member in enum_cls.__members__.items():
            try:
                entries.add((int(tag), name, int(member.value)))
            except (TypeError, ValueError):
                pass
    entries = sorted(entries)
    lines = [
        "//! Complete (GameTag, enum-name)-to-value table. Generated; do not edit.",
        "//!",
        f"//! Source: MIT-licensed `python-hearthstone` {VERSION},",
        "//! `hearthstone.enums.TAG_TYPES`.",
        "//! Upstream: https://github.com/HearthSim/python-hearthstone",
        "//! This table is derived third-party material, not original Hearth Advisor code.",
        "//! See `THIRD_PARTY_NOTICES.html` for attribution and license terms.",
        "//! Regenerate: `python3 tests/differential/gen_tag_tables.py`.",
        "",
        f"pub(crate) static TAG_VALUE_TABLE: [(i32, &str, i32); {len(entries)}] = [",
        *[f'    ({tag}, "{name}", {value}),' for tag, name, value in entries],
        "];",
        "",
        "pub(crate) fn lookup(tag: i32, name: &str) -> Option<i32> {",
        "    TAG_VALUE_TABLE",
        "        .binary_search_by(|(t, n, _)| (*t, *n).cmp(&(tag, name)))",
        "        .ok()",
        "        .map(|idx| TAG_VALUE_TABLE[idx].2)",
        "}",
    ]
    path = ROOT / "crates/hs-log-core/src/game_tag_value_table.rs"
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"{path}: {len(entries)} entries")


if __name__ == "__main__":
    gen_name_table()
    gen_value_table()
