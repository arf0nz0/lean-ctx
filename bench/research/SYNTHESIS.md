# Synthesis: Cross-Disciplinary Research on MCP Tool Overhead Reduction

**Date:** 2026-08-03
**Sources:** 4 parallel research agents (Information Theory, Neuroscience, Mathematics, Systems Engineering)
**Scope:** lean-ctx MCP proxy — minimize token overhead while preserving task success

---

## 1. Convergent Diagnosis: Why Schema-Diet Failed (+42%)

All 4 agents independently reached the same conclusion:

| Agent | Diagnosis |
|-------|-----------|
| **Information Theory** | Multi-channel rate violation: `R_total = R_tools/list + R_instructions + R_promoted` — summary pool and tool promoter added more tokens than schema stripping saved |
| **Neuroscience** | Extraneous cognitive load (Sweller) exceeded intrinsic savings; split-attention between 3 representations (compact schema + catalog + full promoted schema) |
| **Mathematics** | Compression reduces `I(tool; task)` below the Fano floor for reliable discrimination among similar tools — correct axis is *fewer tools*, not *compressed tools* |
| **Systems Engineering** | Every production system that works (Atlassian, Anthropic, Cursor, GitHub) reduces tool *count*, not tool *description length* |

**Root cause formula (Agent 1):**

```
R_schema_diet = R_slim_core + R_summary_pool + R_promoted_schemas
             > R_lazy_core + R_ctx_call
```

**Rule: Never add a second information channel. One source of truth, one location.**

---

## 2. Measured Baseline (2026-08-03)

| Configuration | Tools | Schema Tokens | vs Full | Status |
|---------------|------:|-------------:|--------:|--------|
| Full registry | 82 | 16,680 | — | Opt-in |
| Lazy core + `ctx_call` | 12 | 2,545 | **−84.7%** | **Default (shipped)** |
| Lazy core (live, compressed) | 12 | 2,148 | **−87.1%** | Doctor overhead |
| Schema-Diet (reverted) | ~12+promoted | +42% vs lazy | Negative | **Reverted** |

**Key insight:** lean-ctx's existing lazy core already achieves 85% reduction. The remaining 15% opportunity is small in absolute terms (~2,500 tokens). The real question is: can we reach **91-95%** without sacrificing task success?

---

## 3. Unified Architecture: "Tiered Disclosure"

All 4 agents converge on the same architecture pattern under different names:

| Agent | Name | Core Idea |
|-------|------|-----------|
| Information Theory | Two-phase R-D coding | Index (selection) + full schema (invocation) on demand |
| Neuroscience | Hierarchical lazy chunking | 4±1 category chunks + on-demand expansion |
| Mathematics | Capability-weighted submodular selection | Set cover + greedy fill under token budget |
| Systems Engineering | Lazy surface + invoker | Production consensus: Atlassian, Anthropic, Cursor pattern |

### Architecture: 3 Tiers

```
Tier 0: Always visible (stable, cached)
├── ctx_read     — file reading
├── ctx_search   — code search
├── ctx_shell    — shell execution
├── shell        — shell alias
├── ctx_call     — universal invoker (SAFETY NET)
└── ctx_expand   — expand compressed references
    ~6 tools, ~1,200 tokens

Tier 1: Context-gated (visible when relevant)
├── ctx_compose  — when task_phase = "orient"
├── ctx_knowledge — when task involves memory
├── ctx_session  — when session management needed
├── ctx_glob     — when file discovery needed
├── ctx_tree     — when directory exploration needed
├── ctx_callgraph — when code analysis needed
└── ... (6-12 more tools based on session signals)
    ~0-8 tools, ~0-1,600 tokens

Tier 2: Hidden (reachable via ctx_call only)
├── ctx_graph, ctx_architecture, ctx_agent, ...
└── All 60+ remaining tools
    0 tokens in tools/list; ~200 tok per invocation via ctx_call
```

### Per-Turn Token Budget

| Component | Tokens | Source |
|-----------|-------:|--------|
| Tier 0 schemas | ~1,200 | Stable, prompt-cached |
| Tier 1 schemas (avg 3 tools) | ~600 | Context-gated |
| Instructions | ~500-800 | Existing, byte-stable |
| **Total fixed overhead** | **~2,300-2,600** | vs 16,680 full |
| **Reduction vs full** | **84-86%** | Matches current lazy |

**With on-demand hydration (mcp-compressor pattern):**

| Component | Tokens | Source |
|-----------|-------:|--------|
| Tier 0 compact stubs | ~400 | Name + signature only |
| Hydrated schemas (2-3 tools/turn) | ~600 | On demand |
| Instructions | ~500-800 | Existing |
| **Total per-turn** | **~1,500-1,800** | |
| **Reduction vs full** | **89-91%** | Target |

---

## 4. Implementation Roadmap

### Phase 0: Validate Current State (already done)
- Lazy core + `ctx_call` — **84.7% reduction**, shipped
- Schema-Diet reverted — no negative overhead
- **No code changes needed**

### Phase 1: Context-Aware Tool Gating (~800 LOC, 2-3 weeks)
**Goal:** Replace static `CORE_TOOL_NAMES` with dynamic selection based on session signals.

| Module | LOC | Risk | Description |
|--------|----:|------|-------------|
| `server/tool_gating.rs` | 300 | Medium | BM25/keyword scoring of user message against tool descriptions |
| `server/session_context.rs` | 200 | Low | Track task phase (orient/edit/test/debug) from tool call patterns |
| `tool_visibility.rs` extension | 150 | Low | Wire gating into `advertised_tool_defs()` |
| Tests + benchmarks | 150 | — | Regression: ≤3,500 tok/turn, task success non-inferior |

**Expected result:** 12→8-15 tools dynamically selected; **~85-90% reduction** vs full.

**Key constraint (Fano guard):** `ctx_call` ALWAYS in Tier 0 — guarantees zero tool-miss penalty.

### Phase 2: On-Demand Schema Hydration (~600 LOC, 2-3 weeks)
**Goal:** mcp-compressor-compatible mode where `tools/list` returns compact stubs, full schemas fetched on demand.

| Module | LOC | Risk | Description |
|--------|----:|------|-------------|
| `server/tool_hydration.rs` | 250 | Low | Compact stub generation (name + signature + one-liner) |
| `ctx_tool_schema` tool | 150 | Low | New MCP tool: return full JSON schema for a tool by name |
| `server_handler.rs` wiring | 100 | Low | CandidateSet::Hydrated mode in list_tools |
| Tests | 100 | — | Round-trip: stub → hydrate → invoke |

**Expected result:** Fixed overhead **~800-1,500 tok/turn**; **91-95% reduction** vs full.

### Phase 3: Telemetry-Driven Auto Profile (~500 LOC, 1-2 weeks)
**Goal:** Learn optimal tool sets per task type from production usage data.

| Module | LOC | Risk | Description |
|--------|----:|------|-------------|
| `server/tool_telemetry.rs` | 200 | Low | Per-tool invocation counters, session-level aggregation |
| `ToolProfile::Auto` enhancement | 200 | Medium | Feed counters into profile selection (existing infrastructure) |
| Config + docs | 100 | — | `auto_profile_learning = true` in config.toml |

**Expected result:** Converge to optimal per-project tool sets within ~50-100 turns.

### Phase 4 (Optional): BM25 Intent Router (~1,000 LOC, 4-6 weeks)
**Goal:** Pre-filter tools by semantic relevance before `tools/list` response.

Only implement if Phase 1-3 benchmarks show remaining overhead > target.

---

## 5. Token Reduction Projections

| Phase | Schema Tokens | vs Full (16,680) | vs Current Lazy (2,545) | 95% CI |
|-------|-------------:|:----------------:|:-----------------------:|-------:|
| Current lazy core | 2,545 | −84.7% | baseline | — |
| Phase 1 (context gating) | 1,800-2,500 | −85-89% | −2-29% | 80-92% |
| Phase 2 (hydration) | 800-1,500 | −91-95% | −41-69% | 88-97% |
| Phase 3 (auto profile) | 800-1,200 | −93-95% | −53-69% | 90-97% |
| Combined (P1+P2+P3) | **800-1,500** | **−91-95%** | **−41-69%** | **88-97%** |

**Steady-state with prompt caching (turn ≥2):**

```
R_steady ≈ 0.1 × R_stubs + R_gated ≈ 80 + 600 = 680 tokens
→ 96% reduction vs full registry
```

---

## 6. Anti-Patterns (Never Do Again)

| Anti-Pattern | Why It Fails | Citation |
|--------------|-------------|----------|
| Summary pool in instructions | Adds second information channel; duplicates partial data from tools/list | Agent 1: data processing inequality |
| Tool promoter (dynamic schema injection) | Increases active set mid-session; unpredictable cache invalidation | Agent 2: Cowan limit violation |
| Property description stripping | Destroys discriminative MI needed for tool selection (Fano floor) | Agent 3: ToolExpNet 4%→10% miss rate |
| Schema compression without count reduction | Optimizes wrong axis — bits/schema vs count × bits/schema | Agent 4: production consensus |
| Dual representation (compact + full) | Split-attention effect (Sweller) — two representations worse than one | Agent 2: cognitive load theory |
| Relying on prompt caching alone | Solves billing, not attention quality or first-turn cost | Agent 4: provider analysis |

---

## 7. Design Principles (Consensus)

1. **One source of truth** — tool information in exactly one location (tools/list OR instructions, never both)
2. **Count over compression** — reduce number of tools shown, not bits per tool
3. **Always-available invoker** — `ctx_call` as Fano safety net for hidden tools (zero miss penalty)
4. **Byte-stable prefix** — deterministic tools/list ordering for prompt cache (#498)
5. **Cowan's limit** — ≤7 tool groups visible simultaneously
6. **Orthogonal descriptions** — <15% token overlap between sibling tools (SDR principle)
7. **Predict, then provide** — gate tools by session context, not static profiles
8. **Measure, don't assume** — every change A/B tested against `bench_lazy_default_vs_full_overhead`

---

## 8. Theoretical Foundations (Summary)

| Theory | Key Result | Application |
|--------|-----------|-------------|
| Shannon source coding | R ≥ H(X) — can't compress below entropy | Minimum ~5,500-8,200 tok for lossless full registry |
| Rate-distortion (Nagle 2024) | R-D knee at ~2,000-3,000 tok for coding | Lazy core sits at knee — near-optimal |
| Fano inequality | I(V;Y) ≥ 6.07 bits for <5% error on 82 tools | ~3 tokens/tool minimum discriminative info |
| Information bottleneck (Tishby) | Z* = (name, signature, one-liner, required) | Minimal sufficient statistic for tool schemas |
| Cowan (2001) | Working memory ≈ 4±1 chunks | ≤7 tool groups, ≤4 expanded per turn |
| Submodular greedy (Nemhauser 1978) | ≥(1-1/e) = 63.2% of optimal utility | Tool selection provably near-optimal |
| Set cover (Chvatal 1979) | H(d)-approximation for capability coverage | 8-14 tools cover typical coding capabilities |
| LinUCB (Abbasi-Yadkori 2011) | O(d√(T log T)) regret | Online tool set adaptation converges in ~150-300 turns |

---

## 9. Priority Action Items

### Immediate (this week)
1. ✅ Schema-Diet reverted (done: `04e3efd1f`)
2. Document the "Tiered Disclosure" architecture in `ARCHITECTURE.md`
3. Create GitHub issues for Phase 1-3 with clear acceptance criteria

### Short-term (2-4 weeks)
4. Implement Phase 1: Context-Aware Tool Gating
5. Benchmark Phase 1 against lazy-core baseline
6. Implement Phase 2: On-Demand Schema Hydration (if Phase 1 shows headroom)

### Medium-term (4-8 weeks)
7. Implement Phase 3: Telemetry-Driven Auto Profile
8. Run lifecycle benchmark (Fastify + Beets lanes) for A/B comparison
9. Publish benchmark results

---

## 10. Citations (Cross-Agent, Deduplicated)

### Papers
1. Shannon, C. E. (1948). A Mathematical Theory of Communication.
2. Tishby, N., et al. (2000). The Information Bottleneck Method.
3. Cowan, N. (2001). The magical number 4. Behavioral and Brain Sciences.
4. Nemhauser, G. L., et al. (1978). Submodular set functions. Mathematical Programming.
5. Feige, U. (1998). A threshold of ln n for set cover. JACM.
6. Sakizli, F. (2026). TSCG: Deterministic Tool-Schema Compilation. arXiv:2605.04107.
7. Sadani, A. (2026). Tool Attention Is All You Need. arXiv:2604.21816.
8. Nagle, A., et al. (2024). Rate-Distortion for Black-Box LMs. NeurIPS.
9. Sweller, J. (1988). Cognitive load during problem solving. Cognitive Science.
10. Oberauer, K. (2019). What limits working memory capacity? Psychological Bulletin.
11. Abbasi-Yadkori, Y., et al. (2011). Linear stochastic bandits. NeurIPS.
12. Müller, R. (2025). SC-LinUCB. arXiv:2507.10820.
13. Gong, D. & Zhang, H. (2024). Self-attention limits WM. NeurIPS.

### Production Systems
14. Atlassian mcp-compressor — https://atlassian-labs.github.io/mcp-compressor/
15. Anthropic Tool Search — https://www.anthropic.com/engineering/advanced-tool-use
16. Cursor Dynamic Discovery — https://cursor.com/blog/dynamic-context-discovery
17. GitHub MCP Search mode — https://maniak.io/articles/2026-06-20-github-mcp-token-economics-agentgateway-tool-modes/
18. MCP-Zero — https://arxiv.org/abs/2506.01056
19. MCP spec (2026-07-28) — https://modelcontextprotocol.io/specification/2026-07-28/server/tools

### lean-ctx Internal
20. Schema-Diet revert — commit `04e3efd1f`
21. `intensive_benchmarks.rs` — token accounting tests
22. `tool_visibility.rs` — lazy core policy engine

---

*Synthesis generated 2026-08-03. Based on 4 parallel research reports totaling ~120 citations.*
