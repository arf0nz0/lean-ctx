/// Tokenize a shell command segment respecting single/double quotes and backslash escapes.
/// Returns tokens with outer quotes stripped, matching how the shell would parse them.
/// E.g. `git -C "Program Files" status` → `["git", "-C", "Program Files", "status"]`
pub fn shell_tokenize(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();
    let mut in_single = false;
    let mut in_double = false;
    let mut parameter_depth: u32 = 0;

    while let Some(c) = chars.next() {
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '\\' if !in_single => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            '$' if !in_single && chars.peek() == Some(&'{') => {
                parameter_depth += 1;
                current.push(c);
            }
            '}' if !in_single && parameter_depth > 0 => {
                parameter_depth -= 1;
                current.push(c);
            }
            c if c.is_whitespace() && !in_single && !in_double && parameter_depth == 0 => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(c),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Returns the byte length of the first shell token in `input`, respecting quotes
/// and `(...)` nesting. Used by `skip_env_assignments` to advance past env
/// assignments with quoted values like `FOO="bar baz"` — and, critically, past
/// assignments whose value is a command substitution like `FOO=$(cmd a b)`
/// (#855): without paren-depth tracking, whitespace *inside* the unclosed
/// `$(...)` looked like the end of the token, splitting `s=$(gh pr view …)`
/// into a bogus token `s=$(gh` plus a leftover `pr` that got misread as the
/// base command.
pub(super) fn quote_aware_token_end(input: &str) -> usize {
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut in_single = false;
    let mut in_double = false;
    let mut paren_depth: u32 = 0;
    let mut parameter_depth: u32 = 0;

    while i < len {
        let ch = bytes[i];
        match ch {
            b'\'' if !in_double => {
                in_single = !in_single;
                i += 1;
            }
            b'"' if !in_single => {
                in_double = !in_double;
                i += 1;
            }
            b'\\' if !in_single => {
                i = (i + 2).min(len);
            }
            b'(' if !in_single && !in_double => {
                paren_depth += 1;
                i += 1;
            }
            b')' if !in_single && !in_double && paren_depth > 0 => {
                paren_depth -= 1;
                i += 1;
            }
            b'$' if !in_single && !in_double && bytes.get(i + 1) == Some(&b'{') => {
                parameter_depth += 1;
                i += 1;
            }
            b'}' if !in_single && parameter_depth > 0 => {
                parameter_depth -= 1;
                i += 1;
            }
            b if b.is_ascii_whitespace()
                && !in_single
                && !in_double
                && paren_depth == 0
                && parameter_depth == 0 =>
            {
                return i;
            }
            _ => i += 1,
        }
    }
    len
}
/// Extract ALL command segments from a compound shell command.
/// Splits on: &&, ||, ;, | (pipe), and handles subshell grouping.
pub(super) fn extract_all_commands(command: &str) -> Vec<String> {
    split_on_operators(command)
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Split command string on shell operators: ;, &&, ||, |
/// Respects single/double quotes, parentheses nesting, and backslash escapes
/// outside single quotes (GL #1160): `rg split\.label\|quantityLabel` is ONE
/// command — the escaped pipe is regex data, not an operator. The old scanner
/// split there and blocked the pattern fragment as an unknown command; same
/// for `find … -exec rm {} \;`.
pub(super) fn split_on_operators(command: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut start = 0;
    let bytes = command.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut paren_depth: u32 = 0;
    // #939: brace groups (`{ cmd; }`) need the same operator-shielding as
    // `( cmd )` subshells — otherwise a `}` that closes a `{` opened on an
    // earlier physical line (e.g. after heredoc-body stripping collapses the
    // body between them) is misread as its own bare command segment.
    let mut brace_depth: u32 = 0;

    while i < len {
        let ch = bytes[i];

        if in_single_quote {
            if ch == b'\'' {
                in_single_quote = false;
            }
            i += 1;
            continue;
        }

        if in_double_quote {
            match ch {
                // \" stays inside the string; \\ consumes both so `"x\\"` closes.
                b'\\' => i = (i + 2).min(len),
                b'"' => {
                    in_double_quote = false;
                    i += 1;
                }
                _ => i += 1,
            }
            continue;
        }

        match ch {
            b'\\' => {
                // Escaped char is data (bash semantics outside quotes) — never
                // an operator or quote opener.
                i = (i + 2).min(len);
            }
            b'\'' => {
                in_single_quote = true;
                i += 1;
            }
            b'"' => {
                in_double_quote = true;
                i += 1;
            }
            b'(' => {
                paren_depth += 1;
                i += 1;
            }
            b')' => {
                paren_depth = paren_depth.saturating_sub(1);
                i += 1;
            }
            b'{' => {
                brace_depth += 1;
                i += 1;
            }
            b'}' => {
                brace_depth = brace_depth.saturating_sub(1);
                i += 1;
            }
            b'\n' | b'\r' | b';' if paren_depth == 0 && brace_depth == 0 => {
                segments.push(&command[start..i]);
                i += 1;
                start = i;
            }
            b'&' if paren_depth == 0 && brace_depth == 0 => {
                if i + 1 < len && bytes[i + 1] == b'&' {
                    // &&
                    segments.push(&command[start..i]);
                    i += 2;
                    start = i;
                } else if (i > 0 && bytes[i - 1] == b'>') || (i + 1 < len && bytes[i + 1] == b'>') {
                    // Redirect operator, NOT a separator: `2>&1`, `1>&2`, `>&file` (prev is '>')
                    // or `&>file`, `&>>file` (next is '>'). The '&' belongs to the current
                    // command — splitting here would mistake the fd/target (e.g. `1`) for a
                    // standalone command and falsely block it (#334).
                    i += 1;
                } else {
                    // single & (background operator) — still a command separator
                    segments.push(&command[start..i]);
                    i += 1;
                    start = i;
                }
            }
            b'|' if paren_depth == 0 && brace_depth == 0 => {
                if i + 1 < len && bytes[i + 1] == b'|' {
                    // ||
                    segments.push(&command[start..i]);
                    i += 2;
                    start = i;
                } else if i > 0 && bytes[i - 1] == b'>' {
                    // `>|` (noclobber redirect), NOT a pipe: the '|' belongs to
                    // the redirect operator and the following token is a file
                    // path, not a command. Splitting here treated the target
                    // (e.g. `out` in `date >| out`) as a command and falsely
                    // blocked it against the allowlist (#387).
                    i += 1;
                } else {
                    // pipe
                    segments.push(&command[start..i]);
                    i += 1;
                    start = i;
                }
            }
            _ => {
                i += 1;
            }
        }
    }

    if start < len {
        segments.push(&command[start..]);
    }

    segments
}

/// Extract the base command name from a single segment (no operators).
pub(super) fn extract_base_from_segment(segment: &str) -> String {
    let trimmed = segment.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let cmd_part = skip_env_assignments(trimmed);
    if cmd_part.is_empty() {
        return String::new();
    }

    let tokens = shell_tokenize(cmd_part);
    // #939: a leading `{` brace-group token (e.g. from
    // `agent_wrapper::rebuild`'s `{ <real command>\n} && pwd ...` wrapping)
    // is not itself a command — skip it so the base extracted is the real
    // command inside the group, not the brace.
    let mut token_iter = tokens.iter();
    let first_token = match token_iter.next().map(String::as_str) {
        Some("{") => token_iter.next().map_or("", String::as_str),
        other => other.unwrap_or(""),
    };

    first_token
        .rsplit('/')
        .next()
        .unwrap_or(first_token)
        .to_string()
}

/// Shell builtins that legitimately export or mutate environment variables.
/// A segment beginning with one of these (`export PATH=…`, `readonly FOO=bar`)
/// is not a bare inline `PATH=… cmd` hijack — skip the builtin and any
/// following `VAR=value` tokens so export-only segments contribute no leaf
/// command and `export PATH=… ; python3 …` resolves to `python3`.
const ENV_SETTING_BUILTINS: &[&str] =
    &["export", "unset", "readonly", "local", "declare", "typeset"];

/// Skip leading KEY=VALUE environment variable assignments.
/// Uses quote-aware scanning so `FOO="bar baz" git status` correctly
/// skips the entire `FOO="bar baz"` token.
pub(super) fn skip_env_assignments(segment: &str) -> &str {
    let mut rest = segment;
    loop {
        let rest_trimmed = rest.trim_start();
        if rest_trimmed.is_empty() {
            return rest_trimmed;
        }
        let end = quote_aware_token_end(rest_trimmed);
        if end == 0 {
            return rest_trimmed;
        }
        let raw_token = &rest_trimmed[..end];
        let unquoted: String = raw_token
            .chars()
            .filter(|c| *c != '"' && *c != '\'')
            .collect();
        let base_token = unquoted.rsplit('/').next().unwrap_or(unquoted.as_str());
        if ENV_SETTING_BUILTINS.contains(&base_token) {
            rest = &rest_trimmed[end..];
            continue;
        }
        if unquoted.contains('=')
            && !unquoted.starts_with('-')
            && !unquoted.starts_with('/')
            && !unquoted.starts_with('.')
        {
            rest = &rest_trimmed[end..];
        } else {
            return rest_trimmed;
        }
    }
}
/// Public accessor for extracting all command segments.
pub fn extract_all_commands_pub(command: &str) -> Vec<String> {
    extract_all_commands(command)
}
// Legacy compat: single-segment extraction (used by other callers)
pub fn extract_base_command(command: &str) -> String {
    let first_seg = split_on_operators(command)
        .into_iter()
        .next()
        .unwrap_or(command);
    extract_base_from_segment(first_seg)
}
