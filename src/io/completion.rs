use super::runtime::{ProgramWaker, Waker};
use std::cell::UnsafeCell;
use std::sync::Arc;
use turso_core::Completion as TursoCompletion;
#[macro_export]
macro_rules! unwrap_completion {
    ($base:ident == $variant1:ident::$variant2:ident, |$var:ident| $body_match:expr, $body_no_match:block) => {
        match $base.as_ref() {
            Completion::$variant1($variant1::$variant2($var)) => $body_match,
            _ => $body_no_match,
        }
    };
}

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
        let addr: libc::sockaddr = unsafe { std::mem::zeroed() };
        let addrlen = std::mem::size_of::<libc::sockaddr>() as libc::socklen_t;
        let c = AcceptCompletion {
            waker,
            result: UnsafeCell::new(None),
            addr: UnsafeCell::new(addr),
            addrlen: UnsafeCell::new(addrlen)
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

// let mut addr: libc::sockaddr = unsafe { std::mem::zeroed() };
// let mut addrlen = std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;

pub struct AcceptCompletion<'a> {
    waker: ProgramWaker<'a>,
    result: UnsafeCell<Option<i32>>,
    pub addr: UnsafeCell<libc::sockaddr>,
    pub addrlen: UnsafeCell<libc::socklen_t>
}
impl<'a> AcceptCompletion<'a> {
    fn callback(&self, result: i32) {
        unsafe {
            let r = &mut *self.result.get();
            *r = Some(result);
        }
        self.waker.wake_by_ref();
    }

    pub fn result(&self) -> Option<i32>{
        unsafe { *self.result.get() } 
    }

    pub fn addr(&self) -> &libc::sockaddr{
        unsafe { &*self.addr.get() } 
    }

    pub fn addrlen(&self) -> libc::socklen_t{
        unsafe { (*self.addrlen.get()).clone() } 
    }
}
