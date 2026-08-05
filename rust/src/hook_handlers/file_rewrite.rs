//! Command-rewrite decision logic for the `hook rewrite` entry point.
//!
//! Extracted from `hook_handlers::mod` (#660/#966 LOC gate). Search/dir-list
//! rewriting lives in the sibling `search_rewrite` module; this one owns the
//! file-read (cat/head/tail/Get-Content) rewrites, compound-command wrapping,
//! and the `rewrite_candidate` dispatch every rewrite entry point (Cursor,
//! Codex, Copilot, the inline CLI) funnels through.

use super::search_rewrite::{rewrite_dir_list_command, rewrite_search_command};
use super::{
    HOOK_STDIN_TIMEOUT, build_dual_allow_output, build_dual_rewrite_output, dedup, is_disabled,
    is_shell_tool, payload, read_stdin_with_timeout, resolve_binary, shell_quote, shell_tokenize,
};
use crate::compound_lexer;
use crate::core::debug_log::{self, Route};
use crate::rewrite_registry;

/// Decide the rewrite hook's stdout (a rewrite or an allow-passthrough) without
/// printing, so `handle_rewrite` can run it under the fail-open timeout (#1035).
pub(super) fn compute_rewrite() -> String {
    if is_disabled() {
        return build_dual_allow_output();
    }
    // Shadow-only surface: native Shell passes through without rewrite
    if super::is_shadow_surface_active() {
        return build_dual_allow_output();
    }
    let binary = resolve_binary();
    let Some(input) = read_stdin_with_timeout(HOOK_STDIN_TIMEOUT) else {
        return build_dual_allow_output();
    };

    let Ok(v) = serde_json::from_str::<serde_json::Value>(&input) else {
        tracing::warn!("[hook rewrite] invalid JSON payload, allowing passthrough");
        return build_dual_allow_output();
    };

    // Resolve across host shapes: Claude/Cursor send snake_case `tool_name` +
    // `tool_input`; Copilot CLI sends camelCase `toolName` + `toolArgs` (a
    // JSON-encoded string). Before #551 only the snake_case path was read.
    let Some(tool_name) = payload::resolve_tool_name(&v) else {
        return build_dual_allow_output();
    };

    if !is_shell_tool(&tool_name) {
        return build_dual_allow_output();
    }

    let tool_args = payload::resolve_tool_args(&v);
    let Some(cmd) = payload::resolve_command(&v, tool_args.as_ref()) else {
        return build_dual_allow_output();
    };

    // #1032: Cursor fires preToolUse twice. Dedup on a PID-independent key (tool +
    // command) so the second fire replays the decision instead of re-logging.
    let key_material = format!("{tool_name}\u{0}{cmd}");
    dedup::deduped("rewrite", &key_material, || {
        if let Some(rewritten) = rewrite_candidate(&cmd, &binary) {
            debug_log::log_hook_decision(
                "rewrite",
                &tool_name,
                Route::LeanCtx,
                &cmd,
                "rewritable command",
            );
            build_dual_rewrite_output(tool_args.as_ref(), &rewritten)
        } else if needs_enforcement_wrap(&cmd) {
            // #1408: Commands that bypass compression-routing but violate the
            // shell allowlist must still be wrapped for enforcement. Without
            // this, compound commands (`true && docker --version`) and
            // unconditionally-blocked builtins (`eval ...`) skip the allowlist
            // when the hook passes them through to the native shell.
            debug_log::log_hook_decision(
                "rewrite",
                &tool_name,
                Route::LeanCtx,
                &cmd,
                "enforcement wrap (allowlist violation)",
            );
            build_dual_rewrite_output(tool_args.as_ref(), &wrap_single_command(&cmd, &binary))
        } else {
            debug_log::log_hook_decision(
                "rewrite",
                &tool_name,
                Route::Native,
                &cmd,
                rewrite_skip_reason(&cmd),
            );
            build_dual_allow_output()
        }
    })
}

/// Human-readable reason a shell command was left to the native tool. Mirrors
/// the `None` branches of [`rewrite_candidate`] so #520's debug log can explain
/// *why* a call fell back to native instead of routing through lean-ctx.
pub(super) fn rewrite_skip_reason(cmd: &str) -> &'static str {
    if cmd.starts_with("lean-ctx ") {
        "already a lean-ctx command"
    } else if cmd.contains("<<") {
        "heredoc cannot be rewritten safely"
    } else if is_compound(cmd) && !crate::core::shell_allowlist::passes_enforced(cmd) {
        "compound pipes/chains into a non-allowlisted or interpreter sink — left raw for the agent shell"
    } else {
        "not a known read/search/list command"
    }
}

pub(super) fn is_rewritable(cmd: &str) -> bool {
    rewrite_registry::is_rewritable_command(cmd)
}

/// #1408: True when a command must be wrapped in `lean-ctx -c` purely for shell
/// allowlist enforcement, even though it was not selected for compression routing.
///
/// Conditions: shell security is active (not `Off`), the command would fail the
/// allowlist, and it can survive the quoting round-trip (no heredocs).
fn needs_enforcement_wrap(cmd: &str) -> bool {
    use crate::core::shell_allowlist::{ShellSecurity, passes_enforced};

    if ShellSecurity::resolve() == ShellSecurity::Off {
        return false;
    }
    if cmd.contains("<<") {
        return false;
    }
    if cmd.starts_with("lean-ctx ") {
        return false;
    }
    !passes_enforced(cmd)
}

/// True when `cmd` carries a top-level shell operator (`&&`, `||`, `;`, `|`),
/// i.e. it is a compound/pipeline rather than a single command. Compounds are
/// handled authoritatively by [`build_rewrite_compound`]; this guards the
/// single-command `is_rewritable` fallback in [`rewrite_candidate`] so a
/// compound the compound-handler declined is never re-wrapped whole.
fn is_compound(cmd: &str) -> bool {
    compound_lexer::split_compound(cmd)
        .iter()
        .any(|s| matches!(s, compound_lexer::Segment::Operator(_)))
}

pub(super) fn wrap_single_command(cmd: &str, binary: &str) -> String {
    if cfg!(windows) {
        let escaped = cmd.replace('"', "\\\"");
        format!("{binary} -c \"{escaped}\"")
    } else {
        let shell_escaped = cmd.replace('\'', "'\\''");
        format!("{binary} -c '{shell_escaped}'")
    }
}

/// Quote-aware check for stdout file redirects (`>`, `>>`).
/// Returns `true` when the command contains an unquoted `>` that targets a
/// real file (not `/dev/null`, not `>&N` fd-duplication, not `2>`).
fn has_stdout_file_redirect(cmd: &str) -> bool {
    let bytes = cmd.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut in_single_quote = false;
    let mut in_double_quote = false;

    while i < len {
        let c = bytes[i];
        if c == b'\\' && !in_single_quote {
            i += 2;
            continue;
        }
        if c == b'\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
        } else if c == b'"' && !in_single_quote {
            in_double_quote = !in_double_quote;
        } else if c == b'>' && !in_single_quote && !in_double_quote {
            // Skip stderr redirect `2>`
            if i > 0 && bytes[i - 1] == b'2' {
                i += 1;
                continue;
            }
            let target_start = if i + 1 < len && bytes[i + 1] == b'>' {
                i + 2 // >>
            } else {
                i + 1 // >
            };
            let target: String = cmd[target_start..]
                .trim_start()
                .chars()
                .take_while(|c| !c.is_whitespace())
                .collect();
            // /dev/null and fd-duplication are not file redirects.
            if target == "/dev/null" || target == "/dev/stdout" || target == "/dev/stderr" {
                i += 1;
                continue;
            }
            if let Some(fd) = target.strip_prefix('&')
                && !fd.is_empty()
                && (fd == "-" || fd.chars().all(|c| c.is_ascii_digit()))
            {
                i += 1;
                continue;
            }
            if !target.is_empty() {
                return true;
            }
        }
        i += 1;
    }
    false
}

pub(super) fn rewrite_candidate(cmd: &str, binary: &str) -> Option<String> {
    if cmd.starts_with("lean-ctx ") || cmd.starts_with(&format!("{binary} ")) {
        return None;
    }

    // Heredocs cannot survive the quoting round-trip through `lean-ctx -c '...'`.
    // Newlines get escaped, breaking the heredoc syntax entirely (GitHub #140).
    if cmd.contains("<<") {
        return None;
    }

    // If the command has a LEAN_CTX_DISABLED or LEAN_CTX_NO_HOOK env-prefix,
    // the agent explicitly wants raw execution. Wrapping it in `lean-ctx -c`
    // would bury the flag inside a string literal where is_disabled() can't
    // see it. Skip rewrite entirely. (#1320)
    {
        let stripped = crate::rewrite_registry::strip_env_prefix(cmd);
        if stripped.len() != cmd.len() {
            let prefix_part = &cmd[..cmd.len() - stripped.len()];
            if prefix_part.contains("LEAN_CTX_DISABLED") || prefix_part.contains("LEAN_CTX_NO_HOOK")
            {
                return None;
            }
        }
    }

    // File redirects (`cmd > out`, `cmd >> log`) mean the output is captured
    // as data, not read by the agent. Wrapping in lean-ctx -c would either:
    // (a) compress stdout before the redirect writes it to disk, or
    // (b) add quoting overhead that can break redirect target paths.
    // Let the native shell handle the redirect directly. (#1303)
    if has_stdout_file_redirect(cmd) {
        return None;
    }

    if let Some(rewritten) = rewrite_file_read_command(cmd, binary) {
        return Some(rewritten);
    }

    if let Some(rewritten) = rewrite_search_command(cmd, binary) {
        return Some(rewritten);
    }

    if let Some(rewritten) = rewrite_dir_list_command(cmd, binary) {
        return Some(rewritten);
    }

    if let Some(rewritten) = build_rewrite_compound(cmd, binary) {
        return Some(rewritten);
    }

    // Single-command fallback only. A compound that `build_rewrite_compound`
    // declined (tricky pipe/chain sink, or no rewritable segment) must NOT be
    // re-wrapped here: wrapping the whole string in `lean-ctx -c '…'` would newly
    // subject its sink to the allowlist gate and could block a command the
    // agent's shell ran fine before (#589). Compounds are authoritative above.
    if !is_compound(cmd) && is_rewritable(cmd) {
        return Some(wrap_single_command(cmd, binary));
    }

    None
}

/// Rewrites cat/head/tail to lean-ctx read with appropriate arguments.
/// Only rewrites simple single-file reads within the project scope.
pub(super) fn rewrite_file_read_command(cmd: &str, binary: &str) -> Option<String> {
    // Unix file-read commands come from the central registry; PowerShell-native
    // cmdlets (Get-Content/gc) are detected here so they are not added to the POSIX
    // shell-alias/registry surface (#561).
    if !rewrite_registry::is_file_read_command(cmd) && !is_powershell_file_read(cmd) {
        return None;
    }

    // Compound commands (pipes, chains) should not be rewritten as file reads.
    if cmd.contains('|') || cmd.contains("&&") || cmd.contains("||") || cmd.contains(';') {
        return None;
    }

    // Shell redirections indicate complex usage — don't rewrite.
    if cmd.contains(">&") || cmd.contains(">>") || cmd.contains(" >") {
        return None;
    }

    let parts = shell_tokenize(cmd);
    if parts.len() < 2 {
        return None;
    }

    match parts[0].as_str() {
        "cat" => {
            let path = parts[1..].join(" ");
            if is_outside_project_path(&path) {
                return None;
            }
            Some(format!("{binary} read {}", shell_quote(&path)))
        }
        "head" => {
            let refs: Vec<&str> = parts[1..].iter().map(String::as_str).collect();
            let (n, path) = parse_head_tail_args(&refs);
            let path = path?;
            if is_outside_project_path(path) {
                return None;
            }
            let qp = shell_quote(path);
            match n {
                Some(lines) => Some(format!("{binary} read {qp} -m lines:1-{lines}")),
                None => Some(format!("{binary} read {qp} -m lines:1-10")),
            }
        }
        "tail" => {
            let refs: Vec<&str> = parts[1..].iter().map(String::as_str).collect();
            let (n, path) = parse_head_tail_args(&refs);
            let path = path?;
            if is_outside_project_path(path) {
                return None;
            }
            let qp = shell_quote(path);
            let lines = n.unwrap_or(10);
            Some(format!("{binary} read {qp} -m lines:-{lines}"))
        }
        "Get-Content" | "gc" => rewrite_get_content(&parts, binary),
        _ => None,
    }
}

/// True if the command is a PowerShell-native file-read cmdlet (`Get-Content`/`gc`).
fn is_powershell_file_read(cmd: &str) -> bool {
    matches!(cmd.split_whitespace().next(), Some("Get-Content" | "gc"))
}

/// Maps `Get-Content`/`gc` to `lean-ctx read`, honoring `-Path`/`-LiteralPath`, the
/// positional path, `-TotalCount`/`-Head`/`-First` (first N lines) and `-Tail`/`-Last`
/// (last N lines). PowerShell parameter names are case-insensitive. Any other flag, a
/// missing path, multiple files, or both head+tail makes it pass through (conservative,
/// mirroring the Unix cat/head/tail handling).
fn rewrite_get_content(parts: &[String], binary: &str) -> Option<String> {
    let mut path: Option<String> = None;
    let mut head_n: Option<u64> = None;
    let mut tail_n: Option<u64> = None;
    let mut i = 1;
    while i < parts.len() {
        if let Some(flag) = parts[i].strip_prefix('-') {
            let value = parts.get(i + 1);
            match flag.to_ascii_lowercase().as_str() {
                "path" | "literalpath" => path = Some(value?.clone()),
                "totalcount" | "head" | "first" => head_n = Some(value?.parse().ok()?),
                "tail" | "last" => tail_n = Some(value?.parse().ok()?),
                _ => return None,
            }
            i += 2;
        } else if path.is_none() {
            path = Some(parts[i].clone());
            i += 1;
        } else {
            return None;
        }
    }
    let path = path?;
    if is_outside_project_path(&path) || (head_n.is_some() && tail_n.is_some()) {
        return None;
    }
    let qp = shell_quote(&path);
    match (head_n, tail_n) {
        (Some(n), None) => Some(format!("{binary} read {qp} -m lines:1-{n}")),
        (None, Some(n)) => Some(format!("{binary} read {qp} -m lines:-{n}")),
        _ => Some(format!("{binary} read {qp}")),
    }
}

/// Returns true if the path clearly points outside the current project.
/// Paths starting with `~`, `$`, or absolute paths that don't resolve
/// within the working directory should not be intercepted.
pub(super) fn is_outside_project_path(path: &str) -> bool {
    let trimmed = path.trim();

    // Home-relative paths are always outside the project
    if trimmed.starts_with('~') {
        return true;
    }

    // Environment variable expansion — too complex, pass through
    if trimmed.starts_with('$') {
        return true;
    }

    // /proc, /sys, /dev, /tmp, /var — system paths
    if trimmed.starts_with("/proc/")
        || trimmed.starts_with("/sys/")
        || trimmed.starts_with("/dev/")
        || trimmed.starts_with("/tmp/")
        || trimmed.starts_with("/var/")
    {
        return true;
    }

    // Absolute paths: only pass through if they clearly point outside.
    // We can't know the project root here (hooks are stateless), but we can
    // detect common external patterns.
    if trimmed.starts_with('/') {
        // Home directory paths (e.g. /Users/*/Library, /home/*/.config)
        if trimmed.contains("/Library/") || trimmed.contains("/.config/") {
            return true;
        }
        // lean-ctx's own data directories
        if trimmed.contains("/.lean-ctx/") || trimmed.contains("/lean-ctx/logs/") {
            return true;
        }
    }

    false
}

pub(super) fn parse_head_tail_args<'a>(args: &[&'a str]) -> (Option<usize>, Option<&'a str>) {
    let mut n: Option<usize> = None;
    let mut path: Option<&str> = None;

    let mut i = 0;
    while i < args.len() {
        if args[i] == "-n" && i + 1 < args.len() {
            n = args[i + 1].parse().ok();
            i += 2;
        } else if let Some(num) = args[i].strip_prefix("-n") {
            n = num.parse().ok();
            i += 1;
        } else if args[i].starts_with('-') && args[i].len() > 1 {
            if let Ok(num) = args[i][1..].parse::<usize>() {
                n = Some(num);
            }
            i += 1;
        } else {
            path = Some(args[i]);
            i += 1;
        }
    }

    (n, path)
}

/// Rewrites a compound/pipeline (`a | b`, `a && b`, `a; b`, …) by wrapping the
/// WHOLE string in a single `lean-ctx -c "…"` — but only when it would pass the
/// allowlist gate. Otherwise it declines (`None`) and the command is left to the
/// agent's shell unchanged.
///
/// Why wrap-whole (not per-segment, the previous behavior): `lean-ctx -c` runs
/// the command in a profile-free POSIX shell and compresses only the FINAL
/// output, so `|`, `&&`, `||`, `;` all work natively inside it. The old
/// per-segment split left the operators in the OUTER (hooked) shell, which broke
/// two real cases (#589, idea by @getappz):
///   1. Aliased builtins (`head`, `tail`, …) resolve to an undefined `_lc`
///      helper in non-interactive git-bash → `_lc: command not found` on Windows.
///   2. The LEFT side of a pipe got compressed, so the downstream command read
///      the lean-ctx digest instead of the raw bytes it expected.
///
/// Why gate-clean only (compat-first, no new block, no bypass): wrapping subjects
/// every segment — including the pipe sink — to the allowlist. For gate-clean
/// compounds (`git log | head`, `cargo test && npm run lint`) that is exactly
/// right (compressed + fully gated). For a compound whose sink is an
/// interpreter-eval (`python3 -c …`) or a non-allowlisted tool, wrapping would
/// NEWLY block a command the agent's shell ran fine before. We decline instead
/// and leave it raw, so the user's own shell-security config keeps governing it
/// — the pre-existing behavior, with no agent-reachable raw/no-gate path opened.
pub(super) fn build_rewrite_compound(cmd: &str, binary: &str) -> Option<String> {
    let segments = compound_lexer::split_compound(cmd);
    let commands: Vec<&str> = segments
        .iter()
        .filter_map(|s| match s {
            compound_lexer::Segment::Command(c) => Some(c.trim()),
            compound_lexer::Segment::Operator(_) => None,
        })
        .collect();

    // No top-level operator → single command; the caller's wrap_single_command
    // fallback owns it.
    if segments.len() == commands.len() {
        return None;
    }

    let is_leanctx = |c: &str| c.starts_with("lean-ctx ") || c.starts_with(&format!("{binary} "));

    // A segment is already a lean-ctx call → don't nest `-c "… lean-ctx -c …"`.
    if commands.iter().any(|c| is_leanctx(c)) {
        return None;
    }

    // Nothing lean-ctx could compress/redirect → leave it to the native shell.
    if !commands.iter().any(|c| is_rewritable(c)) {
        return None;
    }

    // Wrap-whole when the compound passes the allowlist gate (compression), OR
    // when security is active and the compound violates the allowlist (#1408:
    // enforcement). The #589 "no new block" concern only applies when the user
    // has shell_security = off — in that case lean-ctx -c won't enforce anyway.
    if crate::core::shell_allowlist::passes_enforced(cmd) || needs_enforcement_wrap(cmd) {
        Some(wrap_single_command(cmd, binary))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::rewrite_candidate;

    #[test]
    fn disabled_prefix_skips_rewrite() {
        let binary = "/Users/test/.local/bin/lean-ctx";
        assert!(
            rewrite_candidate("LEAN_CTX_DISABLED=1 cargo test --lib", binary).is_none(),
            "LEAN_CTX_DISABLED prefix must skip rewrite"
        );
    }

    #[test]
    fn no_hook_prefix_skips_rewrite() {
        let binary = "/Users/test/.local/bin/lean-ctx";
        assert!(
            rewrite_candidate("LEAN_CTX_NO_HOOK=1 cargo test --lib", binary).is_none(),
            "LEAN_CTX_NO_HOOK prefix must skip rewrite"
        );
    }

    #[test]
    fn disabled_with_multiple_env_vars_skips_rewrite() {
        let binary = "/Users/test/.local/bin/lean-ctx";
        assert!(
            rewrite_candidate("FOO=bar LEAN_CTX_DISABLED=1 cargo test --lib", binary).is_none(),
            "LEAN_CTX_DISABLED anywhere in env prefix must skip rewrite"
        );
    }

    #[test]
    fn normal_env_prefix_still_rewrites() {
        let binary = "/Users/test/.local/bin/lean-ctx";
        assert!(
            rewrite_candidate("FOO=bar cargo test --lib", binary).is_some(),
            "Non-disable env prefix must still rewrite"
        );
    }

    #[test]
    fn no_prefix_still_rewrites() {
        let binary = "/Users/test/.local/bin/lean-ctx";
        assert!(
            rewrite_candidate("cargo test --lib", binary).is_some(),
            "Command without env prefix must still rewrite"
        );
    }

    #[test]
    fn lean_ctx_command_not_rewritten() {
        let binary = "/Users/test/.local/bin/lean-ctx";
        assert!(
            rewrite_candidate("lean-ctx ls src/", binary).is_none(),
            "lean-ctx commands must not be rewritten"
        );
    }

    // --- #1303: File redirect detection ---

    #[test]
    fn redirect_to_file_skips_rewrite() {
        let binary = "/Users/test/.local/bin/lean-ctx";
        assert!(
            rewrite_candidate("git show HEAD:README.md > /tmp/out.md", binary).is_none(),
            "stdout redirect to file must skip rewrite"
        );
    }

    #[test]
    fn append_redirect_skips_rewrite() {
        let binary = "/Users/test/.local/bin/lean-ctx";
        assert!(
            rewrite_candidate("echo hello >> /tmp/log.txt", binary).is_none(),
            "append redirect must skip rewrite"
        );
    }

    #[test]
    fn dev_null_redirect_still_rewrites() {
        let binary = "/Users/test/.local/bin/lean-ctx";
        assert!(
            rewrite_candidate("cargo test 2>/dev/null", binary).is_some(),
            "/dev/null redirect must still rewrite"
        );
    }

    #[test]
    fn stderr_redirect_still_rewrites() {
        let binary = "/Users/test/.local/bin/lean-ctx";
        assert!(
            rewrite_candidate("cargo test 2> /tmp/err.log", binary).is_some(),
            "stderr-only redirect must still rewrite"
        );
    }

    #[test]
    fn fd_dup_still_rewrites() {
        let binary = "/Users/test/.local/bin/lean-ctx";
        assert!(
            rewrite_candidate("cargo test 2>&1", binary).is_some(),
            "fd duplication (2>&1) must still rewrite"
        );
    }

    #[test]
    fn quoted_redirect_not_detected() {
        let binary = "/Users/test/.local/bin/lean-ctx";
        assert!(
            rewrite_candidate("echo 'output > file.txt' | grep output", binary).is_some(),
            "redirect inside quotes must not trigger skip"
        );
    }

    // --- has_stdout_file_redirect unit tests ---

    #[test]
    fn redirect_detection_basic() {
        use super::has_stdout_file_redirect;
        assert!(has_stdout_file_redirect("git status > files.txt"));
        assert!(has_stdout_file_redirect("git diff >> changes.log"));
        assert!(!has_stdout_file_redirect("git status"));
        assert!(!has_stdout_file_redirect("git status 2>/dev/null"));
        assert!(!has_stdout_file_redirect("git status > /dev/null"));
        assert!(!has_stdout_file_redirect("git status 2>&1"));
        assert!(!has_stdout_file_redirect("echo 'a > b'"));
        assert!(!has_stdout_file_redirect("echo \"a > b\""));
    }
}
