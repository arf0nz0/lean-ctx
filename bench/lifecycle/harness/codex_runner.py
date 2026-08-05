"""Codex CLI exec wrapper with token accounting from --json JSONL output."""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import time
from dataclasses import dataclass, field, asdict
from pathlib import Path
from typing import Optional


@dataclass
class TaskResult:
    task_id: str
    arm: str
    exit_code: int
    wall_time_s: float
    input_tokens: int = 0
    output_tokens: int = 0
    reasoning_tokens: int = 0
    cache_read_tokens: int = 0
    cache_write_tokens: int = 0
    total_tokens: int = 0
    error: Optional[str] = None
    tool_calls: list[str] = field(default_factory=list)

    @property
    def fresh_input_tokens(self) -> int:
        return self.input_tokens - self.cache_read_tokens

    @property
    def weighted_cost(self) -> int:
        """Cache-adjusted cost: cached tokens at 10% weight."""
        return self.fresh_input_tokens + self.output_tokens + int(self.cache_read_tokens * 0.1)

    def to_dict(self) -> dict:
        d = asdict(self)
        d["fresh_input_tokens"] = self.fresh_input_tokens
        d["weighted_cost"] = self.weighted_cost
        return d


def _ensure_path_complete(path: str) -> str:
    """Ensure PATH includes common tool directories."""
    extras = [
        str(Path.home() / ".local" / "bin"),
        str(Path.home() / "Library" / "Python" / "3.9" / "bin"),
        str(Path.home() / ".cargo" / "bin"),
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "/usr/local/go/bin",
    ]
    parts = path.split(":")
    for extra in extras:
        if extra not in parts and Path(extra).is_dir():
            parts.insert(0, extra)
    return ":".join(parts)


def build_env(home_dir: Path, arm: str) -> dict[str, str]:
    """Build isolated environment for a Codex exec run."""
    env = {}

    for key in ("PATH", "SHELL", "TERM", "LANG", "USER", "LOGNAME", "TMPDIR"):
        if key in os.environ:
            env[key] = os.environ[key]

    env["PATH"] = _ensure_path_complete(env.get("PATH", "/usr/bin:/bin"))

    env["HOME"] = str(home_dir)
    env["CODEX_HOME"] = str(home_dir / ".codex")

    if arm == "bare":
        env["LEAN_CTX_DISABLED"] = "1"
        env.pop("LEAN_CTX_ACTIVE", None)
        env.pop("LEAN_CTX_PROJECT_ROOT", None)
    else:
        for key in ("LEAN_CTX_ACTIVE", "LEAN_CTX_PROJECT_ROOT"):
            if key in os.environ:
                env[key] = os.environ[key]

    return env


def setup_codex_home(home_dir: Path, arm: str, repo_dir: Path) -> None:
    """Prepare a fresh HOME with Codex auth + optional lean-ctx hooks."""
    home_dir.mkdir(parents=True, exist_ok=True)
    codex_home = home_dir / ".codex"
    codex_home.mkdir(exist_ok=True)

    real_codex = Path.home() / ".codex"
    for auth_file in ("auth.json",):
        src = real_codex / auth_file
        if src.exists():
            shutil.copy2(src, codex_home / auth_file)

    real_path = _ensure_path_complete(os.environ.get("PATH", "/usr/bin:/bin"))
    (home_dir / ".zshrc").write_text(f'export PATH="{real_path}"\n')
    (home_dir / ".bashrc").write_text(f'export PATH="{real_path}"\n')
    (home_dir / ".profile").write_text(f'export PATH="{real_path}"\n')

    if arm == "leanctx":
        leanctx_bin = shutil.which("lean-ctx")
        if not leanctx_bin:
            raise RuntimeError("lean-ctx not found on PATH")

        hooks = {
            "hooks": {
                "PostToolUse": [
                    {
                        "hooks": [
                            {
                                "type": "command",
                                "command": f"{leanctx_bin} hook observe",
                                "timeout": 5,
                            }
                        ],
                        "matcher": ".*",
                    }
                ],
            }
        }
        (codex_home / "hooks.json").write_text(json.dumps(hooks, indent=2))

        config_toml = "[features]\nhooks = true\n"
        (codex_home / "config.toml").write_text(config_toml)

        leanctx_config = home_dir / ".config" / "lean-ctx"
        leanctx_config.mkdir(parents=True, exist_ok=True)
        (leanctx_config / "config.toml").write_text(
            'shadow_mode = true\ntool_surface = "shadow"\n'
        )


def parse_codex_jsonl(raw_output: bytes) -> dict:
    """Extract token usage from Codex --json JSONL output."""
    usage = {
        "input_tokens": 0,
        "output_tokens": 0,
        "reasoning_tokens": 0,
        "cache_read_tokens": 0,
        "cache_write_tokens": 0,
        "total_tokens": 0,
        "tool_calls": [],
    }

    for line in raw_output.decode("utf-8", errors="replace").splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue

        event_type = event.get("type", "")

        if "usage" in event:
            u = event["usage"]
            inp = u.get("input_tokens", 0)
            out = u.get("output_tokens", 0)
            usage["input_tokens"] += inp
            usage["output_tokens"] += out
            usage["reasoning_tokens"] += u.get("reasoning_output_tokens", u.get("reasoning_tokens", 0))
            usage["cache_read_tokens"] += u.get("cached_input_tokens", u.get("cache_read_input_tokens", 0))
            usage["cache_write_tokens"] += u.get("cache_write_input_tokens", u.get("cache_creation_input_tokens", 0))
            usage["total_tokens"] += u.get("total_tokens", inp + out)

        if event_type in ("tool_use", "item.started"):
            item = event.get("item", event)
            if item.get("type") == "command_execution":
                cmd = item.get("command", "")[:80]
                usage["tool_calls"].append(f"bash:{cmd}")
            elif event.get("tool"):
                usage["tool_calls"].append(event["tool"])

    return usage


def run_codex_task(
    task_id: str,
    prompt: str,
    cwd: Path,
    home_dir: Path,
    arm: str,
    timeout: int = 600,
    transcript_path: Optional[Path] = None,
) -> TaskResult:
    """Run a single task via `codex exec` and return structured results."""
    env = build_env(home_dir, arm)

    codex_bin = shutil.which("codex")
    if not codex_bin:
        return TaskResult(
            task_id=task_id, arm=arm, exit_code=-1, wall_time_s=0,
            error="codex not found on PATH",
        )

    cmd = [
        codex_bin, "exec",
        "--json",
        "-s", "workspace-write",
        "-C", str(cwd),
        "--dangerously-bypass-approvals-and-sandbox",
        "--dangerously-bypass-hook-trust",
        "--ephemeral",
        prompt,
    ]

    t0 = time.monotonic()
    try:
        proc = subprocess.run(
            cmd,
            capture_output=True,
            env=env,
            timeout=timeout,
            cwd=str(cwd),
        )
        exit_code = proc.returncode
        error_msg = None
    except subprocess.TimeoutExpired as exc:
        exit_code = -1
        error_msg = f"timeout after {timeout}s"
        proc = exc
    except Exception as exc:
        exit_code = -1
        error_msg = str(exc)
        proc = None

    wall_time = time.monotonic() - t0

    stdout = getattr(proc, "stdout", b"") or b""
    stderr = getattr(proc, "stderr", b"") or b""

    if transcript_path:
        transcript_path.parent.mkdir(parents=True, exist_ok=True)
        transcript_path.write_bytes(stdout)
        transcript_path.with_suffix(".stderr.log").write_bytes(stderr)

    usage = parse_codex_jsonl(stdout)

    return TaskResult(
        task_id=task_id,
        arm=arm,
        exit_code=exit_code,
        wall_time_s=round(wall_time, 2),
        input_tokens=usage["input_tokens"],
        output_tokens=usage["output_tokens"],
        reasoning_tokens=usage["reasoning_tokens"],
        cache_read_tokens=usage["cache_read_tokens"],
        cache_write_tokens=usage["cache_write_tokens"],
        total_tokens=usage["total_tokens"],
        error=error_msg,
        tool_calls=usage["tool_calls"],
    )
