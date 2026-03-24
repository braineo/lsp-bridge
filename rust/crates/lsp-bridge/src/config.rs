//! Server configuration loading from langserver/*.json and multiserver/*.json.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Configuration for a single LSP server, loaded from langserver/*.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfig {
    /// Server name (e.g., "pyright", "rust-analyzer")
    pub name: String,

    /// Language ID (e.g., "python", "rust")
    #[serde(rename = "languageId")]
    pub language_id: String,

    /// Command to start the server
    pub command: Vec<String>,

    /// Server-specific settings
    #[serde(default)]
    pub settings: serde_json::Value,

    /// Project files to detect root (e.g., ["Cargo.toml"])
    #[serde(default, rename = "projectFiles")]
    pub project_files: Vec<String>,

    /// Whether the server supports single-file mode
    #[serde(default, rename = "support-single-file")]
    pub support_single_file: bool,

    /// Custom capabilities to send during initialization
    #[serde(default)]
    pub capabilities: serde_json::Value,

    /// Custom initialization options
    #[serde(default, rename = "initializationOptions")]
    pub initialization_options: serde_json::Value,

    /// Virtual file path for org-babel support
    #[serde(default, rename = "orgBabelVirtualFile")]
    pub org_babel_virtual_file: Option<String>,

    /// Completion trigger characters override
    #[serde(default, rename = "completionTriggerCharacters")]
    pub completion_trigger_characters: Option<Vec<String>>,
}

/// Configuration for multi-server setups, loaded from multiserver/*.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiServerConfig {
    /// Default server for methods not explicitly mapped
    pub default: String,

    /// Server(s) for diagnostics
    #[serde(default)]
    pub diagnostics: Option<MultiServerTarget>,

    /// Server(s) for code actions
    #[serde(default)]
    pub code_action: Option<MultiServerTarget>,

    /// Server(s) for execute_command
    #[serde(default)]
    pub execute_command: Option<MultiServerTarget>,

    /// Server for formatting
    #[serde(default)]
    pub formatting: Option<MultiServerTarget>,

    /// Server(s) for completion
    #[serde(default)]
    pub completion: Option<MultiServerTarget>,

    /// Server for find_define (go to definition)
    #[serde(default)]
    pub find_define: Option<MultiServerTarget>,

    /// Server for find_references
    #[serde(default)]
    pub find_references: Option<MultiServerTarget>,

    /// Server for rename
    #[serde(default)]
    pub rename: Option<MultiServerTarget>,

    /// Server for hover
    #[serde(default)]
    pub hover: Option<MultiServerTarget>,
}

/// A multi-server target: either a single server name or a list of names.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MultiServerTarget {
    Single(String),
    Multiple(Vec<String>),
}

impl MultiServerTarget {
    /// Get the server names as a list.
    pub fn names(&self) -> Vec<&str> {
        match self {
            MultiServerTarget::Single(s) => vec![s.as_str()],
            MultiServerTarget::Multiple(v) => v.iter().map(|s| s.as_str()).collect(),
        }
    }
}

/// Load a single server config from a JSON file.
pub fn load_server_config(path: &Path) -> Result<ServerConfig> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read config: {}", path.display()))?;
    let config: ServerConfig = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse config: {}", path.display()))?;
    Ok(config)
}

/// Load a multi-server config from a JSON file.
pub fn load_multi_server_config(path: &Path) -> Result<MultiServerConfig> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read config: {}", path.display()))?;
    let config: MultiServerConfig = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse config: {}", path.display()))?;
    Ok(config)
}

/// Load all server configs from a directory.
pub fn load_all_server_configs(dir: &Path) -> Result<HashMap<String, ServerConfig>> {
    let mut configs = HashMap::new();
    if !dir.exists() {
        return Ok(configs);
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().map(|e| e == "json").unwrap_or(false) {
            match load_server_config(&path) {
                Ok(config) => {
                    configs.insert(config.name.clone(), config);
                }
                Err(e) => {
                    tracing::warn!("Skipping invalid config {}: {}", path.display(), e);
                }
            }
        }
    }
    Ok(configs)
}

/// Load all multi-server configs from a directory.
pub fn load_all_multi_server_configs(dir: &Path) -> Result<HashMap<String, MultiServerConfig>> {
    let mut configs = HashMap::new();
    if !dir.exists() {
        return Ok(configs);
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().map(|e| e == "json").unwrap_or(false) {
            match load_multi_server_config(&path) {
                Ok(config) => {
                    let name = path.file_stem().unwrap().to_string_lossy().to_string();
                    configs.insert(name, config);
                }
                Err(e) => {
                    tracing::warn!("Skipping invalid multiserver config {}: {}", path.display(), e);
                }
            }
        }
    }
    Ok(configs)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pyright_config() {
        let json = r#"{
            "name": "pyright",
            "languageId": "python",
            "command": ["pyright-langserver", "--stdio"],
            "settings": {"python.analysis": {"autoImportCompletions": true}}
        }"#;
        let config: ServerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.name, "pyright");
        assert_eq!(config.language_id, "python");
        assert_eq!(config.command, vec!["pyright-langserver", "--stdio"]);
        assert!(config.settings["python.analysis"]["autoImportCompletions"].as_bool().unwrap());
    }

    #[test]
    fn parse_rust_analyzer_config() {
        let json = r#"{
            "name": "rust-analyzer",
            "languageId": "rust",
            "command": ["rust-analyzer"],
            "settings": {},
            "projectFiles": ["Cargo.toml"],
            "capabilities": {"textDocument": {"hover": {"contentFormat": ["markdown"]}}},
            "initializationOptions": {"diagnostics": {"enable": true}}
        }"#;
        let config: ServerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.name, "rust-analyzer");
        assert_eq!(config.project_files, vec!["Cargo.toml"]);
        assert!(config.capabilities["textDocument"].is_object());
        assert!(config.initialization_options["diagnostics"]["enable"].as_bool().unwrap());
    }

    #[test]
    fn parse_config_with_org_babel() {
        let json = r#"{
            "name": "rust-analyzer",
            "languageId": "rust",
            "command": ["rust-analyzer"],
            "orgBabelVirtualFile": "org_examples/src/main.rs"
        }"#;
        let config: ServerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(
            config.org_babel_virtual_file.as_deref(),
            Some("org_examples/src/main.rs")
        );
    }

    #[test]
    fn parse_config_minimal() {
        let json = r#"{
            "name": "test",
            "languageId": "test",
            "command": ["test-server"]
        }"#;
        let config: ServerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.name, "test");
        assert!(config.project_files.is_empty());
        assert!(!config.support_single_file);
        assert!(config.capabilities.is_null());
    }

    #[test]
    fn parse_multiserver_config() {
        let json = r#"{
            "default": "pyright",
            "diagnostics": ["pyright", "ruff"],
            "code_action": ["pyright", "ruff"],
            "execute_command": ["pyright", "ruff"],
            "formatting": "ruff"
        }"#;
        let config: MultiServerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.default, "pyright");
        match &config.diagnostics {
            Some(MultiServerTarget::Multiple(v)) => {
                assert_eq!(v, &["pyright", "ruff"]);
            }
            other => panic!("expected Multiple, got: {:?}", other),
        }
        match &config.formatting {
            Some(MultiServerTarget::Single(s)) => assert_eq!(s, "ruff"),
            other => panic!("expected Single, got: {:?}", other),
        }
    }

    #[test]
    fn multiserver_target_names() {
        let single = MultiServerTarget::Single("pyright".to_string());
        assert_eq!(single.names(), vec!["pyright"]);

        let multi = MultiServerTarget::Multiple(vec![
            "pyright".to_string(),
            "ruff".to_string(),
        ]);
        assert_eq!(multi.names(), vec!["pyright", "ruff"]);
    }

    #[test]
    fn load_all_real_langserver_configs() {
        // Test that ALL langserver/*.json files parse successfully
        let langserver_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("langserver");

        if !langserver_dir.exists() {
            println!("Skipping: langserver dir not found at {:?}", langserver_dir);
            return;
        }

        let mut count = 0;
        let mut failures = Vec::new();
        for entry in std::fs::read_dir(&langserver_dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.extension().map(|e| e == "json").unwrap_or(false) {
                count += 1;
                if let Err(e) = load_server_config(&path) {
                    failures.push(format!("{}: {}", path.display(), e));
                }
            }
        }

        assert!(count > 50, "expected 50+ langserver configs, found {}", count);
        assert!(
            failures.is_empty(),
            "Failed to parse {} configs:\n{}",
            failures.len(),
            failures.join("\n")
        );
        println!("Successfully parsed {} langserver configs", count);
    }

    #[test]
    fn load_all_real_multiserver_configs() {
        let multiserver_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("multiserver");

        if !multiserver_dir.exists() {
            println!("Skipping: multiserver dir not found at {:?}", multiserver_dir);
            return;
        }

        let mut count = 0;
        let mut failures = Vec::new();
        for entry in std::fs::read_dir(&multiserver_dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.extension().map(|e| e == "json").unwrap_or(false) {
                count += 1;
                if let Err(e) = load_multi_server_config(&path) {
                    failures.push(format!("{}: {}", path.display(), e));
                }
            }
        }

        assert!(count > 0, "expected multiserver configs, found {}", count);
        assert!(
            failures.is_empty(),
            "Failed to parse {} multiserver configs:\n{}",
            failures.len(),
            failures.join("\n")
        );
        println!("Successfully parsed {} multiserver configs", count);
    }
}
