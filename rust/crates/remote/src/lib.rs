//! Remote file support — SSH and Docker connections.
//!
//! Mirrors Python's core/remote_file.py:
//! - SSH connections via russh for port-forwarding and file transfer
//! - Docker connections via TCP socket
//! - FileSyncServer, FileElispServer, FileCommandServer protocols

pub mod protocol;
pub mod ssh;
pub mod docker;
