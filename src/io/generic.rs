use super::completion::Completion;

/// generic IO trait for the application
pub trait IO {
    fn step(&self) -> anyhow::Result<()>;
    /// register a Completion::AcceptCompletion to be executed
    fn register_listener(
        &mut self,
        listener: std::net::TcpListener,
    ) -> anyhow::Result<impl ServerSocket>;
    // TODO: read and write from socket
}

pub trait ServerSocket {
    fn accept(&mut self, c: Completion) -> anyhow::Result<()>;
}
