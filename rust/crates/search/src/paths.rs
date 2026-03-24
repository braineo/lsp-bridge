//! File path search backend — directory listing with matching.
//!
//! Mirrors Python's core/search_paths.py.

use std::cmp::Ordering;
use std::path::Path;

use serde_json::{json, Value};

/// Maximum number of path candidates.
const MAX_NUMBER: usize = 300;

/// Search for files/dirs in a directory matching a prefix.
///
/// Returns candidates sorted: prefix matches first, dirs before files, then alphabetical.
pub fn search_paths(directory: &str, prefix: &str) -> Vec<Value> {
    let dir = Path::new(directory);
    if !dir.is_dir() {
        return vec![];
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return vec![],
    };

    let prefix_lower = prefix.to_lowercase();
    let mut candidates = Vec::new();

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let name_lower = name.to_lowercase();

        if match_symbol(&prefix_lower, &name_lower) {
            let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
            let file_type = if is_dir { "dir" } else { "file" };

            candidates.push(json!({
                "key": name,
                "icon": file_type,
                "label": name,
                "displayLabel": name,
                "annotation": file_type.to_uppercase(),
                "backend": "path"
            }));

            if candidates.len() > MAX_NUMBER {
                break;
            }
        }
    }

    // Sort: prefix matches first, dirs before files, then alphabetical
    candidates.sort_by(|a, b| sort_files(prefix, a, b));
    candidates
}

/// Match a symbol against a prefix.
///
/// Python: `symbol.startswith(prefix) or symbol.replace("-", "").startswith(prefix) or prefix in symbol`
fn match_symbol(prefix: &str, symbol: &str) -> bool {
    symbol.starts_with(prefix)
        || symbol.replace('-', "").starts_with(prefix)
        || symbol.contains(prefix)
}

/// Sort files: prefix matches first, dirs before files, then alphabetical.
fn sort_files(prefix: &str, a: &Value, b: &Value) -> Ordering {
    let a_key = a["key"].as_str().unwrap_or("");
    let b_key = b["key"].as_str().unwrap_or("");
    let a_starts = a_key.starts_with(prefix);
    let b_starts = b_key.starts_with(prefix);

    if a_starts && !b_starts {
        return Ordering::Less;
    }
    if !a_starts && b_starts {
        return Ordering::Greater;
    }

    let a_is_dir = a["icon"].as_str() == Some("dir");
    let b_is_dir = b["icon"].as_str() == Some("dir");

    if a_is_dir && !b_is_dir {
        return Ordering::Less;
    }
    if !a_is_dir && b_is_dir {
        return Ordering::Greater;
    }

    a_key.cmp(b_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_symbol_prefix() {
        assert!(match_symbol("hel", "hello"));
        assert!(!match_symbol("xyz", "hello"));
    }

    #[test]
    fn match_symbol_dash_removed() {
        assert!(match_symbol("foo", "f-o-o-bar")); // "foobar".starts_with("foo")
    }

    #[test]
    fn match_symbol_substring() {
        assert!(match_symbol("ello", "hello"));
    }

    #[test]
    fn sort_dirs_first() {
        let a = json!({"key": "src", "icon": "dir"});
        let b = json!({"key": "main.rs", "icon": "file"});
        assert_eq!(sort_files("", &a, &b), Ordering::Less);
    }

    #[test]
    fn sort_prefix_match_first() {
        let a = json!({"key": "main.rs", "icon": "file"});
        let b = json!({"key": "other.rs", "icon": "file"});
        assert_eq!(sort_files("main", &a, &b), Ordering::Less);
    }

    #[test]
    fn search_real_directory() {
        // Search in /tmp which should exist
        let results = search_paths("/tmp", "");
        // Should return something (even if empty on clean systems)
        assert!(results.iter().all(|r| r["backend"] == "path"));
    }

    #[test]
    fn search_nonexistent_directory() {
        let results = search_paths("/nonexistent_dir_xyz", "test");
        assert!(results.is_empty());
    }
}
