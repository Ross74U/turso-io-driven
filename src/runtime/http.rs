use anyhow::Result;
use std::sync::Arc;
use super::{Runtime, Program, ProgramWaker, unwrap_completion, routes};
use crate::io::completion::{Completion, SharedCompletion, AppCompletion};
use crate::io::generic::{ServerSocket, ClientConnection};
use tracing::{info};
use httparse::{Request, Status};
use http::{Response, StatusCode};

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
    resp_state_machine: Option<Routes>
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
                    State::Pending => {}
                    State::Complete(_) => {self.close_connection(waker);} // we've sent everything, close
                    // connection
                } 
            }
            ClientState::Receiving => {
                if let Some(c) = self.completion.as_ref() {
                    unwrap_completion!(
                        c == AppCompletion::Recv,
                        |c| { 
                            if c.result() == Some(0) {
                                // handle premature EOF 
                                // (client disconnection before sending a complete http request) 
                                self.close_connection(waker.clone());
                                return Ok(());
                            } else {
                                self.req_buf.extend(c.buf());
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
                    Ok(Status::Complete(_body_offset)) => { 
                        let mut resp_program = match req.path {
                            Some("/") => Routes::health_check(self.conn.clone()),
                            _ => Routes::not_found(self.conn.clone()),
                        };
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

enum Routes {
    HealthCheck(HealthCheckProgram),
    NotFound(NotFoundProgram),
}

impl Routes {
    fn health_check(conn: Arc<dyn ClientConnection>) -> Self {
        Self::HealthCheck(HealthCheckProgram::new(conn))
    }
    fn not_found(conn: Arc<dyn ClientConnection>) -> Self {
        Self::NotFound(NotFoundProgram::new(conn))
    }

    fn step(&mut self, waker: ProgramWaker) -> Result<State<()>> {
        match self {
            Self::HealthCheck(p) => {p.step(waker)}
            Self::NotFound(p) => {p.step(waker)}
        }
    } 
}

enum State<T> {
    Complete(T),
    Pending
}

/// state machine (sub-future in HandleHttpClient program)
struct HealthCheckProgram {
    conn: Arc<dyn ClientConnection>,
    completion: Option<SharedCompletion>,
}

impl HealthCheckProgram { 
    fn new(conn: Arc<dyn ClientConnection>) -> Self {
        Self { conn, completion: None }
    }
    fn step(&mut self, waker: ProgramWaker) -> Result<State<()>> {
        if self.completion.is_none() {
            let body = b"running";
            let response = Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "text/plain")
                .header("content-length", format!("{}", body.len()))
                .body(())?;
            let mut buf = Vec::new();
            encode_http1_response_head(&response, &mut buf);
            buf.extend_from_slice(body);
            let sendc = Arc::new(Completion::AppCompletion(AppCompletion::new_send(waker, buf)));
            self.conn.send(sendc.clone())?;
            self.completion = Some(sendc);
            Ok(State::Pending)
        } else {
            Ok(State::Complete(()))
        }
    }
}

struct NotFoundProgram {
    conn: Arc<dyn ClientConnection>,
    completion: Option<SharedCompletion>,
}

impl NotFoundProgram { 
    fn new(conn: Arc<dyn ClientConnection>) -> Self {
        Self { conn, completion: None }
    }
    fn step(&mut self, waker: ProgramWaker) -> Result<State<()>> {
        if self.completion.is_none() {
            let response = Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(())?;
            let mut buf = Vec::new();
            encode_http1_response_head(&response, &mut buf);
            let sendc = Arc::new(Completion::AppCompletion(AppCompletion::new_send(waker, buf)));
            self.conn.send(sendc.clone())?;
            self.completion = Some(sendc);
            Ok(State::Pending)
        } else {
            Ok(State::Complete(()))
        }
    }
}

pub fn encode_http1_response_head<B>(resp: &Response<B>, out: &mut Vec<u8>) {
    let code = resp.status().as_u16();
    let reason = resp.status().canonical_reason().unwrap_or("");
    out.extend_from_slice(b"HTTP/1.1 ");
    out.extend_from_slice(code.to_string().as_bytes());
    out.extend_from_slice(b" ");
    out.extend_from_slice(reason.as_bytes());
    out.extend_from_slice(b"\r\n");

    for (name, value) in resp.headers().iter() {
        out.extend_from_slice(name.as_str().as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(value.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"\r\n");
}
