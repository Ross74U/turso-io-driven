use crate::io::completion::SharedCompletion;
use std::sync::Arc;

/// generic IO trait for the application
pub trait IO {
    fn step(&self) -> anyhow::Result<()>;
    
    #[allow(unused)]
    fn register_listener(
        &self,
        listener: std::net::TcpListener,
    ) -> anyhow::Result<Arc<dyn ServerSocket>> {
        todo!("not implemented")
    }
    
    #[allow(unused)]
    fn register_connection(
        &self,
        fd: i32,
    ) -> anyhow::Result<Arc<dyn ClientConnection>> {
        todo!("not implemented")
    }
}

pub trait ServerSocket {
    fn accept(&self, c: SharedCompletion) -> anyhow::Result<()>;
}

pub trait ClientConnection {
    fn recv(&self, c: SharedCompletion) -> anyhow::Result<()> { todo!() }
    fn send(&self, c: SharedCompletion) -> anyhow::Result<()> { todo!() }
}
