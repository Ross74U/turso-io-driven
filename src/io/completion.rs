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
    Recv(RecvCompletion<'a>),
    Send(SendCompletion<'a>),
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

    pub fn new_recv(waker: ProgramWaker<'a>, len: usize) -> Self {
        let c = RecvCompletion {
            waker,
            result: UnsafeCell::new(None),
            buf: UnsafeCell::new(vec![0u8; len])
        };
        Self::Recv(c)
    }

    pub fn callback(&self, result: i32) {
        match self {
            Self::Accept(c) => c.callback(result),
            Self::Recv(c) => c.callback(result),
            Self::Send(c) => c.callback(result),
        }
    }
}

pub struct AcceptCompletion<'a> {
    waker: ProgramWaker<'a>,
    result: UnsafeCell<Option<i32>>,
    pub addr: UnsafeCell<libc::sockaddr>,
    pub addrlen: UnsafeCell<libc::socklen_t>
}

// Aliasing: callback(), result(), addr(), and addrlen() will only be called once the
// completion has finished, meaning there shouldn't be any refs into the UnsafeCells
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
    pub fn sockaddr(&self) -> libc::sockaddr{
        unsafe { *self.addr.get() } 
    }
    pub fn addrlen(&self) -> libc::socklen_t{
        unsafe { *self.addrlen.get() } 
    }
}

pub struct RecvCompletion<'a> {
    waker: ProgramWaker<'a>,
    result: UnsafeCell<Option<i32>>,
    pub buf: UnsafeCell<Vec<u8>>
}
impl<'a> RecvCompletion<'a> {
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
    /// aliasing-rule: can only be from cqe, by program
    pub fn buf(&self) -> &[u8]{
        unsafe { &mut *self.buf.get() as & _} 
    }
    /// aliasing-rules: only used to create sqe
    pub fn buf_mut(&self) -> &mut Vec<u8>{
        unsafe { &mut *self.buf.get() as &mut _} 
    }
}

pub struct SendCompletion<'a> {
    waker: ProgramWaker<'a>,
    result: UnsafeCell<Option<i32>>,
}
impl<'a> SendCompletion<'a> {
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
}
