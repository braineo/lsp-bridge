//! EPC client: connects to the Emacs EPC server.
//!
//! Provides `eval_in_emacs()`, `get_emacs_vars()`, `get_emacs_func_result()`,
//! and `message_emacs()` — the main API for calling Emacs from Rust.

use std::sync::Arc;

use anyhow::Result;

use crate::server::EpcConnection;
use crate::sexp::SexpValue;
use crate::types::{self, EvalArg};

/// High-level EPC client for communicating with Emacs.
///
/// Wraps an `EpcConnection` and provides the same API surface as Python's
/// `eval_in_emacs()`, `get_emacs_vars()`, etc.
pub struct EpcClient {
    conn: Arc<EpcConnection>,
}

impl EpcClient {
    /// Create a new EPC client from an existing connection.
    pub fn new(conn: EpcConnection) -> Self {
        Self {
            conn: Arc::new(conn),
        }
    }

    /// Get a reference to the underlying connection.
    pub fn connection(&self) -> &EpcConnection {
        &self.conn
    }

    /// Evaluate an S-expression in Emacs (fire-and-forget).
    ///
    /// Equivalent to Python's `eval_in_emacs(method_name, *args)`.
    /// Constructs the sexp and sends via `(call uid "eval-in-emacs" [sexp])`.
    pub async fn eval_in_emacs(&self, method_name: &str, args: &[EvalArg]) -> Result<()> {
        let sexp_str = types::build_eval_in_emacs(method_name, args);
        self.conn
            .call_async("eval-in-emacs", vec![SexpValue::List(vec![SexpValue::String(sexp_str)])])
            .await
    }

    /// Call an Emacs function synchronously and return the result.
    ///
    /// Equivalent to Python's `get_emacs_func_result(method_name, *args)`.
    pub async fn get_emacs_func_result(
        &self,
        method_name: &str,
        args: Vec<SexpValue>,
    ) -> Result<SexpValue> {
        self.conn.call_sync(method_name, args).await
    }

    /// Get Emacs variable values.
    ///
    /// Equivalent to Python's `get_emacs_vars(args)`.
    /// Returns a list of variable values with boolean conversion applied.
    pub async fn get_emacs_vars(&self, var_specs: Vec<SexpValue>) -> Result<Vec<serde_json::Value>> {
        let result = self.conn.call_sync("get-emacs-vars", var_specs).await?;

        match result {
            SexpValue::List(items) => {
                let mut values = Vec::with_capacity(items.len());
                for item in &items {
                    match item {
                        SexpValue::List(pair) if pair.len() == 2 => {
                            let is_bool = match &pair[1] {
                                SexpValue::Bool(true) => true,
                                SexpValue::Symbol(s) if s == "t" => true,
                                _ => false,
                            };
                            values.push(types::convert_emacs_bool(&pair[0], is_bool));
                        }
                        SexpValue::List(pair) if pair.is_empty() => {
                            values.push(serde_json::Value::Bool(false));
                        }
                        other => {
                            values.push(other.to_json());
                        }
                    }
                }
                Ok(values)
            }
            other => Ok(vec![other.to_json()]),
        }
    }

    /// Send a user-visible message to Emacs.
    ///
    /// Equivalent to Python's `message_emacs(message)`.
    pub async fn message_emacs(&self, message: &str) -> Result<()> {
        self.eval_in_emacs(
            "message",
            &[EvalArg::String(format!("[LSP-Bridge] {}", message))],
        )
        .await
    }

    /// Check if the connection is still alive.
    pub fn is_alive(&self) -> bool {
        self.conn.is_alive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Client tests are primarily covered by the server integration tests.
    // The client is a thin wrapper around EpcConnection.

    #[test]
    fn eval_arg_construction() {
        // Verify EvalArg types can be constructed
        let _args: Vec<EvalArg> = vec![
            EvalArg::String("hello".to_string()),
            EvalArg::Integer(42),
            EvalArg::Float(3.14),
            EvalArg::Bool(true),
            EvalArg::Nil,
            EvalArg::QuotedSymbol("python-mode".to_string()),
            EvalArg::List(vec![SexpValue::Integer(1)]),
            EvalArg::Raw(SexpValue::Symbol("sym".to_string())),
        ];
    }
}
