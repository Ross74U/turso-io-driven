use super::runtime::{ProgramWaker, Waker};
use std::cell::Cell;
use std::sync::Arc;
use turso_core::Completion as TursoCompletion;

pub type SharedCompletion<'a> = Arc<Completion<'a>>;

pub enum Completion<'a> {
    TursoCompletion(TursoCompletion),
    AppCompletion(AppCompletion<'a>),
}

pub enum AppCompletion<'a> {
    Accept(AcceptCompletion<'a>),
    ReadSocket,
    WriteSocket,
}

impl<'a> AppCompletion<'a> {
    pub fn new_accept(waker: ProgramWaker<'a>) -> Self {
        let c = AcceptCompletion {
            waker,
            result: Cell::new(None),
        };
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

pub struct AcceptCompletion<'a> {
    waker: ProgramWaker<'a>,
    pub result: Cell<Option<i32>>,
}
impl<'a> AcceptCompletion<'a> {
    fn callback(&self, result: i32) {
        println!("accepted connection with fd: {:?}", result);
        self.result.set(Some(result));
        self.waker.wake_by_ref();
    }
}
