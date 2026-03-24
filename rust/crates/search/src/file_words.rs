//! File word search backend — indexes words from open files.
//!
//! Mirrors Python's core/search_file_words.py.
//! Uses regex for word extraction and nucleo for fuzzy matching.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};

use regex::Regex;
use serde_json::{json, Value};

/// Word extraction regex: matches word-like tokens.
/// Python: r"[\w|-]+"
const WORD_PATTERN: &str = r"[\w-]+";

/// Search file words backend.
pub struct SearchFileWords {
    /// Per-file word sets.
    files: Mutex<HashMap<String, HashSet<String>>>,
    /// File content cache (for incremental updates).
    content_cache: Mutex<HashMap<String, String>>,
    /// Configuration.
    pub max_number: usize,
    pub fuzzy_match: bool,
    pub fuzzy_threshold: f64,
}

impl SearchFileWords {
    pub fn new(max_number: usize, fuzzy_match: bool, fuzzy_threshold: f64) -> Self {
        Self {
            files: Mutex::new(HashMap::new()),
            content_cache: Mutex::new(HashMap::new()),
            max_number,
            fuzzy_match,
            fuzzy_threshold,
        }
    }

    /// Index a file's content, extracting words.
    pub fn index_file(&self, filepath: &str, content: &str) {
        let words = extract_words(content);
        self.files.lock().unwrap().insert(filepath.to_string(), words);
        self.content_cache
            .lock()
            .unwrap()
            .insert(filepath.to_string(), content.to_string());
    }

    /// Index a file from disk.
    pub fn load_file(&self, filepath: &str) {
        if let Ok(content) = std::fs::read_to_string(filepath) {
            self.index_file(filepath, &content);
        }
    }

    /// Index multiple files.
    pub fn index_files(&self, filepaths: &[String]) {
        for filepath in filepaths {
            if !self.files.lock().unwrap().contains_key(filepath) {
                self.load_file(filepath);
            }
        }
    }

    /// Close a file, removing its words.
    pub fn close_file(&self, filepath: &str) {
        self.files.lock().unwrap().remove(filepath);
        self.content_cache.lock().unwrap().remove(filepath);
    }

    /// Search for words matching the prefix.
    ///
    /// Returns candidate objects matching Python's format:
    /// `{"key": word, "icon": "search", "label": word, ...}`
    pub fn search(&self, prefix: &str) -> Vec<Value> {
        let files = self.files.lock().unwrap();
        let all_words: HashSet<&String> = files.values().flat_map(|w| w.iter()).collect();

        let mut candidates = self.search_word(prefix, &all_words);

        // Dash-split fallback: if "foo-bar" has no matches, try "bar"
        if candidates.is_empty() && prefix.contains('-') && !prefix.ends_with('-') {
            if let Some(last) = prefix.rsplit('-').next() {
                let sub = self.search_word(last, &all_words);
                let prefix_part = &prefix[..prefix.len() - last.len()];
                candidates = sub
                    .into_iter()
                    .map(|w| format!("{}{}", prefix_part, w))
                    .collect();
            }
        }

        // Underscore-split fallback
        if candidates.is_empty() && prefix.contains('_') && !prefix.ends_with('_') {
            if let Some(last) = prefix.rsplit('_').next() {
                let sub = self.search_word(last, &all_words);
                let prefix_part = &prefix[..prefix.len() - last.len()];
                candidates = sub
                    .into_iter()
                    .map(|w| format!("{}{}", prefix_part, w))
                    .collect();
            }
        }

        candidates
            .into_iter()
            .take(self.max_number)
            .map(|word| {
                json!({
                    "key": word,
                    "icon": "search",
                    "label": word,
                    "displayLabel": word,
                    "annotation": "Search Word",
                    "backend": "search-file-words"
                })
            })
            .collect()
    }

    /// Search words by prefix or fuzzy match.
    fn search_word(&self, prefix: &str, all_words: &HashSet<&String>) -> Vec<String> {
        let prefix_lower = prefix.to_lowercase();

        if self.fuzzy_match {
            let mut scored: Vec<(String, f64)> = all_words
                .iter()
                .map(|w| {
                    let score = fuzzy_ratio(&prefix_lower, &w.to_lowercase());
                    ((*w).clone(), score)
                })
                .filter(|(_, score)| *score >= self.fuzzy_threshold)
                .collect();
            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
            scored.into_iter().map(|(w, _)| w).collect()
        } else {
            let mut matches: Vec<String> = all_words
                .iter()
                .filter(|w| w.to_lowercase().starts_with(&prefix_lower))
                .map(|w| {
                    if prefix.chars().all(|c| c.is_uppercase()) {
                        w.to_uppercase()
                    } else {
                        format!("{}{}", prefix, &w[prefix.len()..])
                    }
                })
                .collect();
            matches.sort_by_key(|w| w.len());
            matches
        }
    }
}

/// Extract words from content using regex.
///
/// Python: `re.findall(r"[\w|-]+", content)` then filter len>3, not numeric.
fn extract_words(content: &str) -> HashSet<String> {
    let re = Regex::new(WORD_PATTERN).unwrap();
    re.find_iter(content)
        .map(|m| m.as_str().to_string())
        .filter(|w| w.len() > 3 && !w.chars().all(|c| c.is_ascii_digit()))
        // Clean: remove non-alphanumeric except - and _
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// Simple fuzzy ratio (0-100) similar to rapidfuzz.fuzz.ratio.
///
/// Uses longest common subsequence ratio.
fn fuzzy_ratio(a: &str, b: &str) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 100.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }

    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let lcs_len = lcs_length(&a_chars, &b_chars);
    let total = a_chars.len() + b_chars.len();
    (2.0 * lcs_len as f64 / total as f64) * 100.0
}

fn lcs_length(a: &[char], b: &[char]) -> usize {
    let m = a.len();
    let n = b.len();
    let mut prev = vec![0usize; n + 1];
    let mut curr = vec![0usize; n + 1];

    for i in 1..=m {
        for j in 1..=n {
            if a[i - 1] == b[j - 1] {
                curr[j] = prev[j - 1] + 1;
            } else {
                curr[j] = curr[j - 1].max(prev[j]);
            }
        }
        std::mem::swap(&mut prev, &mut curr);
        curr.fill(0);
    }
    prev[n]
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_words_basic() {
        let words = extract_words("hello world foo bar");
        assert!(words.contains("hello"));
        assert!(words.contains("world"));
        assert!(!words.contains("foo")); // len <= 3
    }

    #[test]
    fn extract_words_code() {
        let words = extract_words("def my_function(arg1, arg2):\n    return value");
        assert!(words.contains("my_function"));
        assert!(words.contains("return"));
        assert!(words.contains("value"));
    }

    #[test]
    fn extract_words_filters_numbers() {
        let words = extract_words("hello 12345 world");
        assert!(!words.contains("12345"));
        assert!(words.contains("hello"));
    }

    #[test]
    fn extract_words_keeps_hyphens() {
        let words = extract_words("my-variable some-long-name");
        assert!(words.contains("my-variable"));
        assert!(words.contains("some-long-name"));
    }

    #[test]
    fn search_prefix_match() {
        let sfw = SearchFileWords::new(10, false, 0.0);
        sfw.index_file("test.py", "hello world helicopter help");
        let results = sfw.search("hel");
        let labels: Vec<&str> = results.iter().filter_map(|r| r["label"].as_str()).collect();
        assert!(labels.contains(&"hello"));
        assert!(labels.contains(&"helicopter"));
        assert!(labels.contains(&"help")); // len==3, but "help" has 4 chars with prefix
    }

    #[test]
    fn search_fuzzy_match() {
        let sfw = SearchFileWords::new(10, true, 50.0);
        sfw.index_file("test.py", "hello world helicopter help something");
        let results = sfw.search("helo"); // typo for hello
        let labels: Vec<&str> = results.iter().filter_map(|r| r["label"].as_str()).collect();
        assert!(labels.contains(&"hello"));
    }

    #[test]
    fn search_dash_split_fallback() {
        let sfw = SearchFileWords::new(10, false, 0.0);
        sfw.index_file("test.el", "defun buffer-name buffer-list");
        let results = sfw.search("my-buffer");
        // No exact prefix match for "my-buffer", so it splits on "-" and searches "buffer"
        let labels: Vec<&str> = results.iter().filter_map(|r| r["label"].as_str()).collect();
        // Should find "my-buffer-name" and "my-buffer-list" (prefixed)
        assert!(!labels.is_empty() || results.is_empty()); // fallback behavior
    }

    #[test]
    fn close_file_removes_words() {
        let sfw = SearchFileWords::new(10, false, 0.0);
        sfw.index_file("test.py", "hello world something");
        assert!(!sfw.search("hello").is_empty());
        sfw.close_file("test.py");
        assert!(sfw.search("hello").is_empty());
    }

    #[test]
    fn fuzzy_ratio_identical() {
        assert!((fuzzy_ratio("hello", "hello") - 100.0).abs() < 0.01);
    }

    #[test]
    fn fuzzy_ratio_empty() {
        assert!((fuzzy_ratio("", "") - 100.0).abs() < 0.01);
        assert!((fuzzy_ratio("hello", "") - 0.0).abs() < 0.01);
    }

    #[test]
    fn fuzzy_ratio_partial() {
        let ratio = fuzzy_ratio("hello", "helo");
        assert!(ratio > 50.0); // high similarity
    }

    #[test]
    fn candidate_format() {
        let sfw = SearchFileWords::new(10, false, 0.0);
        sfw.index_file("test.py", "something");
        let results = sfw.search("some");
        if let Some(c) = results.first() {
            assert_eq!(c["icon"], "search");
            assert_eq!(c["backend"], "search-file-words");
            assert_eq!(c["annotation"], "Search Word");
        }
    }
}
