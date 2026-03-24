//! Ctags integration — runs readtags subprocess for symbol lookup.
//!
//! Mirrors Python's core/ctags.py.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use regex::Regex;
use serde_json::{json, Value};

/// Default filter expression for readtags.
pub const DEFAULT_FILTER: &str = r#"(not (or (and $extras ((string->regexp "(^|,) ?(anonymous|reference)(,|$)" :case-fold false) $extras)) (or (and $extras ((string->regexp "(^|,) ?(inputFile)(,|$)" :case-fold false) $extras)) (and $kind ((string->regexp "^(file|F)$" :case-fold false) $kind))) false))"#;

/// Default sorter expression for readtags.
pub const DEFAULT_SORTER: &str = r#"(<or> (if (and $name &name) (<> (length $name) (length &name)) 0) (if (and $name &name) (<> $name &name) 0))"#;

/// Parse a ctags output line into tag fields.
pub fn parse_tag_line(line: &str) -> Option<TagInfo> {
    if line.is_empty() {
        return None;
    }

    let (main_part, extra_part) = match line.rsplit_once(";\"") {
        Some((main, extra)) => (main, Some(extra)),
        None => (line, None),
    };

    let fields: Vec<&str> = main_part.splitn(3, '\t').collect();
    if fields.len() < 3 {
        return None;
    }

    let mut tag = TagInfo {
        name: fields[0].to_string(),
        file: fields[1].to_string(),
        address: fields[2].to_string(),
        kind: String::new(),
        typeref: String::new(),
        scope: String::new(),
        extras: String::new(),
    };

    if let Some(extra) = extra_part {
        for field in extra.split('\t').skip(1) {
            if let Some((key, value)) = field.split_once(':') {
                match key {
                    "kind" => tag.kind = value.to_string(),
                    "typeref" => tag.typeref = value.to_string(),
                    "scope" => tag.scope = value.to_string(),
                    "extras" => tag.extras = value.to_string(),
                    _ => {}
                }
            } else if !field.is_empty() {
                tag.kind = field.to_string();
            }
        }
    }

    Some(tag)
}

/// Parsed tag information.
#[derive(Debug, Clone)]
pub struct TagInfo {
    pub name: String,
    pub file: String,
    pub address: String,
    pub kind: String,
    pub typeref: String,
    pub scope: String,
    pub extras: String,
}

impl TagInfo {
    /// Build annotation string matching Python's make_tag_annotation.
    pub fn annotation(&self) -> String {
        let anon_re = Regex::new(r"(^|:)(__anon[^:]+)(:|$)").unwrap();

        let reference = if self.extras.contains("reference") {
            "<R>"
        } else {
            ""
        };

        let typeref = self
            .typeref
            .strip_prefix("typename:")
            .unwrap_or(&self.typeref);
        let typeref = anon_re.replace_all(typeref, "${1}__anon${3}");

        let scope = anon_re.replace_all(&self.scope, "${1}__anon${3}");

        let sep = if !self.kind.is_empty() && !typeref.is_empty() {
            "/"
        } else {
            ""
        };
        let at = if !scope.is_empty() { "@" } else { "" };

        format!("{}{}{}{}{}{}", reference, self.kind, sep, typeref, at, scope)
    }

    /// Convert to ACM completion candidate.
    pub fn to_candidate(&self) -> Value {
        json!({
            "key": self.name,
            "icon": if self.kind.is_empty() { "text" } else { &self.kind },
            "label": self.name,
            "displayLabel": self.name,
            "annotation": self.annotation(),
            "backend": "ctags"
        })
    }
}

/// Run readtags to get completions for a symbol prefix.
pub fn complete(symbol: &str, filename: &str) -> Result<Vec<Value>> {
    let cwd = if Path::new(filename).is_file() {
        Path::new(filename)
            .parent()
            .unwrap_or(Path::new("."))
            .to_path_buf()
    } else {
        Path::new(filename).to_path_buf()
    };

    let output = Command::new("readtags")
        .args([
            "-t", "tags",
            "-Q", DEFAULT_FILTER,
            "-S", DEFAULT_SORTER,
            "-p", "-i",
            "-", symbol,
        ])
        .current_dir(&cwd)
        .output()
        .context("failed to run readtags")?;

    if !output.status.success() {
        return Ok(vec![]);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let candidates: Vec<Value> = stdout
        .lines()
        .filter_map(|line| parse_tag_line(line))
        .map(|tag| tag.to_candidate())
        .collect();

    Ok(candidates)
}

/// Run readtags to find definition of a symbol.
pub fn find_definition(symbol: &str, filename: &str) -> Result<Vec<Value>> {
    let cwd = if Path::new(filename).is_file() {
        Path::new(filename)
            .parent()
            .unwrap_or(Path::new("."))
            .to_path_buf()
    } else {
        Path::new(filename).to_path_buf()
    };

    let output = Command::new("readtags")
        .args([
            "-t", "tags",
            "-Q", DEFAULT_FILTER,
            "-e", "-", symbol,
        ])
        .current_dir(&cwd)
        .output()
        .context("failed to run readtags")?;

    if !output.status.success() {
        return Ok(vec![]);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let results: Vec<Value> = stdout
        .lines()
        .filter_map(|line| parse_tag_line(line))
        .map(|tag| {
            json!({
                "name": tag.name,
                "file": tag.file,
                "address": tag.address,
                "annotation": tag.annotation(),
            })
        })
        .collect();

    Ok(results)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_tag() {
        let line = "my_func\tsrc/main.py\t/^def my_func/;\"\tkind:function\tscope:module";
        let tag = parse_tag_line(line).unwrap();
        assert_eq!(tag.name, "my_func");
        assert_eq!(tag.file, "src/main.py");
        assert_eq!(tag.kind, "function");
        assert_eq!(tag.scope, "module");
    }

    #[test]
    fn parse_tag_no_extras() {
        let line = "MyClass\tlib.py\t/^class MyClass/;\"";
        let tag = parse_tag_line(line).unwrap();
        assert_eq!(tag.name, "MyClass");
        assert_eq!(tag.file, "lib.py");
    }

    #[test]
    fn parse_tag_with_typeref() {
        let line = "field_x\tmod.c\t/^int field_x;\t/;\"\tkind:member\ttyperef:typename:int\tscope:struct:MyStruct";
        let tag = parse_tag_line(line).unwrap();
        assert_eq!(tag.kind, "member");
        assert_eq!(tag.typeref, "typename:int");
        assert_eq!(tag.scope, "struct:MyStruct");
    }

    #[test]
    fn parse_empty_line() {
        assert!(parse_tag_line("").is_none());
    }

    #[test]
    fn annotation_basic() {
        let tag = TagInfo {
            name: "foo".to_string(),
            file: "test.py".to_string(),
            address: "/^def foo/".to_string(),
            kind: "function".to_string(),
            typeref: String::new(),
            scope: "module:main".to_string(),
            extras: String::new(),
        };
        assert_eq!(tag.annotation(), "function@module:main");
    }

    #[test]
    fn annotation_with_typeref() {
        let tag = TagInfo {
            name: "x".to_string(),
            file: "test.c".to_string(),
            address: "".to_string(),
            kind: "member".to_string(),
            typeref: "typename:int".to_string(),
            scope: String::new(),
            extras: String::new(),
        };
        assert_eq!(tag.annotation(), "member/int");
    }

    #[test]
    fn annotation_reference() {
        let tag = TagInfo {
            name: "x".to_string(),
            file: "test.c".to_string(),
            address: "".to_string(),
            kind: "variable".to_string(),
            typeref: String::new(),
            scope: String::new(),
            extras: "reference".to_string(),
        };
        assert_eq!(tag.annotation(), "<R>variable");
    }

    #[test]
    fn annotation_anon_cleanup() {
        let tag = TagInfo {
            name: "x".to_string(),
            file: "test.c".to_string(),
            address: "".to_string(),
            kind: "member".to_string(),
            typeref: String::new(),
            scope: "struct:__anon12345abc".to_string(),
            extras: String::new(),
        };
        let ann = tag.annotation();
        assert!(ann.contains("__anon"));
        assert!(!ann.contains("__anon12345abc")); // should be cleaned
    }

    #[test]
    fn candidate_format() {
        let tag = TagInfo {
            name: "my_func".to_string(),
            file: "test.py".to_string(),
            address: "".to_string(),
            kind: "function".to_string(),
            typeref: String::new(),
            scope: String::new(),
            extras: String::new(),
        };
        let c = tag.to_candidate();
        assert_eq!(c["key"], "my_func");
        assert_eq!(c["icon"], "function");
        assert_eq!(c["backend"], "ctags");
    }
}
