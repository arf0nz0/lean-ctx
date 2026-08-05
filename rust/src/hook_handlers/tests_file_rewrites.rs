use super::*;

#[test]
fn is_rewritable_basic() {
    assert!(is_rewritable("git status"));
    assert!(is_rewritable("cargo test --lib"));
    assert!(is_rewritable("npm run build"));
    assert!(!is_rewritable("echo hello"));
    assert!(!is_rewritable("cd src"));
    assert!(!is_rewritable("cat file.rs"));
}

#[test]
fn file_read_rewrite_cat() {
    let r = rewrite_file_read_command("cat src/main.rs", "lean-ctx");
    assert_eq!(r, Some("lean-ctx read src/main.rs".to_string()));
}

#[test]
fn rewrite_skip_reason_tracks_candidate_none_branches() {
    // Every command that `rewrite_candidate` declines must get a stable,
    // human-readable reason for the #520 debug log.
    let binary = "lean-ctx";

    let already = "lean-ctx read x";
    assert!(rewrite_candidate(already, binary).is_none());
    assert_eq!(rewrite_skip_reason(already), "already a lean-ctx command");

    let heredoc = "cat <<EOF\nhi\nEOF";
    assert!(rewrite_candidate(heredoc, binary).is_none());
    assert_eq!(
        rewrite_skip_reason(heredoc),
        "heredoc cannot be rewritten safely"
    );

    let unknown = "echo hello";
    assert!(rewrite_candidate(unknown, binary).is_none());
    assert_eq!(
        rewrite_skip_reason(unknown),
        "not a known read/search/list command"
    );

    // #1408: Compounds with non-allowlisted sinks are now wrapped for enforcement.
    // Test a heredoc compound instead — heredocs genuinely can't be wrapped.
    let heredoc_compound = "git log | cat <<EOF\nhi\nEOF";
    assert!(rewrite_candidate(heredoc_compound, binary).is_none());
    assert_eq!(
        rewrite_skip_reason(heredoc_compound),
        "heredoc cannot be rewritten safely"
    );
}

#[test]
fn file_read_rewrite_head_with_n() {
    let r = rewrite_file_read_command("head -n 20 src/main.rs", "lean-ctx");
    assert_eq!(
        r,
        Some("lean-ctx read src/main.rs -m lines:1-20".to_string())
    );
}

#[test]
fn file_read_rewrite_head_short() {
    let r = rewrite_file_read_command("head -50 src/main.rs", "lean-ctx");
    assert_eq!(
        r,
        Some("lean-ctx read src/main.rs -m lines:1-50".to_string())
    );
}

#[test]
fn file_read_rewrite_tail() {
    let r = rewrite_file_read_command("tail -n 10 src/main.rs", "lean-ctx");
    assert_eq!(
        r,
        Some("lean-ctx read src/main.rs -m lines:-10".to_string())
    );
}

#[test]
fn file_read_rewrite_not_git() {
    assert_eq!(rewrite_file_read_command("git status", "lean-ctx"), None);
}

#[test]
fn file_read_skips_home_relative_paths() {
    assert_eq!(
        rewrite_file_read_command("cat ~/Library/Logs/proxy.log", "lean-ctx"),
        None
    );
    assert_eq!(
        rewrite_file_read_command("head -20 ~/.lean-ctx/logs/proxy.stderr.log", "lean-ctx"),
        None
    );
    assert_eq!(
        rewrite_file_read_command("tail -50 ~/some/file.txt", "lean-ctx"),
        None
    );
}

#[test]
fn file_read_skips_system_paths() {
    assert_eq!(
        rewrite_file_read_command("cat /tmp/test.log", "lean-ctx"),
        None
    );
    assert_eq!(
        rewrite_file_read_command("cat /var/log/syslog", "lean-ctx"),
        None
    );
    assert_eq!(
        rewrite_file_read_command("cat /proc/cpuinfo", "lean-ctx"),
        None
    );
}

#[test]
fn file_read_skips_env_var_paths() {
    assert_eq!(
        rewrite_file_read_command("cat $HOME/.bashrc", "lean-ctx"),
        None
    );
}

#[test]
fn file_read_skips_library_and_config_paths() {
    assert_eq!(
        rewrite_file_read_command(
            "cat /Users/user/Library/LaunchAgents/com.leanctx.proxy.plist",
            "lean-ctx"
        ),
        None
    );
    assert_eq!(
        rewrite_file_read_command("cat /home/user/.config/lean-ctx/config.toml", "lean-ctx"),
        None
    );
}

#[test]
fn file_read_skips_pipes_and_redirects() {
    assert_eq!(
        rewrite_file_read_command("cat file.rs | grep fn", "lean-ctx"),
        None
    );
    assert_eq!(
        rewrite_file_read_command("cat file.rs 2>&1", "lean-ctx"),
        None
    );
    assert_eq!(
        rewrite_file_read_command("cat file.rs >> output.log", "lean-ctx"),
        None
    );
    assert_eq!(
        rewrite_file_read_command("cat a.rs && cat b.rs", "lean-ctx"),
        None
    );
    assert_eq!(
        rewrite_file_read_command("cat a.rs; echo done", "lean-ctx"),
        None
    );
}

#[test]
fn file_read_still_rewrites_project_relative_paths() {
    assert_eq!(
        rewrite_file_read_command("cat src/main.rs", "lean-ctx"),
        Some("lean-ctx read src/main.rs".to_string())
    );
    assert_eq!(
        rewrite_file_read_command("cat ./Cargo.toml", "lean-ctx"),
        Some("lean-ctx read ./Cargo.toml".to_string())
    );
    assert_eq!(
        rewrite_file_read_command("head -20 src/lib.rs", "lean-ctx"),
        Some("lean-ctx read src/lib.rs -m lines:1-20".to_string())
    );
}

// --- #561: PowerShell-native cmdlet rewrites ---

#[test]
fn ps_get_content_basic_and_alias() {
    assert_eq!(
        rewrite_file_read_command("Get-Content src/main.rs", "lean-ctx"),
        Some("lean-ctx read src/main.rs".to_string())
    );
    assert_eq!(
        rewrite_file_read_command("gc src/main.rs", "lean-ctx"),
        Some("lean-ctx read src/main.rs".to_string())
    );
    assert_eq!(
        rewrite_file_read_command("Get-Content -Path src/lib.rs", "lean-ctx"),
        Some("lean-ctx read src/lib.rs".to_string())
    );
}

#[test]
fn ps_get_content_head_and_tail() {
    // -TotalCount / -Head / -First == head; -Tail / -Last == tail. Case-insensitive.
    assert_eq!(
        rewrite_file_read_command("Get-Content -TotalCount 20 src/main.rs", "lean-ctx"),
        Some("lean-ctx read src/main.rs -m lines:1-20".to_string())
    );
    assert_eq!(
        rewrite_file_read_command("Get-Content src/main.rs -head 5", "lean-ctx"),
        Some("lean-ctx read src/main.rs -m lines:1-5".to_string())
    );
    assert_eq!(
        rewrite_file_read_command("gc -Tail 10 src/main.rs", "lean-ctx"),
        Some("lean-ctx read src/main.rs -m lines:-10".to_string())
    );
}

#[test]
fn ps_get_content_passthrough() {
    // Unknown flag, both head+tail, outside-project path, and pipelines pass through.
    assert_eq!(
        rewrite_file_read_command("Get-Content -Raw src/main.rs", "lean-ctx"),
        None
    );
    assert_eq!(
        rewrite_file_read_command("Get-Content -TotalCount 5 -Tail 5 src/main.rs", "lean-ctx"),
        None
    );
    assert_eq!(
        rewrite_file_read_command("Get-Content ~/secret.txt", "lean-ctx"),
        None
    );
    assert_eq!(
        rewrite_file_read_command("Get-Content a.txt | Select-String x", "lean-ctx"),
        None
    );
}

#[test]
fn ps_select_string_forms() {
    assert_eq!(
        rewrite_search_command("Select-String TODO src/main.rs", "lean-ctx"),
        Some("lean-ctx grep TODO src/main.rs".to_string())
    );
    assert_eq!(
        rewrite_search_command("sls TODO", "lean-ctx"),
        Some("lean-ctx grep TODO".to_string())
    );
    assert_eq!(
        rewrite_search_command("Select-String -Pattern TODO -Path src/lib.rs", "lean-ctx"),
        Some("lean-ctx grep TODO src/lib.rs".to_string())
    );
    // Unknown flag passes through.
    assert_eq!(
        rewrite_search_command("Select-String -CaseSensitive TODO", "lean-ctx"),
        None
    );
}

#[test]
fn ps_get_childitem_forms() {
    assert_eq!(
        rewrite_dir_list_command("Get-ChildItem", "lean-ctx"),
        Some("lean-ctx ls".to_string())
    );
    assert_eq!(
        rewrite_dir_list_command("gci src", "lean-ctx"),
        Some("lean-ctx ls src".to_string())
    );
    assert_eq!(
        rewrite_dir_list_command("Get-ChildItem -Path src", "lean-ctx"),
        Some("lean-ctx ls src".to_string())
    );
    // -Recurse and other flags pass through.
    assert_eq!(
        rewrite_dir_list_command("Get-ChildItem -Recurse", "lean-ctx"),
        None
    );
}

#[test]
fn ps_cmdlets_route_through_rewrite_candidate() {
    // End-to-end: the dispatcher picks the right rewrite for PowerShell cmdlets.
    assert_eq!(
        rewrite_candidate("Get-Content src/main.rs", "lean-ctx"),
        Some("lean-ctx read src/main.rs".to_string())
    );
    assert_eq!(
        rewrite_candidate("Select-String TODO src/main.rs", "lean-ctx"),
        Some("lean-ctx grep TODO src/main.rs".to_string())
    );
    assert_eq!(
        rewrite_candidate("gci src", "lean-ctx"),
        Some("lean-ctx ls src".to_string())
    );
}

#[test]
fn is_outside_project_path_tests() {
    assert!(is_outside_project_path("~/foo"));
    assert!(is_outside_project_path("~/.lean-ctx/config.toml"));
    assert!(is_outside_project_path("$HOME/.bashrc"));
    assert!(is_outside_project_path("/tmp/test"));
    assert!(is_outside_project_path("/var/log/syslog"));
    assert!(is_outside_project_path("/proc/cpuinfo"));
    assert!(is_outside_project_path("/Users/x/Library/Logs/foo.log"));
    assert!(is_outside_project_path("/home/x/.config/app/conf"));
    assert!(is_outside_project_path("/root/.lean-ctx/logs/proxy.log"));

    assert!(!is_outside_project_path("src/main.rs"));
    assert!(!is_outside_project_path("./Cargo.toml"));
    assert!(!is_outside_project_path("../sibling/file.rs"));
    assert!(!is_outside_project_path("file.txt"));
}

#[test]
fn parse_head_tail_args_basic() {
    let (n, path) = parse_head_tail_args(&["-n", "20", "file.rs"]);
    assert_eq!(n, Some(20));
    assert_eq!(path, Some("file.rs"));
}

#[test]
fn parse_head_tail_args_combined() {
    let (n, path) = parse_head_tail_args(&["-n20", "file.rs"]);
    assert_eq!(n, Some(20));
    assert_eq!(path, Some("file.rs"));
}

#[test]
fn parse_head_tail_args_short_flag() {
    let (n, path) = parse_head_tail_args(&["-50", "file.rs"]);
    assert_eq!(n, Some(50));
    assert_eq!(path, Some("file.rs"));
}

#[test]
fn should_passthrough_rules_files() {
    assert!(should_passthrough("/home/user/.cursorrules"));
    assert!(should_passthrough("/project/.cursor/rules/test.mdc"));
    assert!(should_passthrough("/home/.cursor/hooks/hooks.json"));
    assert!(should_passthrough("/project/SKILL.md"));
    assert!(should_passthrough("/project/AGENTS.md"));
    assert!(should_passthrough("/project/icon.png"));
    assert!(!should_passthrough("/project/src/main.rs"));
    assert!(!should_passthrough("/project/src/lib.ts"));
}

#[test]
fn should_passthrough_claude_auto_memory() {
    assert!(should_passthrough(
        "/home/jules/.claude/projects/-home-jules-Projects-blockposters/memory/MEMORY.md"
    ));
    assert!(should_passthrough(
        "/home/jules/.claude/projects/-home-jules-Projects-blockposters/memory/debugging.md"
    ));
    assert!(
        !should_passthrough(
            "/home/jules/.claude/projects/-home-jules-Projects-blockposters/abc.jsonl"
        ),
        "session transcripts must not passthrough as memory"
    );
}
