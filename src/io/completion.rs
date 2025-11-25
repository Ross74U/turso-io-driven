use crate::io::generic::{ServerSocket, IO};
use std::sync::Arc;
use turso_core::Completion as TursoCompletion;

pub enum WrappedCompletion {
    TursoCompletion(TursoCompletion),
    Completion(Completion),
}

pub enum Completion {
    Accept(AcceptCompletion),
    ReadSocket,
    WriteSocket,
}

impl Completion {
    pub fn new_accept() -> Self {
        let c = AcceptCompletion {};
        Self::Accept(c)
    }

    pub fn callback(&self, result: i32) {
        match self {
            Self::Accept(c) => c.callback(result),
            Self::ReadSocket => {}
            Self::WriteSocket => {}
        }
    }
}

pub struct AcceptCompletion {}
impl AcceptCompletion {
    fn callback(&self, result: i32) {
        println!("accepted connection with fd: {:?}", result);
    }
}
