use crate::io::{completion::Completion, generic::ServerSocket};
use anyhow::Result;
use crossbeam::queue::ArrayQueue;
use std::sync::Arc;
use std::task::Waker;

pub struct Runtime<'a> {
    tasks: slab::Slab<Box<Program<'a>>>,
    run_queue: ArrayQueue<&'a Program<'a>>, // pointers to Program inner
}
impl<'a> Runtime<'a> {
    pub fn run_once(&self) -> Result<()> {
        todo!();
    }
}

enum Program<'a> {
    Accept(AcceptProgram<'a>),
}
impl<'a> Program<'a> {
    fn step(&mut self, waker: Waker) -> Result<()> {
        match self {
            Self::Accept(s) => s.step(waker),
        }
    }
    fn parent(&self) -> &'a Runtime {
        match self {
            Self::Accept(s) => s.parent(),
        }
    }
}

struct AcceptProgram<'a> {
    server_socket: Arc<dyn ServerSocket>,
    parent: &'a Runtime<'a>, // parent runtime
}
impl<'a> AcceptProgram<'a> {
    fn step(&mut self, waker: Waker) -> Result<()> {
        // todo: use waker
        let c = Completion::new_accept();
        let c = self.server_socket.accept(c)?; // todo, change api so c is still kept, thus result
        Ok(())
    }

    fn parent(&self) -> &'a Runtime {
        self.parent
    }
}
