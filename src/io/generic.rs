use super::completion::Completion;
use std::sync::Arc;

/// generic IO trait for the application
pub trait IO {
    fn step(&self) -> anyhow::Result<()>;
    /// register a Completion::AcceptCompletion to be executed
    fn register_listener(
        &self,
        listener: std::net::TcpListener,
    ) -> anyhow::Result<Arc<dyn ServerSocket>>;
    // TODO: read and write from socket
}

pub trait ServerSocket {
    fn accept(&self, c: Completion) -> anyhow::Result<()>;
}
