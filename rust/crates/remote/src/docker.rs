//! Docker remote file client using TCP socket.
//!
//! Mirrors Python's DockerFileClient from core/remote_file.py.
//! Connects to a Docker container via TCP socket for file sync.

use std::net::SocketAddr;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tracing;

use crate::protocol;

/// Docker container connection configuration.
#[derive(Debug, Clone)]
pub struct DockerConfig {
    pub container_name: String,
    pub host: String,
    pub port: u16,
}

/// Docker-based remote file client.
///
/// Connects to a Docker container running lsp-bridge via TCP socket.
pub struct DockerClient {
    pub config: DockerConfig,
    stream: Option<TcpStream>,
}

impl DockerClient {
    /// Create a new Docker client (does not connect yet).
    pub fn new(config: DockerConfig) -> Self {
        Self {
            config,
            stream: None,
        }
    }

    /// Connect to the Docker container's lsp-bridge instance.
    pub async fn connect(&mut self) -> Result<()> {
        let addr = format!("{}:{}", self.config.host, self.config.port);
        tracing::info!(
            "Docker connecting to {} (container: {})",
            addr,
            self.config.container_name
        );

        let stream = TcpStream::connect(&addr)
            .await
            .with_context(|| format!("failed to connect to Docker container at {}", addr))?;

        self.stream = Some(stream);
        Ok(())
    }

    /// Send a JSON message to the Docker container.
    pub async fn send_message(&mut self, message: serde_json::Value) -> Result<()> {
        let stream = self
            .stream
            .as_mut()
            .context("not connected to Docker container")?;

        let data = protocol::encode_message(&message);
        stream.write_all(&data).await?;
        stream.flush().await?;
        Ok(())
    }

    /// Read one JSON message from the Docker container.
    pub async fn recv_message(&mut self) -> Result<Option<serde_json::Value>> {
        let stream = self
            .stream
            .as_mut()
            .context("not connected to Docker container")?;

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Ok(None); // Connection closed
        }

        Ok(protocol::decode_message(&line))
    }

    /// Check if connected.
    pub fn is_connected(&self) -> bool {
        self.stream.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_config_creation() {
        let config = DockerConfig {
            container_name: "my-dev".to_string(),
            host: "127.0.0.1".to_string(),
            port: 9999,
        };
        assert_eq!(config.container_name, "my-dev");
    }

    #[test]
    fn docker_client_not_connected() {
        let client = DockerClient::new(DockerConfig {
            container_name: "test".to_string(),
            host: "127.0.0.1".to_string(),
            port: 9999,
        });
        assert!(!client.is_connected());
    }
}
