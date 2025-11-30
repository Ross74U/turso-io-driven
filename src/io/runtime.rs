use crate::io::completion::{Completion, SharedCompletion};
use crate::io::{completion::AppCompletion, generic::ServerSocket};
use anyhow::Result;
use crossbeam::queue::ArrayQueue;
use std::cell::RefCell;
use std::sync::Arc;

pub struct Runtime<'rt> {
    programs: RefCell<slab::Slab<Box<Program<'rt>>>>,
    run_queue: ArrayQueue<usize>,
}
impl<'rt> Runtime<'rt> {
    pub fn new() -> Self {
        Runtime {
            programs: RefCell::new(slab::Slab::with_capacity(128)),
            run_queue: ArrayQueue::new(128),
        }
    }

    pub fn step(&'rt self) -> Result<()> {
        let mut programs = self.programs.borrow_mut();
        loop {
            let Some(id) = self.run_queue.pop() else {
                break;
            };
            let waker = ProgramWaker {
                program_id: id,
                run_queue: &self.run_queue,
            };
            let Some(p) = programs.get_mut(id) else {
                continue;
            };
            p.step(waker)?;
        }
        Ok(())
    }

    /// creates a new accept program on the tasks slab and pushes it onto run_queue
    pub fn new_accept(&'rt self, server_socket: Arc<dyn ServerSocket>) -> usize {
        let p = Box::new(Program::Accept(AcceptProgram {
            parent: self,
            server_socket,
            completion: None,
        }));
        self.programs.borrow_mut().insert(p)
    }

    pub fn queue(&self, id: usize) {
        self.run_queue.force_push(id);
    }

    /// get a readable reference to a program
    pub fn get_program(&self, id: usize) -> &'rt Program<'rt> {
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
    fn step(&mut self, waker: ProgramWaker<'a>) -> Result<()> {
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
    server_socket: Arc<dyn ServerSocket>,
    parent: &'a Runtime<'a>, // parent runtime
    completion: Option<SharedCompletion<'a>>,
}
impl<'a> AcceptProgram<'a> {
    fn step(&mut self, waker: ProgramWaker<'a>) -> Result<()> {
        if let Some(c) = self.completion.as_ref() {
            match c.as_ref() {
                Completion::AppCompletion(c) => match c {
                    AppCompletion::Accept(c) => {
                        println!("c result: {:?}", c.result)
                    }
                    _ => unreachable!("completion should be accept"),
                },
                _ => {
                    unreachable!("completion should be accept")
                }
            }
        }

        let c = Arc::new(Completion::AppCompletion(AppCompletion::new_accept(waker)));
        self.server_socket.accept(c.clone())?; // todo, change api so c is still kept, thus result
        self.completion = Some(c);
        println!("submitted accept completion from accept program");
        Ok(())
    }
    fn parent(&self) -> &'a Runtime {
        self.parent
    }
}

pub trait Waker<'a> {
    fn wake_by_ref(&'a self) {}
}

pub struct ProgramWaker<'a> {
    program_id: usize,
    run_queue: &'a ArrayQueue<usize>,
}

impl<'a> Waker<'a> for ProgramWaker<'a> {
    fn wake_by_ref(&'a self) {
        // TODO: handle full run queue better
        self.run_queue.force_push(self.program_id);
    }
}
