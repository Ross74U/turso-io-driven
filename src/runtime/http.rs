use anyhow::Result;
use std::sync::Arc;
use super::{Runtime, Program, ProgramWaker, unwrap_completion};
use crate::io::completion::{Completion, SharedCompletion, AppCompletion};
use crate::io::generic::{ServerSocket, ClientConnection};
use tracing::{info};

pub struct HttpServer {
    server_sock: Arc<dyn ServerSocket>,
    parent: Runtime, // parent runtime
    completion: Option<SharedCompletion>,
}

impl HttpServer {
    pub fn new(server_sock: Arc<dyn ServerSocket>, parent: Runtime) -> Box<Self> {
        Box::new(HttpServer { server_sock, parent, completion: None })
    }
} 

impl Program for HttpServer {
    fn step(&mut self, waker: ProgramWaker) -> Result<()> {
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

                    let conn = {
                        let Some(fd) = c.result() else { panic!("None result from accept cqe") };
                        self.parent.io().register_connection(fd)?
                    };
                    let handler_program = HandleHttpClient::new(conn, self.parent.clone());
                    let new_id = self.parent.register(handler_program);
                    self.parent.queue(new_id);
                },
                { unreachable!() }
            );
        }

        let c = Arc::new(Completion::AppCompletion(AppCompletion::new_accept(waker)));
        self.server_sock.accept(c.clone())?;
        self.completion = Some(c);
        Ok(())
    }
}

pub struct HandleHttpClient {
    conn: Arc<dyn ClientConnection>,
    parent: Runtime, // parent runtime
    completion: Option<SharedCompletion>,
}

impl HandleHttpClient {
    pub fn new(conn: Arc<dyn ClientConnection>, parent: Runtime) -> Box<Self> {
        Box::new(HandleHttpClient { conn, parent, completion: None })
    }
}
 
impl Program for HandleHttpClient {
    fn step(&mut self, waker: ProgramWaker) -> Result<()> {
        let mut eof = false;
        let mut text_buf = Vec::new();

        if let Some(c) = self.completion.as_ref() {
            unwrap_completion!(
                c == AppCompletion::Recv,
                |c| { 
                    if c.result() == Some(0) {
                        eof = true;
                    } else {
                        text_buf = c.buf().to_owned();
                    }
                },
                { unreachable!() }
            );
        }
        
        if !text_buf.is_empty() {
            // echo text back with no callback on completion
            let null_waker = ProgramWaker { program_id: None, run_queue: self.parent.run_queue.clone()};
            let sendc = Arc::new(Completion::AppCompletion(AppCompletion::new_send(null_waker, text_buf)));
            self.conn.send(sendc.clone())?;
        }

        if eof {
            // TODO: cleanup (close fd, remove self from programs)
            if let Some(id) = waker.id() {
                self.parent.deregister(id);
            }
            return Ok(());
        }
         
        let recvc = Arc::new(Completion::AppCompletion(AppCompletion::new_recv(waker, 64)));
        self.conn.recv(recvc.clone())?;
        self.completion = Some(recvc);
        Ok(())
    }
}
