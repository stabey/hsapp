#!/usr/bin/env python3
"""对照 hs-log-core Packet 序列与 python-hslog（M1 R5 最小版）。

第一版只对照：种类序列、每种计数、TagChange (entity, tag, value) 集合抽样。
字段级全等不做。差异需人工判读，不作硬门槛。

用法:
    python3 tests/differential/run.py <power.log> [--rust-ndjson <path>]
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from collections import Counter
from pathlib import Path
from typing import Any, Iterable

COMPARE_KINDS = (
    "CreateGame",
    "FullEntity",
    "ShowEntity",
    "ChangeEntity",
    "HideEntity",
    "TagChange",
    "BlockStart",
    "BlockEnd",
    "Options",
    "Choices",
    "ChosenEntities",
    "SendOption",
    "SendChoices",
)

# hslog 树节点 → 对照种类。Block 单独摊平为 BlockStart + 子树 + BlockEnd。
HSLOG_KIND = {
    "CreateGame": "CreateGame",
    "FullEntity": "FullEntity",
    "ShowEntity": "ShowEntity",
    "ChangeEntity": "ChangeEntity",
    "HideEntity": "HideEntity",
    "TagChange": "TagChange",
    "Options": "Options",
    "Choices": "Choices",
    "ChosenEntities": "ChosenEntities",
    "SendOption": "SendOption",
    "SendChoices": "SendChoices",
}

TIMESTAMP_RE = re.compile(r"^([DWE]) ([\d:.]+) (.+)$")
CONTEXT = 3
TAGCHANGE_SAMPLE = 20
MAX_MISMATCHES = 15

REPO_ROOT = Path(__file__).resolve().parents[2]


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Diff hs-log-core Packet kinds against python-hslog"
    )
    parser.add_argument("powerlog", type=Path, help="Power.log 或 fixture 路径")
    parser.add_argument(
        "--rust-ndjson",
        type=Path,
        default=None,
        help="预先生成的 dump_packets NDJSON；缺省则现场 cargo run",
    )
    args = parser.parse_args()
    powerlog = args.powerlog.expanduser().resolve()
    if not powerlog.is_file():
        print(f"error: power.log not found: {powerlog}", file=sys.stderr)
        return 2

    try:
        rust_packets = load_rust_packets(powerlog, args.rust_ndjson)
        hslog_events = parse_hslog(powerlog)
    except HslogUnavailable as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2
    except Exception as exc:  # noqa: BLE001 — CLI 顶层：把解析失败打出来即可
        print(f"error: {exc}", file=sys.stderr)
        return 1

    rust_cmp = [item for item in rust_packets if item["kind"] in COMPARE_KINDS]
    hslog_cmp = [item for item in hslog_events if item["kind"] in COMPARE_KINDS]
    print_report(powerlog, rust_packets, rust_cmp, hslog_events, hslog_cmp)
    rust_kinds = [item["kind"] for item in rust_cmp]
    hslog_kinds = [item["kind"] for item in hslog_cmp]
    return 0 if rust_kinds == hslog_kinds else 1


class HslogUnavailable(RuntimeError):
    pass


def load_rust_packets(powerlog: Path, ndjson_path: Path | None) -> list[dict[str, Any]]:
    if ndjson_path is not None:
        text = ndjson_path.expanduser().resolve().read_text(encoding="utf-8")
    else:
        text = run_dump_packets(powerlog)
    packets: list[dict[str, Any]] = []
    for line_no, line in enumerate(text.splitlines(), start=1):
        stripped = line.strip()
        if not stripped:
            continue
        try:
            obj = json.loads(stripped)
        except json.JSONDecodeError as exc:
            raise RuntimeError(f"rust ndjson line {line_no}: {exc}") from exc
        packets.append({"kind": rust_kind(obj), "raw": obj})
    return packets


def run_dump_packets(powerlog: Path) -> str:
    cmd = [
        "cargo",
        "run",
        "-q",
        "-p",
        "hs-log-core",
        "--example",
        "dump_packets",
        "--",
        str(powerlog),
    ]
    result = subprocess.run(
        cmd,
        cwd=REPO_ROOT,
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
        raise RuntimeError(f"dump_packets failed: {detail}")
    return result.stdout


def rust_kind(obj: Any) -> str:
    if isinstance(obj, str):
        return obj
    if isinstance(obj, dict) and len(obj) == 1:
        return next(iter(obj))
    return "Unknown"


def parse_hslog(powerlog: Path) -> list[dict[str, Any]]:
    try:
        from hslog import LogParser
    except ImportError as exc:
        raise HslogUnavailable(
            "python-hslog is not installed. "
            "Create a venv and `pip install -r tests/differential/requirements.txt`."
        ) from exc

    parser = LogParser()
    with powerlog.open(encoding="utf-8", errors="replace") as handle:
        for raw in handle:
            prepared = prepare_hslog_line(raw)
            if prepared is None:
                continue
            parser.read_line(prepared)
    parser.flush()

    events: list[dict[str, Any]] = []
    skipped: Counter[str] = Counter()
    for tree in parser.games:
        flatten_hslog(tree.packets, events, skipped)
    if skipped:
        events.append({"kind": "_hslog_skipped", "raw": dict(skipped)})
    return events


def prepare_hslog_line(line: str) -> str | None:
    """跳过空行/注释；无 `D 21:35:47.xxx` 前缀时补一条占位时间戳。"""
    raw = line.rstrip("\n")
    stripped = raw.strip()
    if not stripped or stripped.startswith("#"):
        return None
    if TIMESTAMP_RE.match(raw):
        return raw + "\n"
    return f"D 00:00:00.0000000 {stripped}\n"


def flatten_hslog(
    packets: Iterable[Any],
    out: list[dict[str, Any]],
    skipped: Counter[str],
) -> None:
    for packet in packets:
        name = type(packet).__name__
        if name == "Block":
            out.append({"kind": "BlockStart", "raw": packet})
            flatten_hslog(getattr(packet, "packets", ()), out, skipped)
            out.append({"kind": "BlockEnd", "raw": packet})
            continue
        if name == "SubSpell":
            skipped[name] += 1
            flatten_hslog(getattr(packet, "packets", ()), out, skipped)
            continue
        mapped = HSLOG_KIND.get(name)
        if mapped is None:
            skipped[name] += 1
            continue
        out.append({"kind": mapped, "raw": packet})


def print_report(
    powerlog: Path,
    rust_all: list[dict[str, Any]],
    rust_cmp: list[dict[str, Any]],
    hslog_all: list[dict[str, Any]],
    hslog_cmp: list[dict[str, Any]],
) -> None:
    rust_kinds = [item["kind"] for item in rust_cmp]
    hslog_kinds = [item["kind"] for item in hslog_cmp]
    rust_counts = Counter(item["kind"] for item in rust_all)
    hslog_counts = Counter(item["kind"] for item in hslog_cmp)
    rust_unknown = rust_counts.get("Unknown", 0)
    hslog_skipped = next(
        (item["raw"] for item in hslog_all if item["kind"] == "_hslog_skipped"),
        {},
    )

    print(f"power.log: {powerlog}")
    print(
        f"scope: kind sequence + per-kind counts + TagChange (entity,tag,value) set; "
        f"GameState-equivalent only ({', '.join(COMPARE_KINDS)})"
    )
    print(
        f"totals: rust_all={len(rust_all)} rust_cmp={len(rust_cmp)} "
        f"hslog_cmp={len(hslog_cmp)} rust_unknown={rust_unknown} "
        f"hslog_skipped={hslog_skipped or '{}'}"
    )
    print()
    print("kind counts")
    print(f"{'kind':<16} {'rust':>8} {'hslog':>8} {'delta':>8}")
    kinds = list(COMPARE_KINDS)
    extras = sorted(
        (set(rust_counts) | set(hslog_counts)) - set(COMPARE_KINDS) - {"Unknown", "_hslog_skipped"}
    )
    for kind in kinds + extras:
        left = rust_counts.get(kind, 0)
        right = hslog_counts.get(kind, 0)
        delta = left - right
        mark = "  " if delta == 0 else " *"
        print(f"{kind:<16} {left:8d} {right:8d} {delta:8d}{mark}")
    print()

    mismatches = list(kind_mismatches(rust_kinds, hslog_kinds))
    if not mismatches:
        print("kind sequence: MATCH")
    else:
        print(f"kind sequence: {len(mismatches)} mismatch region(s)")
        for idx, (pos, rust_idx, hslog_idx) in enumerate(mismatches[:MAX_MISMATCHES], start=1):
            print()
            print(f"-- mismatch {idx} at aligned index {pos} --")
            print("rust ±3:")
            print_context(rust_kinds, rust_idx)
            print("hslog ±3:")
            print_context(hslog_kinds, hslog_idx)
        if len(mismatches) > MAX_MISMATCHES:
            print(f"... {len(mismatches) - MAX_MISMATCHES} more mismatch region(s) omitted")
    print()
    print_tagchange_sample(rust_cmp, hslog_cmp)


def kind_mismatches(
    rust_kinds: list[str], hslog_kinds: list[str]
) -> Iterable[tuple[int, int, int]]:
    """按位置逐项对齐；连续不一致合并成一个区域，报告该区域起点。"""
    limit = max(len(rust_kinds), len(hslog_kinds))
    i = 0
    while i < limit:
        left = rust_kinds[i] if i < len(rust_kinds) else None
        right = hslog_kinds[i] if i < len(hslog_kinds) else None
        if left == right:
            i += 1
            continue
        yield (i, min(i, len(rust_kinds) - 1) if rust_kinds else 0, min(i, len(hslog_kinds) - 1) if hslog_kinds else 0)
        i += 1
        while i < limit:
            left = rust_kinds[i] if i < len(rust_kinds) else None
            right = hslog_kinds[i] if i < len(hslog_kinds) else None
            if left == right:
                break
            i += 1


def print_context(kinds: list[str], index: int) -> None:
    if not kinds:
        print("  (empty)")
        return
    start = max(0, index - CONTEXT)
    end = min(len(kinds), index + CONTEXT + 1)
    for i in range(start, end):
        mark = ">" if i == index else " "
        print(f"  {mark} [{i}] {kinds[i]}")


def print_tagchange_sample(
    rust_cmp: list[dict[str, Any]], hslog_cmp: list[dict[str, Any]]
) -> None:
    rust_set = [tagchange_from_rust(item["raw"]) for item in rust_cmp if item["kind"] == "TagChange"]
    hslog_set = [tagchange_from_hslog(item["raw"]) for item in hslog_cmp if item["kind"] == "TagChange"]
    rust_counter = Counter(rust_set)
    hslog_counter = Counter(hslog_set)
    rust_only = rust_counter - hslog_counter
    hslog_only = hslog_counter - rust_counter
    print("TagChange (entity, tag, value) multiset")
    print(f"  rust={len(rust_set)} hslog={len(hslog_set)}")
    print(f"  rust_only={sum(rust_only.values())} hslog_only={sum(hslog_only.values())}")
    if rust_only:
        print("  rust-only sample:")
        for triple, count in list(rust_only.items())[:TAGCHANGE_SAMPLE]:
            suffix = f" x{count}" if count > 1 else ""
            print(f"    {triple}{suffix}")
    if hslog_only:
        print("  hslog-only sample:")
        for triple, count in list(hslog_only.items())[:TAGCHANGE_SAMPLE]:
            suffix = f" x{count}" if count > 1 else ""
            print(f"    {triple}{suffix}")
    if not rust_only and not hslog_only:
        print("  sets: MATCH")


def tagchange_from_rust(obj: Any) -> tuple[Any, Any, Any]:
    payload = obj.get("TagChange", obj) if isinstance(obj, dict) else {}
    return (payload.get("entity"), payload.get("tag"), payload.get("value"))


def tagchange_from_hslog(packet: Any) -> tuple[Any, Any, Any]:
    return (coerce_entity(getattr(packet, "entity", None)), coerce_int(getattr(packet, "tag", None)), coerce_int(getattr(packet, "value", None)))


def coerce_entity(entity: Any) -> Any:
    if entity is None:
        return None
    if isinstance(entity, int):
        return entity
    try:
        from hslog.player import coerce_to_entity_id

        return int(coerce_to_entity_id(entity))
    except Exception:
        entity_id = getattr(entity, "entity_id", None)
        if entity_id is not None:
            return entity_id
        return str(entity)


def coerce_int(value: Any) -> Any:
    if value is None:
        return None
    if isinstance(value, bool):
        return int(value)
    if isinstance(value, int):
        return value
    try:
        return int(value)
    except (TypeError, ValueError):
        return value


if __name__ == "__main__":
    sys.exit(main())
