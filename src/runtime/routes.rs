use crate::io::generic::ClientConnection;
use crate::io::completion::{Completion, SharedCompletion, AppCompletion};
use super::{ProgramWaker, StepResult};
use std::sync::Arc;
use http::{Response, StatusCode};
use anyhow::Result;

pub enum Route {
    HealthCheck(HealthCheckProgram),
    NotFound(NotFoundProgram),
}

impl Route {
    pub fn health_check(conn: Arc<dyn ClientConnection>) -> Self {
        Self::HealthCheck(HealthCheckProgram::new(conn))
    }
    pub fn not_found(conn: Arc<dyn ClientConnection>) -> Self {
        Self::NotFound(NotFoundProgram::new(conn))
    }

    pub fn step(&mut self, waker: ProgramWaker) -> Result<StepResult<()>> {
        match self {
            Self::HealthCheck(p) => {p.step(waker)}
            Self::NotFound(p) => {p.step(waker)}
        }
    } 
}

/// state machine (sub-future in HandleHttpClient program)
pub struct HealthCheckProgram {
    conn: Arc<dyn ClientConnection>,
    completion: Option<SharedCompletion>,
}

impl HealthCheckProgram { 
    fn new(conn: Arc<dyn ClientConnection>) -> Self {
        Self { conn, completion: None }
    }
    fn step(&mut self, waker: ProgramWaker) -> Result<StepResult<()>> {
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
            Ok(StepResult::Pending)
        } else {
            Ok(StepResult::Complete(()))
        }
    }
}

pub struct NotFoundProgram {
    conn: Arc<dyn ClientConnection>,
    completion: Option<SharedCompletion>,
}

impl NotFoundProgram { 
    fn new(conn: Arc<dyn ClientConnection>) -> Self {
        Self { conn, completion: None }
    }
    fn step(&mut self, waker: ProgramWaker) -> Result<StepResult<()>> {
        if self.completion.is_none() {
            let response = Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(())?;
            let mut buf = Vec::new();
            encode_http1_response_head(&response, &mut buf);
            let sendc = Arc::new(Completion::AppCompletion(AppCompletion::new_send(waker, buf)));
            self.conn.send(sendc.clone())?;
            self.completion = Some(sendc);
            Ok(StepResult::Pending)
        } else {
            Ok(StepResult::Complete(()))
        }
    }
}

pub fn get_route_handler(path: Option<&str>, conn: Arc<dyn ClientConnection>) -> Route {
    let Some(path) = path else {return Route::not_found(conn)};
    let path = path.trim();
    let path = path.split(['?', '#']).next().unwrap_or("");
    let trimmed = path.trim_matches('/');
    let segments: Vec<&str> = trimmed.split('/').filter(|s| !s.is_empty()).collect();

    type Matcher<'a> = dyn Fn(&[&str]) -> Option<Route> + 'a;

    let root = |segs: &[&str]| (segs.is_empty()).then(|| Route::health_check(conn.clone()));

    let db = |segs: &[&str]| match segs {
        ["db", id] if !id.is_empty() => Some(Route::health_check(conn.clone())),
        _ => None,
    };

    let matchers: [&Matcher; 2] = [&root, &db];

    for m in matchers {
        if let Some(route) = m(&segments) {
            return route;
        }
    }

    Route::not_found(conn)
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
