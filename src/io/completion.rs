use std::{
    net::{TcpListener, TcpStream},
    os::fd::{AsFd, AsRawFd},
};

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
    fn callback(self) {
        match self {
            Self::Accept(c) => c.callback(),
            Self::ReadSocket => {}
            Self::WriteSocket => {}
        }
    }
}

pub struct AcceptCompletion {
    pub sock: TcpStream,
}
impl AcceptCompletion {
    fn callback(self) {
        println!("accepted connection {:?}", self.sock)
    }
}
