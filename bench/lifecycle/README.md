# Lifecycle Benchmark

Reproduces the OpenCode 3-lane sequential benchmark using Codex CLI to measure
lean-ctx treatment vs bare baseline. Directly tests fixes for #1403 (heredoc
reroute), #1404 (export parsing), #1405 (MCP instruction dedup).

## Quick Start

```bash
cd bench/lifecycle

# Smoke: single lane, both arms (~5 min)
python3 -m harness.run_lifecycle --run-id smoke --lane beets --report

# Full: all 3 lanes x 2 arms (~30 min)
python3 -m harness.run_lifecycle --run-id v1 --report

# Single arm for debugging
python3 -m harness.run_lifecycle --run-id debug1 --lane fastify --arm leanctx

# Report only (after a run)
python3 -m harness.report runs/v1
```

## Lanes

| Lane | Repo | Tasks | Key pattern tested |
|------|------|-------|--------------------|
| beets | beetbox/beets | 3 | Python heredoc via ctx_shell |
| fastify | fastify/fastify | 3 | Python heredoc + npm ecosystem |
| terraform | hashicorp/terraform | 3 | export PATH + Go ecosystem |

## Arms

| Arm | Description |
|-----|-------------|
| `leanctx` | Codex with lean-ctx hooks + MCP server |
| `bare` | Codex with `LEAN_CTX_DISABLED=1` (no lean-ctx) |

## Output

```
runs/<run-id>/
  <lane>/
    <arm>/
      meta.json           # Aggregate: pass rate, tokens, wall time
      repo/               # Working copy (post-run state)
      home/               # Isolated HOME used for run
      task-01/
        transcript.jsonl  # Full Codex --json output
        meta.json         # Per-task tokens + verify result
      task-02/ ...
      task-03/ ...
  report.md               # Comparison table
```

## Prerequisites

- `codex` CLI on PATH, authenticated (`codex login`)
- `lean-ctx` on PATH (`lean-ctx doctor` passes)
- Node.js (fastify), Python + uv (beets), Go (terraform)
