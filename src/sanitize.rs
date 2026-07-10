//! Text cleanup shared by output capture (pipeline) and template
//! interpolation. AI CLIs and other tools can emit ANSI escape sequences,
//! carriage returns, and stray control characters; none of those belong in
//! commit messages or shell commands.

/// Strip ANSI escape sequences: CSI (`ESC [ ... final`), OSC
/// (`ESC ] ... BEL/ST`), charset designators (`ESC ( X` / `ESC ) X`), and
/// other two-character `ESC X` escapes.
pub fn strip_ansi_sequences(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            out.push(ch);
            continue;
        }
        match chars.peek() {
            // CSI: parameter/intermediate bytes are 0x20-0x3F, final byte 0x40-0x7E
            Some('[') => {
                chars.next();
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            // OSC: terminated by BEL or ST (ESC \)
            Some(']') => {
                chars.next();
                while let Some(c) = chars.next() {
                    if c == '\u{07}' {
                        break;
                    }
                    if c == '\u{1b}' {
                        if chars.peek() == Some(&'\\') {
                            chars.next();
                        }
                        break;
                    }
                }
            }
            // Charset designators consume a designator plus one charset byte
            Some('(') | Some(')') => {
                chars.next();
                chars.next();
            }
            // Any other simple ESC X escape
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }
    out
}

/// Normalize line endings (CRLF and lone CR become LF) and drop every other
/// control character except newline and tab.
pub fn normalize_control_chars(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .chars()
        .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
        .collect()
}

/// Full cleanup pass: strip ANSI sequences, then normalize control chars.
pub fn clean_text(text: &str) -> String {
    normalize_control_chars(&strip_ansi_sequences(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plain_text_untouched() {
        let s = "feat: plain message\n\nwith body\tand tab";
        assert_eq!(clean_text(s), s);
    }

    #[test]
    fn test_strips_color_codes() {
        assert_eq!(
            strip_ansi_sequences("\u{1b}[31mred\u{1b}[0m normal \u{1b}[1;42mbold\u{1b}[m"),
            "red normal bold"
        );
    }

    #[test]
    fn test_strips_cursor_and_erase_codes() {
        assert_eq!(
            strip_ansi_sequences("progress\u{1b}[2K\u{1b}[1Gdone"),
            "progressdone"
        );
    }

    #[test]
    fn test_strips_osc_title_bel_terminated() {
        assert_eq!(
            strip_ansi_sequences("\u{1b}]0;window title\u{07}text"),
            "text"
        );
    }

    #[test]
    fn test_strips_osc_st_terminated() {
        assert_eq!(
            strip_ansi_sequences("\u{1b}]8;;http://x\u{1b}\\link"),
            "link"
        );
    }

    #[test]
    fn test_strips_charset_designator() {
        assert_eq!(strip_ansi_sequences("\u{1b}(Bhello"), "hello");
    }

    #[test]
    fn test_bare_esc_at_end_dropped() {
        assert_eq!(strip_ansi_sequences("text\u{1b}"), "text");
    }

    #[test]
    fn test_crlf_and_lone_cr_become_lf() {
        assert_eq!(normalize_control_chars("a\r\nb\rc"), "a\nb\nc");
    }

    #[test]
    fn test_control_chars_dropped_keeps_tab_newline() {
        assert_eq!(
            normalize_control_chars("a\u{0}b\u{8}c\u{b}d\te\nf\u{7f}g"),
            "abcd\te\nfg"
        );
    }

    #[test]
    fn test_unicode_preserved() {
        let s = "提交信息 héllo 🚀";
        assert_eq!(clean_text(s), s);
    }

    #[test]
    fn test_clean_text_combined() {
        assert_eq!(
            clean_text("\u{1b}[32mfix: done\u{1b}[0m\r\n\r\nbody\u{0}"),
            "fix: done\n\nbody"
        );
    }
}
