/// A single hunk from a unified diff, identified by a unique ID for grouping.
pub struct DiffHunk {
    pub id: usize,
    pub file_path: String,
    /// Full file header (diff --git ... through +++ line)
    pub file_header: String,
    /// Hunk header (@@ ... @@) — empty for binary/rename-only entries
    pub hunk_header: String,
    /// Hunk body (context and +/- lines) — empty for binary/rename-only entries
    pub body: String,
}

impl DiffHunk {
    /// Whether this hunk can be applied via `git apply` (has actual diff content).
    pub fn is_patchable(&self) -> bool {
        !self.hunk_header.is_empty()
    }
}

/// Parse a unified diff into individual hunks, each with a unique ID.
pub fn parse_diff(diff: &str) -> Vec<DiffHunk> {
    let mut hunks = Vec::new();
    let mut id: usize = 0;

    let mut file_path = String::new();
    let mut file_header = String::new();
    let mut hunk_header = String::new();
    let mut hunk_body = String::new();
    let mut in_hunk = false;
    let mut has_hunk_in_file = false;

    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            // Flush previous hunk or file-level entry
            if in_hunk {
                id += 1;
                hunks.push(DiffHunk {
                    id,
                    file_path: file_path.clone(),
                    file_header: file_header.clone(),
                    hunk_header: std::mem::take(&mut hunk_header),
                    body: std::mem::take(&mut hunk_body),
                });
            } else if !file_path.is_empty() && !has_hunk_in_file {
                // File with no hunks (binary, rename-only, mode-change-only)
                id += 1;
                hunks.push(DiffHunk {
                    id,
                    file_path: file_path.clone(),
                    file_header: file_header.clone(),
                    hunk_header: String::new(),
                    body: String::new(),
                });
            }

            file_path = extract_path(line);
            file_header = line.to_string();
            hunk_header.clear();
            hunk_body.clear();
            in_hunk = false;
            has_hunk_in_file = false;
        } else if line.starts_with("@@ ") {
            // Flush previous hunk in the same file
            if in_hunk {
                id += 1;
                hunks.push(DiffHunk {
                    id,
                    file_path: file_path.clone(),
                    file_header: file_header.clone(),
                    hunk_header: std::mem::take(&mut hunk_header),
                    body: std::mem::take(&mut hunk_body),
                });
            }
            hunk_header = line.to_string();
            hunk_body.clear();
            in_hunk = true;
            has_hunk_in_file = true;
        } else if in_hunk {
            if !hunk_body.is_empty() {
                hunk_body.push('\n');
            }
            hunk_body.push_str(line);
        } else {
            // Part of file header (index, ---, +++, new file mode, etc.)
            file_header.push('\n');
            file_header.push_str(line);
        }
    }

    // Flush the last entry
    if in_hunk {
        id += 1;
        hunks.push(DiffHunk {
            id,
            file_path,
            file_header,
            hunk_header,
            body: hunk_body,
        });
    } else if !file_path.is_empty() && !has_hunk_in_file {
        id += 1;
        hunks.push(DiffHunk {
            id,
            file_path,
            file_header,
            hunk_header: String::new(),
            body: String::new(),
        });
    }

    hunks
}

/// Extract the file path from a "diff --git a/path b/path" line.
fn extract_path(diff_line: &str) -> String {
    // "diff --git a/path/to/file b/path/to/file" → "path/to/file"
    diff_line
        .split(" b/")
        .last()
        .unwrap_or(diff_line)
        .to_string()
}

/// Reconstruct a valid unified diff patch from selected hunks.
/// Hunks are grouped by file header so each file header appears only once.
pub fn hunks_to_patch(hunks: &[&DiffHunk]) -> String {
    let mut patch = String::new();
    let mut last_file_header = "";

    for hunk in hunks {
        if !hunk.is_patchable() {
            continue;
        }
        if hunk.file_header != last_file_header {
            if !patch.is_empty() {
                patch.push('\n');
            }
            patch.push_str(&hunk.file_header);
            patch.push('\n');
            last_file_header = &hunk.file_header;
        }
        patch.push_str(&hunk.hunk_header);
        patch.push('\n');
        patch.push_str(&hunk.body);
        patch.push('\n');
    }

    patch
}

/// Format hunks as a numbered list for Claude to review and group.
pub fn format_hunks_for_prompt(hunks: &[DiffHunk]) -> String {
    let mut output = String::new();
    for hunk in hunks {
        output.push_str(&format!("=== HUNK {} === {}\n", hunk.id, hunk.file_path));
        if hunk.is_patchable() {
            output.push_str(&hunk.hunk_header);
            output.push('\n');
            output.push_str(&hunk.body);
        } else {
            output.push_str("(binary or rename-only change)\n");
        }
        output.push_str("\n\n");
    }
    output
}

/// Collect unique file paths from a set of hunks.
pub fn files_from_hunks<'a>(hunks: impl Iterator<Item = &'a DiffHunk>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut files = Vec::new();
    for hunk in hunks {
        if seen.insert(&hunk.file_path) {
            files.push(hunk.file_path.clone());
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_single_file_single_hunk() {
        let diff = "\
diff --git a/src/main.rs b/src/main.rs
index abc..def 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -10,3 +10,4 @@ fn main() {
     let x = 1;
+    let y = 2;
     println!(\"hi\");";

        let hunks = parse_diff(diff);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].id, 1);
        assert_eq!(hunks[0].file_path, "src/main.rs");
        assert!(hunks[0].hunk_header.starts_with("@@ "));
        assert!(hunks[0].body.contains("+    let y = 2;"));
    }

    #[test]
    fn test_parse_single_file_multiple_hunks() {
        let diff = "\
diff --git a/src/lib.rs b/src/lib.rs
index abc..def 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,4 @@
+use std::io;
 fn foo() {
     bar();
@@ -50,3 +51,3 @@ fn baz() {
-    old_call();
+    new_call();
     end();";

        let hunks = parse_diff(diff);
        assert_eq!(hunks.len(), 2);
        assert_eq!(hunks[0].id, 1);
        assert_eq!(hunks[1].id, 2);
        assert_eq!(hunks[0].file_path, "src/lib.rs");
        assert_eq!(hunks[1].file_path, "src/lib.rs");
        assert!(hunks[0].body.contains("+use std::io;"));
        assert!(hunks[1].body.contains("+    new_call();"));
    }

    #[test]
    fn test_parse_multiple_files() {
        let diff = "\
diff --git a/a.rs b/a.rs
index 111..222 100644
--- a/a.rs
+++ b/a.rs
@@ -1,2 +1,2 @@
-old_a
+new_a
diff --git a/b.rs b/b.rs
index 333..444 100644
--- a/b.rs
+++ b/b.rs
@@ -1,2 +1,2 @@
-old_b
+new_b";

        let hunks = parse_diff(diff);
        assert_eq!(hunks.len(), 2);
        assert_eq!(hunks[0].file_path, "a.rs");
        assert_eq!(hunks[1].file_path, "b.rs");
    }

    #[test]
    fn test_parse_binary_file() {
        let diff = "\
diff --git a/image.png b/image.png
new file mode 100644
index 0000000..abc1234
Binary files /dev/null and b/image.png differ
diff --git a/code.rs b/code.rs
index aaa..bbb 100644
--- a/code.rs
+++ b/code.rs
@@ -1,2 +1,2 @@
-old
+new";

        let hunks = parse_diff(diff);
        assert_eq!(hunks.len(), 2);
        assert_eq!(hunks[0].file_path, "image.png");
        assert!(!hunks[0].is_patchable());
        assert_eq!(hunks[1].file_path, "code.rs");
        assert!(hunks[1].is_patchable());
    }

    #[test]
    fn test_hunks_to_patch_groups_by_file() {
        let diff = "\
diff --git a/lib.rs b/lib.rs
index abc..def 100644
--- a/lib.rs
+++ b/lib.rs
@@ -1,3 +1,4 @@
+use std::io;
 fn foo() {
     bar();
@@ -50,3 +51,3 @@ fn baz() {
-    old_call();
+    new_call();
     end();";

        let hunks = parse_diff(diff);
        let refs: Vec<&DiffHunk> = hunks.iter().collect();
        let patch = hunks_to_patch(&refs);

        // File header should appear only once
        assert_eq!(patch.matches("diff --git").count(), 1);
        // Both hunks present (note: hunk headers like "@@ -50,3 +51,3 @@ fn..." contain "@@ " twice)
        assert!(patch.contains("@@ -1,3 +1,4 @@"));
        assert!(patch.contains("@@ -50,3 +51,3 @@"));
    }

    #[test]
    fn test_files_from_hunks() {
        let diff = "\
diff --git a/a.rs b/a.rs
index 111..222 100644
--- a/a.rs
+++ b/a.rs
@@ -1,2 +1,3 @@
+first
 line
@@ -10,2 +11,2 @@
-old
+new
diff --git a/b.rs b/b.rs
index 333..444 100644
--- a/b.rs
+++ b/b.rs
@@ -1,2 +1,2 @@
-old_b
+new_b";

        let hunks = parse_diff(diff);
        assert_eq!(hunks.len(), 3);
        let files = files_from_hunks(hunks.iter());
        assert_eq!(files, vec!["a.rs", "b.rs"]);
    }
}
