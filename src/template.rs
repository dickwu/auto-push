use crate::sanitize;
use anyhow::{Result, anyhow};
use regex::Regex;
use std::collections::HashMap;

/// Cap on interpolated value size, applied before escaping so truncation can
/// never split an escape sequence.
///
/// Sized for Linux: `execve` caps a single argument at `MAX_ARG_STRLEN`
/// (128 KiB), the whole `sh -c <script>` script is one argument, and
/// single-quote escaping can quadruple a run of `'`. Keeping raw values under
/// ~24k chars keeps the escaped command well under that limit (4×24k = 96 KiB)
/// while still covering realistic commit messages and the default diff cap.
const MAX_INTERPOLATED_CHARS: usize = 24_000;

/// POSIX shell quoting context at a given position in a command template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuoteContext {
    Unquoted,
    Single,
    Double,
    /// Inside a `#` comment (until the next newline). Content is discarded by
    /// the shell, so an interpolated value here only needs its newlines
    /// neutralised so it can't end the comment and expose live code.
    Comment,
}

/// Best-effort POSIX shell lexer state carried across literal template
/// segments. Tracks the quote/comment context plus whether the next character
/// begins a new word (needed to recognise a `#` comment, which only starts at
/// a word boundary). It does not model nested contexts inside `$(...)` or
/// backticks — straightforward one-liner templates (everything auto-push
/// generates) are tracked exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ShellState {
    ctx: QuoteContext,
    /// In unquoted context, true when the previous character was a blank or
    /// token delimiter (or start of input) — i.e. a `#` here opens a comment.
    at_word_start: bool,
}

impl ShellState {
    fn start() -> Self {
        Self {
            ctx: QuoteContext::Unquoted,
            at_word_start: true,
        }
    }
}

/// Advance the shell lexer state across a literal template segment.
fn advance_shell_state(segment: &str, mut st: ShellState) -> ShellState {
    let bytes = segment.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match st.ctx {
            QuoteContext::Unquoted => match bytes[i] {
                b'#' if st.at_word_start => st.ctx = QuoteContext::Comment,
                b'\'' => {
                    st.ctx = QuoteContext::Single;
                    st.at_word_start = false;
                }
                b'"' => {
                    st.ctx = QuoteContext::Double;
                    st.at_word_start = false;
                }
                b'\\' => {
                    i += 1;
                    st.at_word_start = false;
                }
                // Blanks and the unquoted token delimiters start a new word.
                b' ' | b'\t' | b'\n' | b';' | b'&' | b'|' | b'(' | b')' | b'<' | b'>' => {
                    st.at_word_start = true;
                }
                _ => st.at_word_start = false,
            },
            QuoteContext::Single => {
                if bytes[i] == b'\'' {
                    st.ctx = QuoteContext::Unquoted;
                    st.at_word_start = false;
                }
            }
            QuoteContext::Double => match bytes[i] {
                b'"' => {
                    st.ctx = QuoteContext::Unquoted;
                    st.at_word_start = false;
                }
                b'\\' => i += 1,
                _ => {}
            },
            QuoteContext::Comment => {
                if bytes[i] == b'\n' {
                    st.ctx = QuoteContext::Unquoted;
                    st.at_word_start = true;
                }
            }
        }
        i += 1;
    }
    st
}

/// Normalize a value before shell interpolation: strip ANSI sequences and
/// control chars (except newline/tab), trim, and truncate.
fn normalize_value(raw: &str) -> String {
    let cleaned = sanitize::clean_text(raw);
    let trimmed = cleaned.trim();
    if trimmed.chars().count() > MAX_INTERPOLATED_CHARS {
        let truncated: String = trimmed.chars().take(MAX_INTERPOLATED_CHARS).collect();
        format!("{truncated}...(truncated)")
    } else {
        trimmed.to_string()
    }
}

/// True if the value can be substituted bare in unquoted context: one word,
/// no metacharacters, glob chars, or whitespace. A leading `-` is rejected so
/// a value can never be reinterpreted as a command-line option.
fn is_shell_safe_bare(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && value.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '_' | '-' | '.' | ',' | ':' | '/' | '@' | '%' | '+')
        })
}

/// Escape `value` so the shell reads it back verbatim in the given context.
///
/// - Single-quoted context: close the quote around embedded `'` (`'\''`);
///   everything else, including newlines, is literal inside single quotes.
/// - Double-quoted context: backslash-escape the four characters the shell
///   interprets there (`\`, `"`, `$`, backtick). `!` is left alone: history
///   expansion is off in non-interactive `sh -c`.
/// - Unquoted context: substitute bare only when the value is a single safe
///   word; otherwise wrap it in single quotes (adjacent quoted text
///   concatenates, so this stays correct mid-word).
fn escape_for_context(value: &str, ctx: QuoteContext) -> String {
    match ctx {
        QuoteContext::Single => value.replace('\'', "'\\''"),
        QuoteContext::Double => {
            let mut out = String::with_capacity(value.len());
            for ch in value.chars() {
                if matches!(ch, '\\' | '"' | '$' | '`') {
                    out.push('\\');
                }
                out.push(ch);
            }
            out
        }
        QuoteContext::Unquoted => {
            if is_shell_safe_bare(value) {
                value.to_string()
            } else {
                format!("'{}'", value.replace('\'', "'\\''"))
            }
        }
        // Comment content is discarded; only a newline could end the comment
        // early and expose the rest as live code, so collapse newlines.
        QuoteContext::Comment => value.replace('\n', " "),
    }
}

/// Render a template string for use in shell commands.
/// Values are escaped for the quote context they land in (single-quoted,
/// double-quoted, or bare), so quotes, `$`, backticks, semicolons, and
/// newlines in a value can neither break the command nor inject one.
/// Unresolved `{{ var }}` patterns are left as-is.
pub fn render_shell(template: &str, vars: &HashMap<String, String>) -> String {
    let spans = scan_template_expressions(template);
    if spans.is_empty() {
        return template.to_string();
    }
    let mut result = String::with_capacity(template.len());
    let mut last = 0;
    let mut state = ShellState::start();
    for (start, end, expr) in spans {
        let literal = &template[last..start];
        state = advance_shell_state(literal, state);
        result.push_str(literal);
        match resolve_expression(expr, vars) {
            Ok(val) => {
                // A correctly escaped value never changes the quote/comment
                // context, so `state` stays valid; it only ends a word.
                result.push_str(&escape_for_context(&normalize_value(&val), state.ctx));
                state.at_word_start = false;
            }
            Err(_) => {
                let raw_span = &template[start..end];
                state = advance_shell_state(raw_span, state);
                result.push_str(raw_span);
            }
        }
        last = end;
    }
    result.push_str(&template[last..]);
    result
}

/// Render a template string for use as process arguments.
/// No shell escaping is needed since values are passed directly via
/// `Command::new().args()`, not through a shell, but ANSI sequences and stray
/// control characters are still stripped so tool output doesn't leak into
/// argv (e.g. a colored AI response used as a commit message).
/// Unresolved `{{ var }}` patterns are left as-is.
pub fn render_raw(template: &str, vars: &HashMap<String, String>) -> String {
    let spans = scan_template_expressions(template);
    if spans.is_empty() {
        return template.to_string();
    }
    let mut result = String::with_capacity(template.len());
    let mut last = 0;
    for (start, end, expr) in spans {
        result.push_str(&template[last..start]);
        match resolve_expression(expr, vars) {
            Ok(val) => result.push_str(sanitize::clean_text(&val).trim()),
            Err(_) => result.push_str(&template[start..end]),
        }
        last = end;
    }
    result.push_str(&template[last..]);
    result
}

/// Apply a regex to `text`.  Returns:
/// - the first capture group if present,
/// - or the full match if no capture groups,
/// - or an empty string if no match.
pub fn extract_regex(text: &str, pattern: &str) -> String {
    let re = match Regex::new(pattern) {
        Ok(r) => r,
        Err(_) => return String::new(),
    };

    if let Some(caps) = re.captures(text) {
        if caps.len() > 1 {
            caps.get(1)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default()
        } else {
            caps.get(0)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default()
        }
    } else {
        String::new()
    }
}

/// Parse "var_name:/pattern/" into (var_name, pattern).
fn parse_regex_expr(expr: &str) -> Option<(&str, &str)> {
    let idx = expr.find(":/")?;
    let var_name = &expr[..idx];
    let rest = &expr[idx + 2..];
    let pattern = rest.strip_suffix('/')?;
    Some((var_name.trim(), pattern))
}

/// Parse "var_name.field.0.nested" into (var_name, path_segments).
fn parse_dot_path(expr: &str) -> Option<(&str, Vec<&str>)> {
    let dot_idx = expr.find('.')?;
    let var_name = &expr[..dot_idx];
    let path_str = &expr[dot_idx + 1..];
    let segments: Vec<&str> = path_str.split('.').collect();
    if segments.is_empty() {
        return None;
    }
    Some((var_name.trim(), segments))
}

/// Navigate a serde_json::Value by dot-path segments.
fn resolve_json_path(value: &serde_json::Value, segments: &[&str]) -> Result<String> {
    let mut current = value;
    for segment in segments {
        if *segment == "length" {
            if let Some(arr) = current.as_array() {
                return Ok(arr.len().to_string());
            }
            return Err(anyhow!("'length' used on non-array value"));
        }
        if let Ok(idx) = segment.parse::<usize>() {
            current = current
                .get(idx)
                .ok_or_else(|| anyhow!("array index {idx} out of bounds"))?;
        } else {
            current = current
                .get(*segment)
                .ok_or_else(|| anyhow!("field '{}' not found", segment))?;
        }
    }
    match current {
        serde_json::Value::String(s) => Ok(s.clone()),
        serde_json::Value::Null => Ok("null".to_string()),
        other => Ok(other.to_string()),
    }
}

/// Resolve a template expression to its string value.
///
/// Supports three forms (tried in order):
/// 1. Regex extraction: `"ver:/v(\d+)/"` -> applies regex to `ver` value
/// 2. Exact key match: `"name"` or `"command_output.prev"` -> direct lookup in vars
/// 3. JSON dot-path: `"data.status"` -> parses `data` as JSON, navigates to `.status`
pub fn resolve_expression(expr: &str, vars: &HashMap<String, String>) -> Result<String> {
    if let Some((var_name, pattern)) = parse_regex_expr(expr) {
        let raw = vars
            .get(var_name)
            .ok_or_else(|| anyhow!("unknown variable: '{var_name}'"))?;
        return Ok(extract_regex(raw, pattern));
    }
    // Try exact key match first (handles keys with dots like "command_output.prev")
    if let Some(val) = vars.get(expr) {
        return Ok(val.clone());
    }
    if let Some((var_name, segments)) = parse_dot_path(expr) {
        let raw = vars
            .get(var_name)
            .ok_or_else(|| anyhow!("unknown variable: '{var_name}'"))?;
        let json: serde_json::Value = serde_json::from_str(raw)
            .map_err(|_| anyhow!("variable '{var_name}' is not valid JSON for dot-path access"))?;
        return resolve_json_path(&json, &segments);
    }
    Err(anyhow!("unknown variable: '{expr}'"))
}

/// Scan a template string for {{ expression }} spans.
/// Handles :/regex/ bodies that may contain } characters.
/// Returns Vec of (start_byte, end_byte, trimmed_expression).
pub fn scan_template_expressions(input: &str) -> Vec<(usize, usize, &str)> {
    let mut results = Vec::new();
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i + 1 < len {
        if bytes[i] == b'{' && bytes[i + 1] == b'{' {
            let start = i;
            i += 2;
            while i < len && bytes[i] == b' ' {
                i += 1;
            }
            let expr_start = i;
            let mut in_regex = false;

            while i + 1 < len {
                if !in_regex && bytes[i] == b':' && i + 1 < len && bytes[i + 1] == b'/' {
                    in_regex = true;
                    i += 2;
                    continue;
                }
                if in_regex && bytes[i] == b'/' {
                    let escaped = {
                        let mut count = 0usize;
                        let mut j = i;
                        while j > 0 && bytes[j - 1] == b'\\' {
                            count += 1;
                            j -= 1;
                        }
                        count % 2 == 1
                    };
                    if !escaped {
                        in_regex = false;
                        i += 1;
                        continue;
                    }
                }
                if !in_regex && bytes[i] == b'}' && bytes[i + 1] == b'}' {
                    let expr = input[expr_start..i].trim();
                    if !expr.is_empty() {
                        results.push((start, i + 2, expr));
                    }
                    i += 2;
                    break;
                }
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn test_render_raw_basic() {
        let v = vars(&[("name", "hello"), ("value", "world")]);
        assert_eq!(render_raw("{{ name }} {{ value }}", &v), "hello world");
    }

    #[test]
    fn test_render_raw_no_escaping() {
        let v = vars(&[("prompt", "What's the $PATH?")]);
        assert_eq!(render_raw("{{ prompt }}", &v), "What's the $PATH?");
    }

    #[test]
    fn test_render_shell_escapes() {
        // Unquoted context: unsafe values get wrapped in single quotes with
        // embedded quotes closed around ('\'').
        let v = vars(&[("val", "it's a $test")]);
        let result = render_shell("echo {{ val }}", &v);
        assert_eq!(result, "echo 'it'\\''s a $test'");
    }

    #[test]
    fn test_render_shell_single_quoted_context() {
        let v = vars(&[("msg", "don't")]);
        let result = render_shell("git commit -m '{{ msg }}'", &v);
        assert_eq!(result, "git commit -m 'don'\\''t'");
    }

    #[test]
    fn test_render_shell_double_quoted_context() {
        let v = vars(&[("msg", "a \"b\" $c `d` \\e")]);
        let result = render_shell("git commit -m \"{{ msg }}\"", &v);
        assert_eq!(result, "git commit -m \"a \\\"b\\\" \\$c \\`d\\` \\\\e\"");
    }

    #[test]
    fn test_render_shell_bare_safe_value_not_quoted() {
        let v = vars(&[("branch", "feature/foo-1.2")]);
        assert_eq!(
            render_shell("git push origin {{ branch }}", &v),
            "git push origin feature/foo-1.2"
        );
    }

    #[test]
    fn test_render_shell_strips_ansi_and_controls() {
        let v = vars(&[("msg", "\u{1b}[32mfix: colored\u{1b}[0m\u{0}")]);
        let result = render_shell("git commit -m '{{ msg }}'", &v);
        assert_eq!(result, "git commit -m 'fix: colored'");
    }

    #[test]
    fn test_advance_shell_state_tracking() {
        use super::QuoteContext::*;
        let s = ShellState::start();
        let ctx = |seg| advance_shell_state(seg, s).ctx;
        assert_eq!(ctx("echo "), Unquoted);
        assert_eq!(ctx("echo '"), Single);
        assert_eq!(ctx("echo 'a' "), Unquoted);
        assert_eq!(ctx("echo \""), Double);
        assert_eq!(ctx("echo \"a\\\""), Double);
        assert_eq!(ctx("echo \\'"), Unquoted);
        assert_eq!(ctx("a'b\"c"), Single);
        assert_eq!(ctx("'\"'"), Unquoted);
        // `#` opens a comment only at a word boundary.
        assert_eq!(ctx("true # "), Comment);
        assert_eq!(ctx("# "), Comment);
        assert_eq!(ctx("echo foo#bar"), Unquoted);
        assert_eq!(ctx("echo 'a'#b"), Unquoted);
        // A newline ends the comment.
        assert_eq!(ctx("true # x\nfoo"), Unquoted);
    }

    #[test]
    fn test_render_raw_unresolved_left_asis() {
        let v = vars(&[("known", "yes")]);
        assert_eq!(
            render_raw("{{ known }} {{ unknown }}", &v),
            "yes {{ unknown }}"
        );
    }

    #[test]
    fn test_render_shell_unresolved_left_asis() {
        let v = vars(&[]);
        assert_eq!(render_shell("{{ missing }}", &v), "{{ missing }}");
    }

    #[test]
    fn test_normalize_value_truncates_before_escaping() {
        // Truncation happens before escaping, so a wall of quotes can never
        // be cut mid-escape and unbalance the command. Assert on the rendered
        // string directly rather than round-tripping a maximal value, which
        // would push a huge single argument past the shell's per-arg limit.
        let long = "'".repeat(MAX_INTERPOLATED_CHARS + 100);
        let normalized = normalize_value(&long);
        assert!(normalized.ends_with("...(truncated)"));
        assert_eq!(
            normalized.chars().count(),
            MAX_INTERPOLATED_CHARS + "...(truncated)".len()
        );

        // Each kept `'` becomes `'\''`; the command stays balanced — opening
        // template quote + escaped quotes, ending in the marker then the
        // template's closing quote.
        let v = vars(&[("msg", long.as_str())]);
        let rendered = render_shell("printf %s '{{ msg }}'", &v);
        assert!(rendered.starts_with("printf %s ''\\''"));
        assert!(rendered.ends_with("...(truncated)'"));
    }

    #[test]
    fn test_extract_regex_capture_group() {
        assert_eq!(extract_regex("v1.2.3", r"v(\d+\.\d+\.\d+)"), "1.2.3");
    }

    #[test]
    fn test_extract_regex_no_groups() {
        assert_eq!(extract_regex("hello world", r"\w+"), "hello");
    }

    #[test]
    fn test_extract_regex_no_match() {
        assert_eq!(extract_regex("hello", r"\d+"), "");
    }

    #[test]
    fn test_extract_regex_invalid_pattern() {
        assert_eq!(extract_regex("hello", r"[invalid"), "");
    }

    #[test]
    fn test_render_raw_trims_values() {
        let v = vars(&[("name", "  spaced  ")]);
        assert_eq!(render_raw("{{ name }}", &v), "spaced");
    }

    #[test]
    fn test_render_raw_whitespace_in_braces() {
        let v = vars(&[("x", "val")]);
        assert_eq!(render_raw("{{  x  }}", &v), "val");
    }

    #[test]
    fn test_scan_simple_var() {
        let spans = scan_template_expressions("hello {{ name }} world");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].2, "name");
    }

    #[test]
    fn test_scan_regex_with_brace() {
        let spans = scan_template_expressions("{{ val:/\\d{7}/ }}");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].2, "val:/\\d{7}/");
    }

    #[test]
    fn test_scan_dot_path() {
        let spans = scan_template_expressions("{{ plan.0.message }}");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].2, "plan.0.message");
    }

    #[test]
    fn test_scan_multiple() {
        let spans = scan_template_expressions("{{ a }} and {{ b.x }}");
        assert_eq!(spans.len(), 2);
    }

    #[test]
    fn test_scan_no_expressions() {
        let spans = scan_template_expressions("no templates here");
        assert_eq!(spans.len(), 0);
    }

    #[test]
    fn test_scan_unclosed_left_asis() {
        let spans = scan_template_expressions("{{ unclosed");
        assert_eq!(spans.len(), 0);
    }

    #[test]
    fn test_scan_regex_double_backslash_before_slash() {
        // \\/ means literal backslash then closing slash — regex body should close
        let spans = scan_template_expressions("{{ val:/foo\\\\/ }}");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].2, "val:/foo\\\\/");
    }

    #[test]
    fn test_resolve_simple_var() {
        let v = vars(&[("name", "hello")]);
        assert_eq!(resolve_expression("name", &v).unwrap(), "hello");
    }

    #[test]
    fn test_resolve_unknown_var_errors() {
        let v = vars(&[]);
        assert!(resolve_expression("missing", &v).is_err());
    }

    #[test]
    fn test_resolve_dot_path_object() {
        let v = vars(&[("data", r#"{"status":"ok","count":3}"#)]);
        assert_eq!(resolve_expression("data.status", &v).unwrap(), "ok");
        assert_eq!(resolve_expression("data.count", &v).unwrap(), "3");
    }

    #[test]
    fn test_resolve_dot_path_array() {
        let v = vars(&[("items", r#"[{"name":"a"},{"name":"b"}]"#)]);
        assert_eq!(resolve_expression("items.0.name", &v).unwrap(), "a");
        assert_eq!(resolve_expression("items.1.name", &v).unwrap(), "b");
    }

    #[test]
    fn test_resolve_dot_path_length() {
        let v = vars(&[("arr", r#"[1,2,3]"#)]);
        assert_eq!(resolve_expression("arr.length", &v).unwrap(), "3");
    }

    #[test]
    fn test_resolve_dot_path_not_json_errors() {
        let v = vars(&[("plain", "just text")]);
        assert!(resolve_expression("plain.field", &v).is_err());
    }

    #[test]
    fn test_resolve_regex_capture_group() {
        let v = vars(&[("ver", "release v1.2.3 deployed")]);
        assert_eq!(
            resolve_expression("ver:/v(\\d+\\.\\d+\\.\\d+)/", &v).unwrap(),
            "1.2.3"
        );
    }

    #[test]
    fn test_resolve_regex_no_match_empty() {
        let v = vars(&[("text", "no numbers here")]);
        assert_eq!(resolve_expression("text:/\\d+/", &v).unwrap(), "");
    }

    // -----------------------------------------------------------------------
    // Shell roundtrip tests: render a template with a hostile value, execute
    // it through a real `sh -c`, and assert the value survives byte-for-byte.
    // These pin the safety contract of render_shell for every quote context
    // that appears in generated pipelines (single, double, unquoted).
    // -----------------------------------------------------------------------

    /// Run `sh -c <rendered>` and return stdout. Panics if sh reports a
    /// syntax error or non-zero exit — a broken quoting scheme fails here.
    fn sh_roundtrip(rendered: &str) -> String {
        let out = std::process::Command::new("sh")
            .args(["-c", rendered])
            .output()
            .expect("failed to spawn sh");
        assert!(
            out.status.success(),
            "sh failed for command {rendered:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    /// Hostile values every quote context must survive unchanged.
    fn hostile_values() -> Vec<&'static str> {
        vec![
            "fix: don't panic",
            "it's a \"mixed\" quote",
            "cost is $100 and `date`",
            "a $(reboot) substitution",
            "semi; colons && ampersands || pipes | here",
            "redirect < in > out",
            "bang! star* quest? brack[et]",
            "back\\slash and tab\there",
            "'; echo INJECTED; '",
            "\"; echo INJECTED; \"",
            "`echo INJECTED`",
            "unicode: héllo wörld 提交信息 🚀",
            "-starts-with-dash",
            "trailing backslash\\",
            "#hash and %percent +plus",
        ]
    }

    #[test]
    fn test_shell_roundtrip_single_quoted_context() {
        // Default auto-init commit step: git commit -m '{{ commit_message }}'
        for msg in hostile_values() {
            let v = vars(&[("msg", msg)]);
            let rendered = render_shell("printf %s '{{ msg }}'", &v);
            assert_eq!(sh_roundtrip(&rendered), msg, "value corrupted: {msg:?}");
        }
    }

    #[test]
    fn test_shell_roundtrip_double_quoted_context() {
        // Smart-init commit step: git commit -m "{{ commit_message }}"
        for msg in hostile_values() {
            let v = vars(&[("msg", msg)]);
            let rendered = render_shell("printf %s \"{{ msg }}\"", &v);
            assert_eq!(sh_roundtrip(&rendered), msg, "value corrupted: {msg:?}");
        }
    }

    #[test]
    fn test_shell_roundtrip_unquoted_context() {
        // Bare interpolation must yield exactly one word with the value intact.
        for msg in hostile_values() {
            let v = vars(&[("msg", msg)]);
            let rendered = render_shell("printf %s {{ msg }}", &v);
            assert_eq!(sh_roundtrip(&rendered), msg, "value corrupted: {msg:?}");
        }
    }

    #[test]
    fn test_shell_roundtrip_multiline_preserved() {
        let msg = "feat: add feature\n\n- bullet one\n- bullet 'two'\n- costs $5";
        for template in [
            "printf %s '{{ msg }}'",
            "printf %s \"{{ msg }}\"",
            "printf %s {{ msg }}",
        ] {
            let v = vars(&[("msg", msg)]);
            let rendered = render_shell(template, &v);
            assert_eq!(
                sh_roundtrip(&rendered),
                msg,
                "newlines corrupted via {template:?}"
            );
        }
    }

    #[test]
    fn test_shell_roundtrip_injection_never_executes() {
        // If quoting is broken the injected command runs and the marker file
        // appears; the printf output also diverges from the literal value.
        let dir = std::env::temp_dir().join(format!("ap-inject-{}", std::process::id()));
        let marker = dir.join("pwned");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let payload = format!("'; touch {}; '", marker.display());
        let v = vars(&[("msg", payload.as_str())]);
        let rendered = render_shell("printf %s '{{ msg }}'", &v);
        let echoed = sh_roundtrip(&rendered);

        assert!(!marker.exists(), "injection executed: {rendered}");
        assert_eq!(echoed, payload);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_shell_roundtrip_value_after_closed_quote() {
        // Context tracker must notice the template's own quotes closing.
        let v = vars(&[("a", "it's one"), ("b", "two $x")]);
        let rendered = render_shell("printf '%s|%s' '{{ a }}' \"{{ b }}\"", &v);
        assert_eq!(sh_roundtrip(&rendered), "it's one|two $x");
    }

    #[test]
    fn test_render_shell_empty_value_unquoted_stays_one_arg() {
        // An empty value in unquoted context must not vanish into zero args.
        let v = vars(&[("msg", "")]);
        let rendered = render_shell("printf 'x%sy' {{ msg }}", &v);
        assert_eq!(sh_roundtrip(&rendered), "xy");
    }

    #[test]
    fn test_shell_roundtrip_comment_context_no_injection() {
        // A value interpolated after an unquoted `#` lands in a shell comment.
        // A multi-line value must not end the comment and run injected code.
        let dir = std::env::temp_dir().join(format!("ap-comment-{}", std::process::id()));
        let marker = dir.join("pwned");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let payload = format!("subject line\ntouch {}\ntrailer", marker.display());
        let v = vars(&[("msg", payload.as_str())]);
        let rendered = render_shell("true # {{ msg }}", &v);
        // `true` plus a one-line comment: succeeds, produces nothing, and the
        // newline-borne `touch` never executes.
        assert_eq!(sh_roundtrip(&rendered), "");
        assert!(!marker.exists(), "comment injection executed: {rendered}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_render_shell_comment_context_collapses_newlines() {
        let v = vars(&[("msg", "one\ntwo\nthree")]);
        let rendered = render_shell("true # {{ msg }}", &v);
        assert_eq!(rendered, "true # one two three");
    }
}
