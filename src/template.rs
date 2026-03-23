use regex::Regex;
use std::collections::HashMap;
use std::sync::OnceLock;

fn template_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\{\{\s*([^}]+?)\s*\}\}").expect("static regex"))
}

/// Sanitize a value before interpolating it into a shell command template.
/// Trims whitespace, escapes shell metacharacters, normalises newlines,
/// and truncates to 4096 chars.
pub fn sanitize_shell_value(raw: &str) -> String {
    let trimmed = raw.trim();
    let no_cr = trimmed.replace("\r\n", "\n").replace('\r', "");

    let shell_chars = [
        '\'', '"', '`', '$', '!', '(', ')', '|', '&', ';', '<', '>', '\\',
    ];
    let mut escaped = String::with_capacity(no_cr.len());
    for ch in no_cr.chars() {
        if shell_chars.contains(&ch) {
            escaped.push('\\');
        }
        escaped.push(ch);
    }

    let normalised = escaped.replace('\n', "\\n");

    if normalised.chars().count() > 4096 {
        let truncated: String = normalised.chars().take(4096).collect();
        format!("{truncated}...(truncated)")
    } else {
        normalised
    }
}

/// Render a template string for use in shell commands.
/// Values are shell-escaped to prevent injection.
/// Unresolved `{{ var }}` patterns are left as-is.
pub fn render_shell(template: &str, vars: &HashMap<String, String>) -> String {
    let re = template_regex();
    re.replace_all(template, |caps: &regex::Captures| {
        let key = caps[1].trim();
        vars.get(key)
            .map(|v| sanitize_shell_value(v))
            .unwrap_or_else(|| caps[0].to_string())
    })
    .into_owned()
}

/// Render a template string for use as process arguments.
/// Values are substituted raw (no shell escaping) since they will be
/// passed directly via `Command::new().args()`, not through a shell.
/// Unresolved `{{ var }}` patterns are left as-is.
pub fn render_raw(template: &str, vars: &HashMap<String, String>) -> String {
    let re = template_regex();
    re.replace_all(template, |caps: &regex::Captures| {
        let key = caps[1].trim();
        vars.get(key)
            .map(|v| v.trim().to_string())
            .unwrap_or_else(|| caps[0].to_string())
    })
    .into_owned()
}

#[allow(dead_code)]
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

#[allow(dead_code)]
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
                    let escaped = i > 0 && bytes[i - 1] == b'\\';
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
        let v = vars(&[("val", "it's a $test")]);
        let result = render_shell("echo {{ val }}", &v);
        assert!(result.contains("\\'"));
        assert!(result.contains("\\$"));
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
    fn test_sanitize_shell_value_truncates() {
        let long = "x".repeat(5000);
        let result = sanitize_shell_value(&long);
        assert!(result.ends_with("...(truncated)"));
        assert!(result.chars().count() < 5000);
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
}
