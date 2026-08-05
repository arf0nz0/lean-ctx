"""Main benchmark runner: orchestrates lanes, arms, and sequential tasks.

Usage:
    python3 -m harness.run_lifecycle --run-id smoke --lane beets
    python3 -m harness.run_lifecycle --run-id v1
    python3 -m harness.run_lifecycle --run-id debug1 --lane fastify --arm leanctx
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

from . import BENCH_ROOT, LANES_DIR, CONFIG_PATH, ARMS
from .codex_runner import run_codex_task, setup_codex_home, TaskResult, _ensure_path_complete
from .repo_setup import load_lane, setup_lane


def load_config() -> dict:
    return json.loads(CONFIG_PATH.read_text())


def _verifier_env() -> dict[str, str]:
    """Build env for verifier subprocesses with complete PATH."""
    env = dict(os.environ)
    env["PATH"] = _ensure_path_complete(env.get("PATH", "/usr/bin:/bin"))
    return env


def run_verifier(repo: Path, verify_cmd: str) -> tuple[int, str]:
    """Run a task verifier and return (exit_code, output)."""
    try:
        proc = subprocess.run(
            verify_cmd, shell=True, cwd=str(repo),
            capture_output=True, timeout=120, text=True,
            env=_verifier_env(),
        )
        output = (proc.stdout + "\n" + proc.stderr).strip()
        return proc.returncode, output
    except subprocess.TimeoutExpired:
        return -1, "verifier timeout"
    except Exception as exc:
        return -1, str(exc)


def run_lane_arm(
    lane: dict,
    arm: str,
    run_dir: Path,
    config: dict,
    cache_dir: Path,
) -> list[dict]:
    """Run all tasks for one lane+arm combination. Returns per-task results."""
    lane_id = lane["lane_id"]
    arm_dir = run_dir / lane_id / arm

    meta_path = arm_dir / "meta.json"
    if meta_path.exists():
        print(f"\n[{lane_id}/{arm}] Already completed (meta.json exists), skipping.")
        return json.loads(meta_path.read_text())["tasks"]

    repo_dir = setup_lane(lane, arm_dir, cache_dir)
    home_dir = arm_dir / "home"
    setup_codex_home(home_dir, arm, repo_dir)

    timeout = config.get("timeouts", {}).get("task_seconds", 600)
    tasks = lane["tasks"]
    results: list[dict] = []

    for task in sorted(tasks, key=lambda t: t["order"]):
        task_id = task["id"]
        task_num = f"task-{task['order']:02d}"
        task_dir = arm_dir / task_num

        print(f"\n[{lane_id}/{arm}] Running {task_num}: {task_id}")
        print(f"  Class: {task['class']}")

        result = run_codex_task(
            task_id=task_id,
            prompt=task["prompt"],
            cwd=repo_dir,
            home_dir=home_dir,
            arm=arm,
            timeout=timeout,
            transcript_path=task_dir / "transcript.jsonl",
        )

        print(f"  Exit: {result.exit_code} | Wall: {result.wall_time_s}s")
        print(f"  Tokens: {result.total_tokens} total "
              f"({result.input_tokens} in, {result.output_tokens} out)")

        verify_exit, verify_output = run_verifier(repo_dir, task["verify_cmd"])
        task_passed = verify_exit == 0

        print(f"  Verify: {'PASS' if task_passed else 'FAIL'} (exit {verify_exit})")

        task_result = {
            **result.to_dict(),
            "verify_exit": verify_exit,
            "verify_passed": task_passed,
            "verify_output": verify_output[:2000],
        }
        results.append(task_result)

        (task_dir / "meta.json").parent.mkdir(parents=True, exist_ok=True)
        (task_dir / "meta.json").write_text(json.dumps(task_result, indent=2))

    meta = {
        "lane_id": lane_id,
        "arm": arm,
        "completed_at": datetime.now(timezone.utc).isoformat(),
        "tasks_passed": sum(1 for r in results if r["verify_passed"]),
        "tasks_total": len(results),
        "total_tokens": sum(r["total_tokens"] for r in results),
        "total_input_tokens": sum(r["input_tokens"] for r in results),
        "total_output_tokens": sum(r["output_tokens"] for r in results),
        "total_wall_time_s": round(sum(r["wall_time_s"] for r in results), 2),
        "tasks": results,
    }
    meta_path.parent.mkdir(parents=True, exist_ok=True)
    meta_path.write_text(json.dumps(meta, indent=2))

    return results


def main():
    parser = argparse.ArgumentParser(description="Lifecycle Benchmark Harness")
    parser.add_argument("--run-id", required=True, help="Run identifier (e.g. smoke, v1)")
    parser.add_argument("--lane", help="Single lane to run (beets/fastify/terraform)")
    parser.add_argument("--arm", help="Single arm to run (leanctx/bare)")
    parser.add_argument("--report", action="store_true", help="Generate report after run")
    args = parser.parse_args()

    config = load_config()
    runs_dir = BENCH_ROOT / config.get("runs_dir", "runs")
    run_dir = runs_dir / args.run_id
    cache_dir = BENCH_ROOT / config.get("repo_cache_dir", ".cache/repos")

    lane_ids = [args.lane] if args.lane else config.get("lanes", [])
    arms = [args.arm] if args.arm else list(ARMS)

    if not shutil.which("codex"):
        print("ERROR: codex not found on PATH", file=sys.stderr)
        sys.exit(1)

    print(f"=== Lifecycle Benchmark: run-id={args.run_id} ===")
    print(f"Lanes: {lane_ids}")
    print(f"Arms: {arms}")
    print(f"Output: {run_dir}")

    t0 = time.monotonic()

    for lane_id in lane_ids:
        lane_path = LANES_DIR / f"{lane_id}.json"
        if not lane_path.exists():
            print(f"ERROR: lane definition not found: {lane_path}", file=sys.stderr)
            sys.exit(1)
        lane = load_lane(lane_path)

        for arm in arms:
            run_lane_arm(lane, arm, run_dir, config, cache_dir)

    total_time = time.monotonic() - t0
    print(f"\n=== Benchmark complete in {total_time:.0f}s ===")

    if args.report:
        from .report import generate_report
        generate_report(run_dir)


if __name__ == "__main__":
    main()
