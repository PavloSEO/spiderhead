//! Tool handlers and the registry domain crates plug into.

use std::collections::HashMap;
use std::sync::mpsc::Sender;

use serde_json::Value;

use crate::error::ToolError;
use crate::protocol;

/// Implemented later by domain crates; this crate ships only [`NotYetPorted`].
///
/// The call is synchronous on purpose: the server loop is line-sequential, and a
/// handler streams progress through [`CallContext`] rather than blocking the wire.
pub trait ToolHandler: Send + Sync {
    /// Execute the tool; `Ok` becomes a `structuredContent` result, `Err` an
    /// `isError` result carrying the error's `Display` text.
    fn call(&self, name: &str, args: Value, ctx: &CallContext) -> Result<Value, ToolError>;
}

/// What a running tool can tell the client while it works.
pub struct CallContext {
    progress_token: Option<Value>,
    progress_tx: Sender<Value>,
}

impl CallContext {
    pub(crate) fn new(progress_token: Option<Value>, progress_tx: Sender<Value>) -> Self {
        Self {
            progress_token,
            progress_tx,
        }
    }

    /// The `progressToken` the client supplied with the request, if any.
    pub fn progress_token(&self) -> Option<&Value> {
        self.progress_token.as_ref()
    }

    /// Emit `notifications/progress` for the in-flight request.
    ///
    /// Without a client-supplied token this is a no-op — the reference reporter
    /// stays silent in that case too. Messages are queued in order and written
    /// to the wire before the call's final response.
    pub fn progress(&self, done: f64, total: Option<f64>, message: Option<String>) {
        let Some(token) = self.progress_token() else {
            return;
        };
        let notification = protocol::progress_notification(token, done, total, message.as_deref());
        // The queue only fails if the server loop hung up, i.e. it is shutting
        // down; dropping the notification then is the right response anyway.
        let _ = self.progress_tx.send(notification);
    }
}

/// Name-keyed handler registry so domain crates attach implementations without
/// touching the server.
#[derive(Default)]
pub struct Registry {
    handlers: HashMap<String, Box<dyn ToolHandler>>,
}

impl Registry {
    /// An empty registry; the server then answers [`NotYetPorted`] for everything.
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach (or replace) the handler for one tool name.
    pub fn register(&mut self, name: &str, handler: Box<dyn ToolHandler>) {
        self.handlers.insert(name.to_string(), handler);
    }

    /// The handler registered for `name`, if any.
    pub fn get(&self, name: &str) -> Option<&dyn ToolHandler> {
        self.handlers.get(name).map(|boxed| boxed.as_ref())
    }
}

/// The placeholder handler: every tool answers "not yet available in this build".
pub struct NotYetPorted;

impl ToolHandler for NotYetPorted {
    fn call(&self, name: &str, _args: Value, _ctx: &CallContext) -> Result<Value, ToolError> {
        Err(ToolError::NotYetPorted(name.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn not_yet_ported_names_the_tool() {
        let (tx, _rx) = mpsc::channel();
        let ctx = CallContext::new(None, tx);
        let error = NotYetPorted
            .call("seo_doctor", Value::Null, &ctx)
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "seo_doctor is not yet available in this build"
        );
    }

    #[test]
    fn registry_replaces_by_name() {
        struct Stub;
        impl ToolHandler for Stub {
            fn call(
                &self,
                _name: &str,
                _args: Value,
                _ctx: &CallContext,
            ) -> Result<Value, ToolError> {
                Ok(Value::Null)
            }
        }
        let mut registry = Registry::new();
        registry.register("seo_x", Box::new(NotYetPorted));
        registry.register("seo_x", Box::new(Stub));
        assert!(registry.get("seo_x").is_some());
        assert!(registry.get("seo_y").is_none());
    }

    #[test]
    fn progress_without_token_is_silent() {
        let (tx, rx) = mpsc::channel();
        let ctx = CallContext::new(None, tx);
        ctx.progress(1.0, Some(3.0), None);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn progress_with_token_is_queued_in_order() {
        let (tx, rx) = mpsc::channel();
        let ctx = CallContext::new(Some(Value::from(7)), tx);
        ctx.progress(1.0, Some(3.0), Some("step one".into()));
        ctx.progress(2.0, Some(3.0), None);
        assert_eq!(
            rx.try_recv().unwrap()["params"]["progressToken"],
            Value::from(7)
        );
        assert_eq!(
            rx.try_recv().unwrap()["params"]["progress"],
            Value::from(2.0)
        );
    }
}
