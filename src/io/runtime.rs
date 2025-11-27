use crate::io::{completion::Completion, generic::ServerSocket};
use anyhow::Result;
use crossbeam::queue::ArrayQueue;

pub struct Runtime<'a> {
    tasks: slab::Slab<Box<Program<'a>>>,
    run_queue: ArrayQueue<&'a Program<'a>>, // pointers to Program inner
}
impl<'a> Runtime<'a> {
    pub fn run_once(&'a self) -> Result<()> {
        loop {
            let Some(program) = self.run_queue.pop() else {break};
            let waker = ProgramWaker {
                run_queue: &self.run_queue,
                program,
            };
            program.step(waker)?;
        }

        Ok(())
    }

    /// creates a new accept program on the tasks slab and pushes it onto run_queue
    pub fn new_accept(&mut self) -> usize {
        todo!();
    }

    /// get a readable reference to a program
    pub fn get_program(&self, id: usize) -> &'a Program<'a> {
        todo!();
    }

    /// Removes a program from the tasks slab
    pub fn deregister(&mut self) -> usize {
        todo!();
    }
}

pub enum Program<'a> {
    Accept(AcceptProgram<'a>),
}
impl<'a> Program<'a> {
    fn step(&'a self, waker: ProgramWaker<'a>) -> Result<()> {
        match self {
            Self::Accept(s) => s.step(waker),
        }
    }
    fn parent(&'a self) -> &'a Runtime<'a> {
        match self {
            Self::Accept(s) => s.parent(),
        }
    }
}

pub struct AcceptProgram<'a> {
    server_socket: Box<dyn ServerSocket>,
    parent: &'a Runtime<'a>, // parent runtime
}
impl<'a> AcceptProgram<'a> {
    fn step(&self, waker: ProgramWaker<'a>) -> Result<()> {
        let c = Completion::new_accept(waker);
        let c = self.server_socket.accept(c)?; // todo, change api so c is still kept, thus result
        Ok(())
    }

    fn parent(&self) -> &'a Runtime {
        self.parent
    }
}

pub trait Waker {
    fn wake_by_ref(&self) {}
}

pub struct ProgramWaker<'a> {
    run_queue: &'a ArrayQueue<&'a Program<'a>>,
    program: &'a Program<'a>,
}
impl<'a> Waker for ProgramWaker<'a> {
    fn wake_by_ref(&self) {
        // TODO: handle full run queue better
        self.run_queue.force_push(self.program);
    }
}
