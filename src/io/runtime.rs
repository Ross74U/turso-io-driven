#![allow(clippy::arc_with_non_send_sync)]
use crate::io::completion::{Completion, SharedCompletion};
use crate::io::generic::{ClientConnection, IO};
use crate::io::{completion::AppCompletion, generic::ServerSocket};
use crate::unwrap_completion;

use anyhow::Result;
use crossbeam::queue::ArrayQueue;
use rustix::path::Arg;
use std::cell::RefCell;
use std::sync::Arc;
use tracing::{info};

struct ProgramsStorage<'a> (slab::Slab<Option<Box<Program<'a>>>>);
impl<'a> ProgramsStorage<'a> {
    fn new(capacity: usize) -> Self {
        Self(slab::Slab::with_capacity(capacity))
    }
    fn take(&mut self, id: usize) -> Option<Box<Program<'a>>> {
        self.0[id].take()
    }
    fn set(&mut self, id: usize, p: Box<Program<'a>>) {
        self.0[id] = Some(p);
    }
    fn insert(&mut self, p: Box<Program<'a>>) -> usize {
        self.0.insert(Some(p))
    }
}

pub struct Runtime<'rt> {
    io: Arc<dyn IO>,
    programs: RefCell<ProgramsStorage<'rt>>,
    run_queue: ArrayQueue<usize>,
}

impl<'rt> Runtime<'rt> {
    pub fn new(io: Arc<dyn IO>) -> Self {
        Runtime {
            io,
            programs: RefCell::new(ProgramsStorage::new(128)),
            run_queue: ArrayQueue::new(128),
        }
    }
    
    pub fn io(&self) -> Arc<dyn IO> {
        self.io.clone()
    }

    pub fn step(&'rt self) -> Result<()> {
        loop {
            let Some(id) = self.run_queue.pop() else {
                break;
            };
            let waker = ProgramWaker {
                program_id: id,
                run_queue: &self.run_queue,
            };
            
            let mut p = {
                let mut programs = self.programs.borrow_mut();
                let Some(p) = programs.take(id) else {continue};
                p
            };

            p.step(waker, &mut self.programs.borrow_mut())?;
            
            self.programs.borrow_mut().set(id, p);
        }
        println!("Finished rt step");
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

    pub fn new_client_handler(&'rt self, conn: Arc<dyn ClientConnection>) -> Box<Program<'rt>> {
        Box::new(Program::HandleClient(HandleClientProgram {
            conn,
            parent: self,
            completion: None,
        }))
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
    HandleClient(HandleClientProgram<'a>),
}
impl<'a> Program<'a> {
    fn step(&mut self, waker: ProgramWaker<'a>, programs: &mut ProgramsStorage<'a>) -> Result<()> {
        match self {
            Self::Accept(s) => s.step(waker, programs),
            Self::HandleClient(s) => s.step(waker),
        }
    }
    fn parent(&'a self) -> &'a Runtime<'a> {
        match self {
            Self::Accept(s) => s.parent(),
            Self::HandleClient(s) => s.parent(),
        }
    }
}

pub struct AcceptProgram<'a> {
    server_socket: Arc<dyn ServerSocket>,
    parent: &'a Runtime<'a>, // parent runtime
    completion: Option<SharedCompletion<'a>>,
}
impl<'a> AcceptProgram<'a> {
    fn step(&mut self, waker: ProgramWaker<'a>, programs: &mut ProgramsStorage<'a>) -> Result<()> {
        if let Some(c) = self.completion.as_ref() {
            unwrap_completion!(
                c == AppCompletion::Accept,
                |c| { 
                    info!(
                        "accept completion result {:?} {:?} {:?}",
                        c.result(), c.sockaddr(), c.addrlen()
                    );
                    
                    match c.sockaddr().sa_family as i32 {
                        libc::AF_INET => {},
                        _ => panic!("only support IPv4")
                    }

                    // create RecvProgram here to handle the client 
                    let conn = {
                        let Some(fd) = c.result() else { panic!("None result from accept cqe") };
                        self.parent().io().register_connection(fd)?
                    };
                    let handler_program = self.parent().new_client_handler(conn);
                    let new_id = programs.insert(handler_program);
                    self.parent().queue(new_id);
                },
                { unreachable!() }
            );
        }

        let c = Arc::new(Completion::AppCompletion(AppCompletion::new_accept(waker)));
        self.server_socket.accept(c.clone())?;
        self.completion = Some(c);
        Ok(())
    }

    fn parent(&self) -> &'a Runtime<'a> {
        self.parent
    }
}

pub struct HandleClientProgram<'a> {
    conn: Arc<dyn ClientConnection>,
    parent: &'a Runtime<'a>, // parent runtime
    completion: Option<SharedCompletion<'a>>,
}

impl<'a> HandleClientProgram<'a> {
    fn step(&mut self, waker: ProgramWaker<'a>) -> Result<()> {
        let mut eof = false;

        if let Some(c) = self.completion.as_ref() {
            unwrap_completion!(
                c == AppCompletion::Recv,
                |c| { 
                    info!("recv result: {:?}", c.result());
                    info!("recv text: {}", c.buf().to_string_lossy());
                    if c.result() == Some(0) {
                        eof = true;
                    }
                },
                { unreachable!() }
            );
        }

        if eof {
            // TODO: cleanup (close fd, remove self from programs)
            return Ok(());
        }
         
        let new_c = Arc::new(Completion::AppCompletion(AppCompletion::new_recv(waker, 64)));
        self.conn.recv(new_c.clone())?;
        self.completion = Some(new_c);
        Ok(())
    }

    fn parent(&self) -> &'a Runtime<'a> {
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
