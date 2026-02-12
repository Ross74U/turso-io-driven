#![allow(clippy::arc_with_non_send_sync)]

pub mod http;
mod routes;
mod receive_body;

use crate::io::generic::IO;
use crate::unwrap_completion;

use anyhow::Result;
use crossbeam::queue::ArrayQueue;
use std::cell::RefCell;
use std::sync::Arc;
use tracing::debug;

pub type Runtime = Arc<RuntimeInner>;

pub struct RuntimeInner {
    io: Arc<dyn IO>,
    programs: RefCell<ProgramsStorage>,
    run_queue: Arc<ArrayQueue<usize>>,
}

impl RuntimeInner {
    pub fn new(io: Arc<dyn IO>) -> Runtime {
        Arc::new(Self {
            io,
            programs: RefCell::new(ProgramsStorage::new(128)),
            run_queue: Arc::new(ArrayQueue::new(128)),
        })
    }

    pub fn io(&self) -> Arc<dyn IO> {
        self.io.clone()
    }

    /// Drive runnable programs until the queue is empty.
    pub fn step(self: &Runtime) -> Result<()> {
        loop {
            let Some(id) = self.run_queue.pop() else {
                break;
            };

            let waker = ProgramWaker {
                program_id: Some(id),
                run_queue: Arc::clone(&self.run_queue),
            };

            let mut p = {
                let mut programs = self.programs.borrow_mut();
                let Some(p) = programs.take(id) else { continue };
                p
            };

            p.step(waker)?;

            // Safe try-set in case the program deregistered itself during `step`.
            self.programs.borrow_mut().try_set(id, p);
        }

        debug!("Finished rt step");
        Ok(())
    }

    pub fn queue(&self, id: usize) {
        self.run_queue.force_push(id);
    }

    pub fn register(&self, p: Box<dyn Program>) -> usize {
        self.programs.borrow_mut().insert(p)
    }

    /// Removes a program from the tasks slab.
    pub fn deregister(&self, id: usize) {
        self.programs.borrow_mut().remove(id);
    }
}

struct ProgramsStorage(slab::Slab<Option<Box<dyn Program>>>);

impl ProgramsStorage {
    fn new(capacity: usize) -> Self {
        Self(slab::Slab::with_capacity(capacity))
    }

    // panics if id is not valid
    fn take(&mut self, id: usize) -> Option<Box<dyn Program>> {
        self.0[id].take()
    }

    fn try_set(&mut self, id: usize, p: Box<dyn Program>) {
        if self.0.contains(id) {
            self.0[id] = Some(p);
        }
    }

    fn insert(&mut self, p: Box<dyn Program>) -> usize {
        self.0.insert(Some(p))
    }

    fn remove(&mut self, id: usize) -> Option<Box<dyn Program>> {
        self.0.remove(id)
    }
}

pub trait Program {
    fn step(&mut self, waker: ProgramWaker) -> Result<()> {
        unimplemented!()
    }
}

#[derive(Clone)]
pub struct ProgramWaker {
    program_id: Option<usize>,
    run_queue: Arc<ArrayQueue<usize>>,
}

impl ProgramWaker {
    pub fn id(&self) -> Option<usize> {
        self.program_id
    }

    pub fn wake_by_ref(&self) {
        if let Some(id) = self.program_id {
            self.run_queue.force_push(id);
        }
    }
}

pub enum StepResult<T> {
    Complete(T),
    Pending
}
