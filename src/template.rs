use anyhow::{Result, anyhow};
use regex::Regex;
use std::collections::HashMap;

/// Sanitize a value before interpolating it into a shell command template.
/// Trims whitespace, escapes shell metacharacters, normalises newlines,
/// and truncates to 200_000 chars (large enough for most diffs).
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

    if normalised.chars().count() > 200_000 {
        let truncated: String = normalised.chars().take(200_000).collect();
        format!("{truncated}...(truncated)")
    } else {
        normalised
    }
}

/// Render a template string for use in shell commands.
/// Values are shell-escaped to prevent injection.
/// Unresolved `{{ var }}` patterns are left as-is.
pub fn render_shell(template: &str, vars: &HashMap<String, String>) -> String {
    let spans = scan_template_expressions(template);
    if spans.is_empty() {
        return template.to_string();
    }
    let mut result = String::with_capacity(template.len());
    let mut last = 0;
    for (start, end, expr) in spans {
        result.push_str(&template[last..start]);
        match resolve_expression(expr, vars) {
            Ok(val) => result.push_str(&sanitize_shell_value(&val)),
            Err(_) => result.push_str(&template[start..end]),
        }
        last = end;
    }
    result.push_str(&template[last..]);
    result
}

/// Render a template string for use as process arguments.
/// Values are substituted raw (no shell escaping) since they will be
/// passed directly via `Command::new().args()`, not through a shell.
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
            Ok(val) => result.push_str(val.trim()),
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
        let json: serde_json::Value = serde_json::from_str(raw).map_err(|_| {
            anyhow!("variable '{var_name}' is not valid JSON for dot-path access")
        })?;
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
        let long = "x".repeat(200_100);
        let result = sanitize_shell_value(&long);
        assert!(result.ends_with("...(truncated)"));
        assert!(result.chars().count() < 200_100);
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
}
