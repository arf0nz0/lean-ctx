"""Report generator: comparison tables, delta calculations, markdown output."""

from __future__ import annotations

import json
import sys
from datetime import datetime, timezone
from pathlib import Path


def load_run_data(run_dir: Path) -> dict[str, dict[str, dict]]:
    """Load all meta.json files. Returns {lane_id: {arm: meta}}."""
    data: dict[str, dict[str, dict]] = {}
    for meta_path in sorted(run_dir.rglob("meta.json")):
        if meta_path.parent.name in ("task-01", "task-02", "task-03"):
            continue
        meta = json.loads(meta_path.read_text())
        lane_id = meta.get("lane_id", meta_path.parent.parent.name)
        arm = meta.get("arm", meta_path.parent.name)
        data.setdefault(lane_id, {})[arm] = meta
    return data


def format_delta(treatment: int | float, bare: int | float) -> str:
    if bare == 0:
        return "N/A"
    pct = ((treatment - bare) / bare) * 100
    sign = "+" if pct > 0 else ""
    return f"{sign}{pct:.0f}%"


def generate_report(run_dir: Path) -> str:
    data = load_run_data(run_dir)

    if not data:
        return "No data found."

    lines: list[str] = []
    lines.append(f"# Lifecycle Benchmark Report")
    lines.append(f"")
    lines.append(f"Run: `{run_dir.name}` | Generated: {datetime.now(timezone.utc).strftime('%Y-%m-%d %H:%M UTC')}")
    lines.append("")

    lines.append("## Per-Lane Results")
    lines.append("")
    lines.append("| Lane | Arm | Pass | Total Tok | Input | Output | Reasoning | Cache Read | Weighted | Wall (s) |")
    lines.append("|------|-----|------|-----------|-------|--------|-----------|------------|----------|----------|")

    totals: dict[str, dict[str, int | float]] = {}

    for lane_id in sorted(data.keys()):
        for arm in ("leanctx", "bare"):
            if arm not in data[lane_id]:
                continue
            meta = data[lane_id][arm]
            passed = meta.get("tasks_passed", 0)
            total = meta.get("tasks_total", 0)
            tok = meta.get("total_tokens", 0)
            inp = meta.get("total_input_tokens", 0)
            out = meta.get("total_output_tokens", 0)
            wall = meta.get("total_wall_time_s", 0)

            reasoning = sum(t.get("reasoning_tokens", 0) for t in meta.get("tasks", []))
            cache_read = sum(t.get("cache_read_tokens", 0) for t in meta.get("tasks", []))
            weighted = sum(t.get("weighted_cost", 0) for t in meta.get("tasks", []))

            lines.append(
                f"| {lane_id} | {arm} | {passed}/{total} | "
                f"{tok:,} | {inp:,} | {out:,} | {reasoning:,} | "
                f"{cache_read:,} | {weighted:,} | {wall:.1f} |"
            )

            t = totals.setdefault(arm, {
                "passed": 0, "total": 0, "tokens": 0, "input": 0,
                "output": 0, "reasoning": 0, "cache_read": 0,
                "weighted": 0, "wall": 0,
            })
            t["passed"] += passed
            t["total"] += total
            t["tokens"] += tok
            t["input"] += inp
            t["output"] += out
            t["reasoning"] += reasoning
            t["cache_read"] += cache_read
            t["weighted"] += weighted
            t["wall"] += wall

    lines.append("")
    lines.append("## Aggregate Comparison")
    lines.append("")

    if "leanctx" in totals and "bare" in totals:
        lc = totals["leanctx"]
        br = totals["bare"]

        lines.append("| Metric | lean-ctx | Bare | Delta |")
        lines.append("|--------|----------|------|-------|")
        lines.append(f"| Tasks passed | {lc['passed']}/{lc['total']} | {br['passed']}/{br['total']} | |")
        lines.append(f"| Total tokens | {lc['tokens']:,} | {br['tokens']:,} | {format_delta(lc['tokens'], br['tokens'])} |")
        lines.append(f"| Input tokens | {lc['input']:,} | {br['input']:,} | {format_delta(lc['input'], br['input'])} |")
        lines.append(f"| Output tokens | {lc['output']:,} | {br['output']:,} | {format_delta(lc['output'], br['output'])} |")
        lines.append(f"| Reasoning | {lc['reasoning']:,} | {br['reasoning']:,} | {format_delta(lc['reasoning'], br['reasoning'])} |")
        lines.append(f"| Cache read | {lc['cache_read']:,} | {br['cache_read']:,} | {format_delta(lc['cache_read'], br['cache_read'])} |")
        lines.append(f"| Weighted cost | {lc['weighted']:,} | {br['weighted']:,} | {format_delta(lc['weighted'], br['weighted'])} |")
        lines.append(f"| Wall time (s) | {lc['wall']:.1f} | {br['wall']:.1f} | {format_delta(lc['wall'], br['wall'])} |")
    else:
        for arm, t in totals.items():
            lines.append(f"**{arm}:** {t['passed']}/{t['total']} passed, "
                         f"{t['tokens']:,} tokens, {t['wall']:.1f}s")

    lines.append("")
    lines.append("## Per-Task Detail")
    lines.append("")

    for lane_id in sorted(data.keys()):
        lines.append(f"### {lane_id}")
        lines.append("")
        lines.append("| Task | Arm | Pass | Tokens | Input | Output | Wall (s) | Error |")
        lines.append("|------|-----|------|--------|-------|--------|----------|-------|")

        for arm in ("leanctx", "bare"):
            if arm not in data[lane_id]:
                continue
            for idx, task in enumerate(data[lane_id][arm].get("tasks", []), 1):
                passed = "PASS" if task.get("verify_passed") else "FAIL"
                error = task.get("error", "") or ""
                tid = task.get("task_id", f"task-{idx}")
                lines.append(
                    f"| {tid} | "
                    f"{arm} | {passed} | {task.get('total_tokens', 0):,} | "
                    f"{task.get('input_tokens', 0):,} | {task.get('output_tokens', 0):,} | "
                    f"{task.get('wall_time_s', 0):.1f} | {error[:40]} |"
                )
        lines.append("")

    report = "\n".join(lines)

    report_path = run_dir / "report.md"
    report_path.write_text(report)
    print(f"\nReport written to {report_path}")

    return report


def main():
    if len(sys.argv) < 2:
        print("Usage: python3 -m harness.report <run-dir>")
        sys.exit(1)

    run_dir = Path(sys.argv[1])
    if not run_dir.exists():
        print(f"ERROR: {run_dir} not found", file=sys.stderr)
        sys.exit(1)

    report = generate_report(run_dir)
    print(report)


if __name__ == "__main__":
    main()
