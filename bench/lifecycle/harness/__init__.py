"""Lifecycle Benchmark Harness for lean-ctx.

Reproduces the OpenCode 3-lane sequential benchmark (Fastify/Beets/Terraform)
using Codex CLI, comparing lean-ctx treatment vs bare baseline.
"""

from pathlib import Path

BENCH_ROOT = Path(__file__).resolve().parent.parent
LANES_DIR = BENCH_ROOT / "lanes"
CONFIG_PATH = BENCH_ROOT / "config.json"
ARMS = ("leanctx", "bare")
