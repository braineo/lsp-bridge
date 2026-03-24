//! SSH remote file client using russh.
//!
//! Mirrors Python's RemoteFileClient from core/remote_file.py.
//! Connects via SSH, opens port-forwarding channels, and exchanges
//! JSON messages for file synchronization.

use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing;

use crate::protocol;

/// SSH connection configuration.
#[derive(Debug, Clone)]
pub struct SshConfig {
    pub hostname: String,
    pub port: u16,
    pub username: String,
    /// Authentication: key file path, or None for agent-based auth.
    pub key_file: Option<String>,
    /// Password (fallback if key auth fails).
    pub password: Option<String>,
}

/// SSH-based remote file client.
///
/// Manages an SSH connection to a remote server running lsp-bridge.
/// Uses port-forwarding channels for file sync, command, and elisp RPC.
pub struct SshClient {
    pub config: SshConfig,
    // russh connection handle would go here
    // For now this is a structural placeholder
}

impl SshClient {
    /// Create a new SSH client (does not connect yet).
    pub fn new(config: SshConfig) -> Self {
        Self { config }
    }

    /// Connect to the remote server via SSH.
    ///
    /// This establishes the SSH connection and verifies authentication.
    /// Port-forwarding channels are created separately.
    pub async fn connect(&mut self) -> Result<()> {
        tracing::info!(
            "SSH connecting to {}@{}:{}",
            self.config.username,
            self.config.hostname,
            self.config.port
        );

        // TODO: implement actual SSH connection using russh crate
        // russh::client::connect(config, (hostname, port), handler).await
        // For now, this is a structural placeholder.

        Ok(())
    }

    /// Start the lsp-bridge process on the remote server.
    pub async fn start_remote_process(&self) -> Result<()> {
        tracing::info!("Starting lsp-bridge on remote server");
        // TODO: execute "python3 lsp_bridge.py" via SSH exec
        Ok(())
    }

    /// Kill the lsp-bridge process on the remote server.
    pub async fn kill_remote_process(&self) -> Result<()> {
        tracing::info!("Killing lsp-bridge on remote server");
        // TODO: send kill signal via SSH
        Ok(())
    }

    /// Send a JSON message over a port-forwarded channel.
    pub async fn send_message(&self, _message: serde_json::Value) -> Result<()> {
        // TODO: write to SSH channel
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_config_creation() {
        let config = SshConfig {
            hostname: "example.com".to_string(),
            port: 22,
            username: "user".to_string(),
            key_file: Some("/home/user/.ssh/id_rsa".to_string()),
            password: None,
        };
        assert_eq!(config.hostname, "example.com");
        assert_eq!(config.port, 22);
    }

    #[test]
    fn ssh_client_creation() {
        let config = SshConfig {
            hostname: "localhost".to_string(),
            port: 22,
            username: "test".to_string(),
            key_file: None,
            password: Some("pass".to_string()),
        };
        let client = SshClient::new(config);
        assert_eq!(client.config.hostname, "localhost");
    }
}
