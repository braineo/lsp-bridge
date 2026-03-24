//! EPC server: listens for connections from Emacs, dispatches method calls.
//!
//! The server binds to `127.0.0.1:0` (random port), prints the port,
//! then accepts a single connection from Emacs. Incoming `call` messages
//! are dispatched to registered async handlers.

use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::Result;
use dashmap::DashMap;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing;

use crate::sexp::{self, SexpValue};
use crate::types::EpcMessage;
use crate::wire;

/// Type alias for an async method handler function.
///
/// Takes method name and args, returns a SexpValue result.
pub type MethodHandler = Box<
    dyn Fn(Vec<SexpValue>) -> Pin<Box<dyn Future<Output = Result<SexpValue>> + Send>>
        + Send
        + Sync,
>;

/// An EPC server that listens for calls from Emacs.
pub struct EpcServer {
    /// Registered method handlers.
    methods: Arc<DashMap<String, MethodHandler>>,
    /// The local address the server is listening on.
    local_addr: SocketAddr,
    /// TCP listener.
    listener: TcpListener,
}

impl EpcServer {
    /// Create a new EPC server bound to localhost with a random port.
    pub async fn new() -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let local_addr = listener.local_addr()?;

        Ok(Self {
            methods: Arc::new(DashMap::new()),
            local_addr,
            listener,
        })
    }

    /// Get the port the server is listening on.
    pub fn port(&self) -> u16 {
        self.local_addr.port()
    }

    /// Register a method handler.
    pub fn register<F, Fut>(&self, name: impl Into<String>, handler: F)
    where
        F: Fn(Vec<SexpValue>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<SexpValue>> + Send + 'static,
    {
        let name = name.into();
        self.methods.insert(
            name,
            Box::new(move |args| Box::pin(handler(args))),
        );
    }

    /// Accept a single connection and run the message loop.
    ///
    /// Returns a handle to the connection for sending messages back.
    pub async fn accept(self) -> Result<EpcConnection> {
        let (stream, peer_addr) = self.listener.accept().await?;
        tracing::info!("EPC connection from {}", peer_addr);

        let (read_half, write_half) = stream.into_split();
        let (write_tx, write_rx) = mpsc::channel::<Vec<u8>>(256);

        let methods = self.methods;
        let pending_returns: Arc<DashMap<u64, oneshot::Sender<SexpValue>>> =
            Arc::new(DashMap::new());

        let pending_returns_clone = pending_returns.clone();
        let write_tx_clone = write_tx.clone();

        // Writer task: sends framed messages to the TCP stream
        let writer_handle = tokio::spawn(writer_loop(write_half, write_rx));

        // Reader task: reads framed messages, dispatches calls
        let reader_handle = tokio::spawn(reader_loop(
            read_half,
            methods,
            pending_returns_clone,
            write_tx_clone,
        ));

        Ok(EpcConnection {
            write_tx,
            pending_returns,
            reader_handle,
            writer_handle,
            uid_counter: Arc::new(std::sync::atomic::AtomicU64::new(1)),
        })
    }
}

/// A live EPC connection to Emacs.
pub struct EpcConnection {
    write_tx: mpsc::Sender<Vec<u8>>,
    pending_returns: Arc<DashMap<u64, oneshot::Sender<SexpValue>>>,
    reader_handle: tokio::task::JoinHandle<()>,
    writer_handle: tokio::task::JoinHandle<()>,
    uid_counter: Arc<std::sync::atomic::AtomicU64>,
}

impl EpcConnection {
    fn next_uid(&self) -> u64 {
        self.uid_counter
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }

    /// Send a raw EPC message.
    pub async fn send_message(&self, msg: &EpcMessage) -> Result<()> {
        let sexp_str = msg.encode();
        let frame = wire::encode(&sexp_str);
        self.write_tx.send(frame).await?;
        Ok(())
    }

    /// Call a method on the Emacs side and wait for the response.
    pub async fn call_sync(
        &self,
        method: &str,
        args: Vec<SexpValue>,
    ) -> Result<SexpValue> {
        let uid = self.next_uid();
        let (tx, rx) = oneshot::channel();
        self.pending_returns.insert(uid, tx);

        let msg = EpcMessage::Call {
            uid,
            method: method.to_string(),
            args,
        };
        self.send_message(&msg).await?;

        // Wait for the return
        let result = rx.await?;
        Ok(result)
    }

    /// Call a method on the Emacs side without waiting for a response (fire-and-forget).
    pub async fn call_async(
        &self,
        method: &str,
        args: Vec<SexpValue>,
    ) -> Result<()> {
        let uid = self.next_uid();
        let msg = EpcMessage::Call {
            uid,
            method: method.to_string(),
            args,
        };
        self.send_message(&msg).await?;
        Ok(())
    }

    /// Send a return response for a given uid.
    pub async fn send_return(&self, uid: u64, result: SexpValue) -> Result<()> {
        let msg = EpcMessage::Return { uid, result };
        self.send_message(&msg).await
    }

    /// Send an error response for a given uid.
    pub async fn send_error(&self, uid: u64, error: SexpValue) -> Result<()> {
        let msg = EpcMessage::ReturnError { uid, error };
        self.send_message(&msg).await
    }

    /// Check if the connection is still alive.
    pub fn is_alive(&self) -> bool {
        !self.reader_handle.is_finished() && !self.writer_handle.is_finished()
    }
}

/// Writer loop: takes framed byte messages from the channel and writes to TCP.
async fn writer_loop(
    mut write_half: tokio::net::tcp::OwnedWriteHalf,
    mut rx: mpsc::Receiver<Vec<u8>>,
) {
    while let Some(data) = rx.recv().await {
        if let Err(e) = write_half.write_all(&data).await {
            tracing::error!("EPC write error: {}", e);
            break;
        }
        if let Err(e) = write_half.flush().await {
            tracing::error!("EPC flush error: {}", e);
            break;
        }
    }
    tracing::debug!("EPC writer loop ended");
}

/// Reader loop: reads framed messages from TCP, dispatches method calls.
async fn reader_loop(
    mut read_half: tokio::net::tcp::OwnedReadHalf,
    methods: Arc<DashMap<String, MethodHandler>>,
    pending_returns: Arc<DashMap<u64, oneshot::Sender<SexpValue>>>,
    write_tx: mpsc::Sender<Vec<u8>>,
) {
    let mut decoder = wire::FrameDecoder::new();
    let mut buf = [0u8; 8192];

    loop {
        match read_half.read(&mut buf).await {
            Ok(0) => {
                tracing::info!("EPC connection closed by peer");
                break;
            }
            Ok(n) => {
                decoder.push(&buf[..n]);

                // Process all complete messages in the buffer
                loop {
                    match decoder.next_message() {
                        Ok(Some(sexp_str)) => {
                            handle_incoming_message(
                                &sexp_str,
                                &methods,
                                &pending_returns,
                                &write_tx,
                            )
                            .await;
                        }
                        Ok(None) => break, // Need more data
                        Err(e) => {
                            tracing::error!("EPC frame decode error: {}", e);
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                tracing::error!("EPC read error: {}", e);
                break;
            }
        }
    }
    tracing::debug!("EPC reader loop ended");
}

/// Handle a single incoming EPC message.
async fn handle_incoming_message(
    sexp_str: &str,
    methods: &Arc<DashMap<String, MethodHandler>>,
    pending_returns: &Arc<DashMap<u64, oneshot::Sender<SexpValue>>>,
    write_tx: &mpsc::Sender<Vec<u8>>,
) {
    let msg = match EpcMessage::decode(sexp_str) {
        Ok(msg) => msg,
        Err(e) => {
            tracing::error!("Failed to decode EPC message: {} (sexp: {:?})", e, sexp_str);
            return;
        }
    };

    match msg {
        EpcMessage::Call { uid, method, args } => {
            let methods = methods.clone();
            let write_tx = write_tx.clone();

            // Dispatch in a new task so we don't block the reader
            tokio::spawn(async move {
                let response = if let Some(handler) = methods.get(&method) {
                    match handler(args).await {
                        Ok(result) => EpcMessage::Return { uid, result },
                        Err(e) => EpcMessage::ReturnError {
                            uid,
                            error: SexpValue::String(e.to_string()),
                        },
                    }
                } else {
                    EpcMessage::EpcError {
                        uid,
                        message: format!("method not found: {}", method),
                    }
                };

                let sexp_str = response.encode();
                let frame = wire::encode(&sexp_str);
                if let Err(e) = write_tx.send(frame).await {
                    tracing::error!("Failed to send EPC response: {}", e);
                }
            });
        }
        EpcMessage::Return { uid, result } => {
            if let Some((_, tx)) = pending_returns.remove(&uid) {
                let _ = tx.send(result);
            } else {
                tracing::warn!("Received return for unknown uid: {}", uid);
            }
        }
        EpcMessage::ReturnError { uid, error } => {
            if let Some((_, tx)) = pending_returns.remove(&uid) {
                // Send error as result — caller can inspect
                let _ = tx.send(error);
            } else {
                tracing::warn!("Received return-error for unknown uid: {}", uid);
            }
        }
        EpcMessage::EpcError { uid, message } => {
            tracing::error!("EPC error (uid={}): {}", uid, message);
            if let Some((_, tx)) = pending_returns.remove(&uid) {
                let _ = tx.send(SexpValue::String(format!("epc-error: {}", message)));
            }
        }
        EpcMessage::Methods { uid } => {
            let method_names: Vec<SexpValue> = methods
                .iter()
                .map(|entry| SexpValue::Symbol(entry.key().clone()))
                .collect();
            let response = EpcMessage::Return {
                uid,
                result: SexpValue::List(method_names),
            };
            let sexp_str = response.encode();
            let frame = wire::encode(&sexp_str);
            if let Err(e) = write_tx.send(frame).await {
                tracing::error!("Failed to send methods response: {}", e);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    /// Helper: connect to EPC server and exchange messages directly over TCP.
    async fn connect_raw(port: u16) -> TcpStream {
        TcpStream::connect(format!("127.0.0.1:{}", port))
            .await
            .unwrap()
    }

    /// Helper: send a framed EPC message over a raw TCP stream.
    async fn send_raw(stream: &mut TcpStream, msg: &EpcMessage) {
        let sexp_str = msg.encode();
        let frame = wire::encode(&sexp_str);
        stream.write_all(&frame).await.unwrap();
        stream.flush().await.unwrap();
    }

    /// Helper: read one framed EPC message from a raw TCP stream.
    async fn recv_raw(stream: &mut TcpStream) -> EpcMessage {
        let mut decoder = wire::FrameDecoder::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = stream.read(&mut buf).await.unwrap();
            assert!(n > 0, "connection closed unexpectedly");
            decoder.push(&buf[..n]);
            if let Some(sexp_str) = decoder.next_message().unwrap() {
                return EpcMessage::decode(&sexp_str).unwrap();
            }
        }
    }

    #[tokio::test]
    async fn server_accepts_connection() {
        let server = EpcServer::new().await.unwrap();
        let port = server.port();
        assert!(port > 0);

        // Spawn accept in background
        let handle = tokio::spawn(async move { server.accept().await.unwrap() });

        // Connect
        let _stream = connect_raw(port).await;
        let conn = handle.await.unwrap();
        assert!(conn.is_alive());
    }

    #[tokio::test]
    async fn call_and_return() {
        let server = EpcServer::new().await.unwrap();
        let port = server.port();

        server.register("ping", |_args| async { Ok(SexpValue::String("pong".to_string())) });

        let handle = tokio::spawn(async move { server.accept().await.unwrap() });

        let mut stream = connect_raw(port).await;
        let _conn = handle.await.unwrap();

        // Send a call
        send_raw(
            &mut stream,
            &EpcMessage::Call {
                uid: 1,
                method: "ping".to_string(),
                args: vec![],
            },
        )
        .await;

        // Read response
        let response = recv_raw(&mut stream).await;
        match response {
            EpcMessage::Return { uid, result } => {
                assert_eq!(uid, 1);
                assert_eq!(result, SexpValue::String("pong".to_string()));
            }
            other => panic!("expected Return, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn call_method_not_found() {
        let server = EpcServer::new().await.unwrap();
        let port = server.port();

        let handle = tokio::spawn(async move { server.accept().await.unwrap() });

        let mut stream = connect_raw(port).await;
        let _conn = handle.await.unwrap();

        send_raw(
            &mut stream,
            &EpcMessage::Call {
                uid: 1,
                method: "nonexistent".to_string(),
                args: vec![],
            },
        )
        .await;

        let response = recv_raw(&mut stream).await;
        match response {
            EpcMessage::EpcError { uid, message } => {
                assert_eq!(uid, 1);
                assert!(message.contains("method not found"));
            }
            other => panic!("expected EpcError, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn call_with_args() {
        let server = EpcServer::new().await.unwrap();
        let port = server.port();

        server.register("echo", |args| async move {
            Ok(SexpValue::List(args))
        });

        let handle = tokio::spawn(async move { server.accept().await.unwrap() });

        let mut stream = connect_raw(port).await;
        let _conn = handle.await.unwrap();

        send_raw(
            &mut stream,
            &EpcMessage::Call {
                uid: 1,
                method: "echo".to_string(),
                args: vec![
                    SexpValue::String("hello".to_string()),
                    SexpValue::Integer(42),
                ],
            },
        )
        .await;

        let response = recv_raw(&mut stream).await;
        match response {
            EpcMessage::Return { uid, result } => {
                assert_eq!(uid, 1);
                assert_eq!(
                    result,
                    SexpValue::List(vec![
                        SexpValue::String("hello".to_string()),
                        SexpValue::Integer(42),
                    ])
                );
            }
            other => panic!("expected Return, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn call_handler_error() {
        let server = EpcServer::new().await.unwrap();
        let port = server.port();

        server.register("fail", |_args| async {
            Err(anyhow::anyhow!("intentional error"))
        });

        let handle = tokio::spawn(async move { server.accept().await.unwrap() });

        let mut stream = connect_raw(port).await;
        let _conn = handle.await.unwrap();

        send_raw(
            &mut stream,
            &EpcMessage::Call {
                uid: 1,
                method: "fail".to_string(),
                args: vec![],
            },
        )
        .await;

        let response = recv_raw(&mut stream).await;
        match response {
            EpcMessage::ReturnError { uid, error } => {
                assert_eq!(uid, 1);
                if let SexpValue::String(s) = error {
                    assert!(s.contains("intentional error"));
                }
            }
            other => panic!("expected ReturnError, got: {:?}", other),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_calls() {
        let server = EpcServer::new().await.unwrap();
        let port = server.port();

        server.register("echo_id", |args| async move {
            Ok(args.into_iter().next().unwrap_or(SexpValue::Nil))
        });

        let handle = tokio::spawn(async move { server.accept().await.unwrap() });

        let stream = connect_raw(port).await;
        let _conn = handle.await.unwrap();

        let (read_half, mut write_half) = stream.into_split();

        // Send 10 calls in a background task
        let sender = tokio::spawn(async move {
            for i in 0..10u64 {
                let msg = EpcMessage::Call {
                    uid: i,
                    method: "echo_id".to_string(),
                    args: vec![SexpValue::Integer(i as i64)],
                };
                let sexp_str = msg.encode();
                let frame = wire::encode(&sexp_str);
                write_half.write_all(&frame).await.unwrap();
                write_half.flush().await.unwrap();
            }
        });

        // Read 10 responses concurrently
        let reader = tokio::spawn(async move {
            let mut decoder = wire::FrameDecoder::new();
            let mut read_half = read_half;
            let mut buf = [0u8; 4096];
            let mut received = std::collections::HashSet::new();

            while received.len() < 10 {
                let n = read_half.read(&mut buf).await.unwrap();
                assert!(n > 0, "connection closed");
                decoder.push(&buf[..n]);
                while let Some(sexp_str) = decoder.next_message().unwrap() {
                    let msg = EpcMessage::decode(&sexp_str).unwrap();
                    match msg {
                        EpcMessage::Return { uid, result } => {
                            assert_eq!(result, SexpValue::Integer(uid as i64));
                            received.insert(uid);
                        }
                        other => panic!("expected Return, got: {:?}", other),
                    }
                }
            }
            assert_eq!(received.len(), 10);
        });

        sender.await.unwrap();
        reader.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn connection_call_sync() {
        let server = EpcServer::new().await.unwrap();
        let port = server.port();

        let handle = tokio::spawn(async move { server.accept().await.unwrap() });

        // Connect and act as Emacs (respond to calls from the Rust side)
        let mut stream = connect_raw(port).await;
        let conn = handle.await.unwrap();

        // Spawn a task that reads from the stream and responds
        let stream = Arc::new(Mutex::new(stream));
        let stream_clone = stream.clone();
        let responder = tokio::spawn(async move {
            let mut stream = stream_clone.lock().await;
            let msg = recv_raw(&mut stream).await;
            match msg {
                EpcMessage::Call { uid, method, args } => {
                    assert_eq!(method, "get-emacs-vars");
                    // Respond with a result
                    send_raw(
                        &mut stream,
                        &EpcMessage::Return {
                            uid,
                            result: SexpValue::List(vec![SexpValue::String(
                                "/usr/bin".to_string(),
                            )]),
                        },
                    )
                    .await;
                }
                other => panic!("expected Call, got: {:?}", other),
            }
        });

        // call_sync from the Rust side
        let result = conn
            .call_sync(
                "get-emacs-vars",
                vec![SexpValue::String("exec-path".to_string())],
            )
            .await
            .unwrap();

        assert_eq!(
            result,
            SexpValue::List(vec![SexpValue::String("/usr/bin".to_string())])
        );

        responder.await.unwrap();
    }

    #[tokio::test]
    async fn unicode_heavy_payload() {
        let server = EpcServer::new().await.unwrap();
        let port = server.port();

        server.register("echo", |args| async move {
            Ok(SexpValue::List(args))
        });

        let handle = tokio::spawn(async move { server.accept().await.unwrap() });

        let mut stream = connect_raw(port).await;
        let _conn = handle.await.unwrap();

        let unicode_str = "中文测试 日本語 한국어 🎉🎊✨ Ñoño café résumé";
        send_raw(
            &mut stream,
            &EpcMessage::Call {
                uid: 1,
                method: "echo".to_string(),
                args: vec![SexpValue::String(unicode_str.to_string())],
            },
        )
        .await;

        let response = recv_raw(&mut stream).await;
        match response {
            EpcMessage::Return { result, .. } => {
                assert_eq!(
                    result,
                    SexpValue::List(vec![SexpValue::String(unicode_str.to_string())])
                );
            }
            other => panic!("expected Return, got: {:?}", other),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn large_payload() {
        let server = EpcServer::new().await.unwrap();
        let port = server.port();

        server.register("echo", |args| async move {
            Ok(SexpValue::List(args))
        });

        let handle = tokio::spawn(async move { server.accept().await.unwrap() });

        let stream = connect_raw(port).await;
        let _conn = handle.await.unwrap();

        let (read_half, mut write_half) = stream.into_split();

        // 100KB string payload
        let large = "x".repeat(100_000);
        let large_clone = large.clone();

        let sender = tokio::spawn(async move {
            let msg = EpcMessage::Call {
                uid: 1,
                method: "echo".to_string(),
                args: vec![SexpValue::String(large_clone)],
            };
            let sexp_str = msg.encode();
            let frame = wire::encode(&sexp_str);
            write_half.write_all(&frame).await.unwrap();
            write_half.flush().await.unwrap();
        });

        let reader = tokio::spawn(async move {
            let mut decoder = wire::FrameDecoder::new();
            let mut read_half = read_half;
            let mut buf = [0u8; 8192];
            loop {
                let n = read_half.read(&mut buf).await.unwrap();
                assert!(n > 0);
                decoder.push(&buf[..n]);
                if let Some(sexp_str) = decoder.next_message().unwrap() {
                    return EpcMessage::decode(&sexp_str).unwrap();
                }
            }
        });

        sender.await.unwrap();
        let response = reader.await.unwrap();
        match response {
            EpcMessage::Return { result, .. } => {
                if let SexpValue::List(items) = result {
                    assert_eq!(items.len(), 1);
                    assert_eq!(items[0], SexpValue::String(large));
                } else {
                    panic!("expected list");
                }
            }
            other => panic!("expected Return, got: {:?}", other),
        }
    }
}
