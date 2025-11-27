use super::runtime::{ProgramWaker, Waker};
use turso_core::Completion as TursoCompletion;

pub enum WrappedCompletion<'a> {
    TursoCompletion(TursoCompletion),
    Completion(Completion<'a>),
}

pub enum Completion<'a> {
    Accept(AcceptCompletion<'a>),
    ReadSocket,
    WriteSocket,
}

impl<'a> Completion<'a> {
    pub fn new_accept(waker: ProgramWaker<'a>) -> Self {
        let c = AcceptCompletion { waker };
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
}
impl<'a> AcceptCompletion<'a> {
    fn callback(&self, result: i32) {
        println!("accepted connection with fd: {:?}", result);
        self.waker.wake_by_ref();
    }
}
