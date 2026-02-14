use anyhow::Result;
use std::{mem, sync::Arc};
use super::{Runtime, Program, ProgramWaker, unwrap_completion, StepResult,
    routes::{get_route_handler, RouteHandler, OwnedRequest}
};
use crate::io::completion::{Completion, SharedCompletion, AppCompletion};
use crate::io::generic::{ServerSocket, ClientConnection};
use tracing::{info, error};
use httparse::{Request, Status};

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

enum ClientState {
    Receiving,
    Responding
}

pub struct HandleHttpClient {
    state: ClientState,
    conn: Arc<dyn ClientConnection>,
    parent: Runtime, // parent runtime
    req_buf: Vec<u8>,
    completion: Option<SharedCompletion>,
    resp_state_machine: Option<RouteHandler>
}

impl HandleHttpClient {
    pub fn new(conn: Arc<dyn ClientConnection>, parent: Runtime) -> Box<Self> {
        let req_buf = Vec::with_capacity(8*1024); //8KB
        Box::new(HandleHttpClient { 
            state: ClientState::Receiving,
            conn,
            parent,
            completion: None,
            resp_state_machine: None,
            req_buf,
        })
    }

    fn close_connection(&mut self, waker: ProgramWaker) {
        if let Some(id) = waker.id() {
            self.parent.deregister(id);
        }
        self.conn.close().unwrap();
    }
}
 
impl Program for HandleHttpClient {
    fn step(&mut self, waker: ProgramWaker) -> Result<()> {
        match self.state {
            ClientState::Responding => {
                let Some(resp_state) = self.resp_state_machine.as_mut() else {unreachable!()};
                match resp_state.step(waker.clone())? {
                    StepResult::Pending => {
                        info!("pending");
                    }
                    StepResult::Complete(_) => {
                        info!("complete");
                        self.close_connection(waker);
                    } // we've sent everything, close connection
                } 
            }
            ClientState::Receiving => {
                if let Some(c) = self.completion.as_ref() {
                    unwrap_completion!(
                        c == AppCompletion::Recv,
                        |c| { 
                            match c.result() {
                                Some(0) => {
                                    // handle premature EOF 
                                    // (client disconnection before sending a complete http request) 
                                    self.close_connection(waker.clone());
                                    return Ok(());
                                }
                                Some(-1) => {
                                    // socket error
                                    error!("socket error");
                                    self.close_connection(waker.clone());
                                    return Ok(());
                                }
                                Some(_) => { 
                                    self.req_buf.extend(c.buf());
                                }
                                None => { unreachable!("Spurious calls on waker") }
                            }
                        },
                        { unreachable!() }
                    );
                }

                let mut headers = [httparse::EMPTY_HEADER; 64];
                let mut req = Request::new(&mut headers);
                match req.parse(&self.req_buf) {
                    Ok(Status::Partial) => { 
                        let recvc = Arc::new(Completion::AppCompletion(AppCompletion::new_recv(waker.clone(), 64)));
                        self.conn.recv(recvc.clone())?;
                        self.completion = Some(recvc);
                    }
                    Ok(Status::Complete(_)) => { 
                        let req_buf = mem::take(&mut self.req_buf);
                        let owned_req = OwnedRequest::from_buf(req_buf)?;
                        let mut resp_program = get_route_handler(owned_req, self.conn.clone());
                        resp_program.step(waker.clone())?; // step once to initiate (lazy)
                        self.resp_state_machine = Some(resp_program);
                        self.state = ClientState::Responding;
                    }
                    Err(_error) => { todo!("handle malformed http request") }
                } 
            }
        }

        Ok(())
    }
}

