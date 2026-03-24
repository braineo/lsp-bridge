//! Generic symbol list search backend.
//!
//! Mirrors Python's core/search_list.py.
//! Provides updateable symbol lists with prefix/fuzzy matching.

use std::collections::HashMap;
use std::sync::Mutex;

use regex::Regex;
use serde_json::{json, Value};

/// A search list backend that holds symbol lists per named backend.
pub struct SearchList {
    backends: Mutex<HashMap<String, BackendData>>,
}

struct BackendData {
    symbols: Vec<String>,
    max_number: usize,
}

impl SearchList {
    pub fn new() -> Self {
        Self {
            backends: Mutex::new(HashMap::new()),
        }
    }

    /// Update the symbol list for a backend.
    pub fn update(&self, backend_name: &str, symbols: Vec<String>, max_number: usize) {
        let mut sorted = symbols;
        sorted.sort_by_key(|s| s.len());

        self.backends.lock().unwrap().insert(
            backend_name.to_string(),
            BackendData {
                symbols: sorted,
                max_number,
            },
        );
    }

    /// Search for symbols matching prefix in the given backend.
    pub fn search(&self, backend_name: &str, prefix: &str) -> Vec<String> {
        let backends = self.backends.lock().unwrap();
        let data = match backends.get(backend_name) {
            Some(d) => d,
            None => return vec![],
        };

        // Build fuzzy regex: "abc" → "a.*b.*c.*"
        let escaped = regex::escape(prefix);
        let pattern: String = escaped
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
            .map(|c| format!("{}.*", c))
            .collect();
        let fuzzy_re = Regex::new(&pattern).ok();

        let mut candidates: Vec<String> = data
            .symbols
            .iter()
            .filter(|sym| match_symbol(prefix, fuzzy_re.as_ref(), sym))
            .take(data.max_number)
            .cloned()
            .collect();

        // Sort: prefix matches first, then by length
        let prefix_owned = prefix.to_string();
        candidates.sort_by(|a, b| {
            let a_starts = a.starts_with(&prefix_owned);
            let b_starts = b.starts_with(&prefix_owned);
            match (a_starts, b_starts) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.len().cmp(&b.len()),
            }
        });

        candidates
    }
}

impl Default for SearchList {
    fn default() -> Self {
        Self::new()
    }
}

/// Match a symbol against a prefix using multiple strategies.
///
/// Python: `symbol.startswith(prefix) or symbol.replace("-", "").startswith(prefix) or prefix_regexp.match(symbol)`
fn match_symbol(prefix: &str, fuzzy_re: Option<&Regex>, symbol: &str) -> bool {
    if symbol.starts_with(prefix) {
        return true;
    }
    if symbol.replace('-', "").starts_with(prefix) {
        return true;
    }
    if let Some(re) = fuzzy_re {
        if re.is_match(symbol) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_and_search() {
        let sl = SearchList::new();
        sl.update(
            "test",
            vec![
                "hello".to_string(),
                "help".to_string(),
                "world".to_string(),
                "helicopter".to_string(),
            ],
            100,
        );
        let results = sl.search("test", "hel");
        assert!(results.contains(&"hello".to_string()));
        assert!(results.contains(&"help".to_string()));
        assert!(results.contains(&"helicopter".to_string()));
        assert!(!results.contains(&"world".to_string()));
    }

    #[test]
    fn search_dash_removal() {
        let sl = SearchList::new();
        sl.update("test", vec!["my-func".to_string()], 100);
        let results = sl.search("test", "myfunc");
        assert!(results.contains(&"my-func".to_string()));
    }

    #[test]
    fn search_fuzzy_regex() {
        let sl = SearchList::new();
        sl.update("test", vec!["buffer-string".to_string()], 100);
        let results = sl.search("test", "bs");
        assert!(results.contains(&"buffer-string".to_string()));
    }

    #[test]
    fn search_empty_backend() {
        let sl = SearchList::new();
        let results = sl.search("nonexistent", "hello");
        assert!(results.is_empty());
    }

    #[test]
    fn search_respects_max() {
        let sl = SearchList::new();
        let symbols: Vec<String> = (0..200).map(|i| format!("sym{}", i)).collect();
        sl.update("test", symbols, 10);
        let results = sl.search("test", "sym");
        assert!(results.len() <= 10);
    }

    #[test]
    fn sort_prefix_first() {
        let sl = SearchList::new();
        sl.update(
            "test",
            vec![
                "something".to_string(),
                "prefix-long".to_string(),
                "prefix".to_string(),
            ],
            100,
        );
        let results = sl.search("test", "prefix");
        assert_eq!(results[0], "prefix");
    }

    // ===== Python ground truth tests =====
    // Generated via: uv run python3 with core/search_list.py logic

    #[test]
    fn python_fuzzy_regex_pattern() {
        // Python: re.sub(r'([a-zA-Z0-9-_])', r'\1.*', re.escape("bs")) → "b.*s.*"
        // "buffer-string" matches b.*s.* → True
        // "buffer-size" matches b.*s.* → True
        // "abc" does NOT match b.*s.* → False
        // "basic" matches b.*s.* → True
        let sl = SearchList::new();
        sl.update(
            "t",
            vec![
                "buffer-string".to_string(),
                "buffer-size".to_string(),
                "abc".to_string(),
                "basic".to_string(),
            ],
            100,
        );
        let results = sl.search("t", "bs");
        assert!(results.contains(&"buffer-string".to_string()));
        assert!(results.contains(&"buffer-size".to_string()));
        assert!(!results.contains(&"abc".to_string()));
        assert!(results.contains(&"basic".to_string()));
    }

    #[test]
    fn python_dash_removal_match() {
        // Python: symbol.replace("-", "").startswith(prefix)
        // "my-func".replace("-","") = "myfunc".startswith("myfunc") → True
        let sl = SearchList::new();
        sl.update("t", vec!["my-func".to_string(), "other".to_string()], 100);
        let results = sl.search("t", "myfunc");
        assert!(results.contains(&"my-func".to_string()));
        assert!(!results.contains(&"other".to_string()));
    }
}
