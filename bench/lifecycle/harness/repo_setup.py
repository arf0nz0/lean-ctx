"""Repository setup: clone, checkout fixed snapshot, apply seed regressions."""

from __future__ import annotations

import json
import shutil
import subprocess
from pathlib import Path
from typing import Union


def load_lane(lane_path: Path) -> dict:
    return json.loads(lane_path.read_text())


def clone_repo(repo_url: str, target: Path, cache_dir: Path) -> None:
    """Clone via mirror cache for speed on repeat runs."""
    cache_dir.mkdir(parents=True, exist_ok=True)
    repo_name = repo_url.rstrip("/").split("/")[-1].replace(".git", "")
    mirror = cache_dir / f"{repo_name}.git"

    if not mirror.exists():
        print(f"  Cloning mirror: {repo_url}")
        subprocess.run(
            ["git", "clone", "--mirror", repo_url, str(mirror)],
            check=True, capture_output=True,
        )
    else:
        print(f"  Updating mirror: {mirror.name}")
        subprocess.run(
            ["git", "remote", "update"],
            cwd=str(mirror), check=True, capture_output=True,
        )

    if target.exists():
        shutil.rmtree(target)

    print(f"  Cloning from mirror to {target.name}")
    subprocess.run(
        ["git", "clone", str(mirror), str(target)],
        check=True, capture_output=True,
    )


def checkout_snapshot(repo: Path, commit: str) -> None:
    subprocess.run(
        ["git", "checkout", commit],
        cwd=str(repo), check=True, capture_output=True,
    )
    subprocess.run(
        ["git", "checkout", "-b", "broken-start"],
        cwd=str(repo), check=True, capture_output=True,
    )


def apply_seed_patches(repo: Path, lane: dict) -> None:
    """Apply seed regressions by text replacement in source files."""
    patches = lane.get("seed_patch_files", {})
    for filepath, spec in patches.items():
        target = repo / filepath
        if not target.exists():
            print(f"  WARNING: seed target not found: {filepath}")
            continue

        content = target.read_text()
        replacements = spec if isinstance(spec, list) else [spec]

        for repl in replacements:
            old = repl["find"]
            new = repl["replace"]
            count = content.count(old)
            if count == 0:
                print(f"  WARNING: seed anchor not found in {filepath}: {old[:60]}...")
                continue
            if count > 1:
                print(f"  WARNING: seed anchor found {count} times in {filepath}, replacing first")
            content = content.replace(old, new, 1)

        target.write_text(content)
        print(f"  Seeded regression in {filepath}")


def add_seed_files(repo: Path, lane: dict) -> None:
    """Write additional files (e.g. baseline test files) into the repo."""
    additions = lane.get("seed_add_files", {})
    for filepath, content in additions.items():
        target = repo / filepath
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content)
        print(f"  Added seed file: {filepath}")


def reinit_git(repo: Path) -> None:
    """Commit the seeded state as a clean broken-start root."""
    subprocess.run(
        ["git", "add", "-A"],
        cwd=str(repo), check=True, capture_output=True,
    )
    subprocess.run(
        ["git", "commit", "-m", "broken-start: composite seed applied",
         "--author", "benchmark <bench@lifecycle>"],
        cwd=str(repo), check=True, capture_output=True,
        env={**dict(__import__("os").environ), "GIT_COMMITTER_NAME": "benchmark",
             "GIT_COMMITTER_EMAIL": "bench@lifecycle"},
    )
    subprocess.run(
        ["git", "remote", "remove", "origin"],
        cwd=str(repo), capture_output=True,
    )


def install_deps(repo: Path, lane: dict) -> None:
    """Run dependency installation for the lane if configured."""
    deps_cmd = lane.get("setup_deps")
    if not deps_cmd:
        return
    print(f"  Installing deps: {deps_cmd}")
    subprocess.run(
        deps_cmd, shell=True, cwd=str(repo),
        capture_output=True, timeout=300,
    )


def setup_lane(
    lane: dict, run_dir: Path, cache_dir: Path
) -> Path:
    """Full lane setup: clone -> checkout -> seed -> deps. Returns repo path."""
    lane_id = lane["lane_id"]
    repo_dir = run_dir / "repo"

    print(f"\n[{lane_id}] Setting up repository...")
    clone_repo(lane["repo_url"], repo_dir, cache_dir)
    checkout_snapshot(repo_dir, lane["fixed_snapshot"])
    apply_seed_patches(repo_dir, lane)
    add_seed_files(repo_dir, lane)
    reinit_git(repo_dir)
    install_deps(repo_dir, lane)

    print(f"[{lane_id}] Repository ready at {repo_dir}")
    return repo_dir
