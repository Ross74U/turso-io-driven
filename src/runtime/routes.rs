use crate::io::generic::ClientConnection;
use crate::io::completion::{Completion, SharedCompletion, AppCompletion};
use super::{ProgramWaker, StepResult};
use std::sync::Arc;
use http::{Response, StatusCode};
use anyhow::{Result, Context, anyhow};
use httparse::Request;

pub enum RouteHandler {
    HealthCheck(HealthCheckProgram),
    HttpError(HttpErrorProgram),
    PostOp(PostOpProgram)
}

impl RouteHandler {
    pub fn health_check(conn: Arc<dyn ClientConnection>) -> Self {
        Self::HealthCheck(HealthCheckProgram::new(conn))
    }
    pub fn http_error(conn: Arc<dyn ClientConnection>, status: StatusCode) -> Self {
        Self::HttpError(HttpErrorProgram::new(conn, status))
    }
    pub fn db_post(conn: Arc<dyn ClientConnection>, req: OwnedRequest, id: String) -> Self {
        Self::PostOp(PostOpProgram::new(conn, req, id))
    }

    pub fn step(&mut self, waker: ProgramWaker) -> Result<StepResult<()>> {
        match self {
            Self::HealthCheck(p) => {p.step(waker)}
            Self::HttpError(p) => {p.step(waker)}
            Self::PostOp(p) => {p.step(waker)}
        }
    } 
}

enum PostOpProgramState {
    ReceivingBody(ReceivingBodyState),
    Initializing,
    Updating,
    Responding,
    RespondingError(HttpErrorProgram),
}

struct ReceivingBodyState {
    chunked: bool,
    received: u64,
    content_length: Option<u64>
}

pub struct PostOpProgram {
    conn: Arc<dyn ClientConnection>,
    req: OwnedRequest,
    id: String,
    completion: Option<SharedCompletion>,
    state: PostOpProgramState
}

impl PostOpProgram {
    fn new(conn: Arc<dyn ClientConnection>, req: OwnedRequest, id: String) -> Self {
        fn is_application_json(ct: &str) -> bool {
            // Accept: "application/json" plus parameters, any casing, extra whitespace.
            // e.g. "Application/JSON; charset=utf-8"
            ct.split(';')
                .next()
                .map(|v| v.trim())
                .is_some_and(|media_type| media_type.eq_ignore_ascii_case("application/json"))
        }

        fn transfer_encoding_has_chunked(te: &str) -> bool {
            // Accept comma-separated codings, any casing, extra whitespace.
            // e.g. "gzip, chunked"
            te.split(',')
                .map(|v| v.trim())
                .any(|coding| coding.eq_ignore_ascii_case("chunked"))
        }

        let state = (|| {
            let Some(content_type) = req.header("content-type") else {
                return PostOpProgramState::RespondingError(HttpErrorProgram::new(conn.clone(), StatusCode::BAD_REQUEST));
            };
            if !is_application_json(content_type) {
                return PostOpProgramState::RespondingError(HttpErrorProgram::new(conn.clone(), StatusCode::BAD_REQUEST));
            }

            let chunked = req
                .header("transfer-encoding")
                .is_some_and(transfer_encoding_has_chunked);

            let content_length = req
                .header("content-length")
                .and_then(|s| s.parse::<u64>().ok());

            // If both are present, it's ambiguous. Many servers reject this (good security posture).
            if chunked && content_length.is_some() {
                return PostOpProgramState::RespondingError(HttpErrorProgram::new(conn.clone(), StatusCode::BAD_REQUEST));
            }

            PostOpProgramState::ReceivingBody(ReceivingBodyState {
                chunked,
                content_length,
                received: 0,
            })
        })();

        Self {
            conn,
            completion: None,
            state,
            req,
            id,
        }
    }
    fn step(&mut self, waker: ProgramWaker) -> Result<StepResult<()>> {
        match &mut self.state {
            PostOpProgramState::ReceivingBody(state) => {
                todo!()
            }
            PostOpProgramState::Initializing => {
                todo!()
            }
            PostOpProgramState::Updating => {
                todo!()
            }
            PostOpProgramState::Responding => {
                todo!()
            }
            PostOpProgramState::RespondingError(state) => {
                state.step(waker)
            }
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

pub struct HttpErrorProgram {
    conn: Arc<dyn ClientConnection>,
    status: StatusCode,
    completion: Option<SharedCompletion>,
}

impl HttpErrorProgram { 
    fn new(conn: Arc<dyn ClientConnection>, status: StatusCode) -> Self {
        Self { conn, completion: None, status }
    }
    fn step(&mut self, waker: ProgramWaker) -> Result<StepResult<()>> {
        if self.completion.is_none() {
            let response = Response::builder()
                .status(self.status)
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

pub fn get_route_handler(req: Request, conn: Arc<dyn ClientConnection>) -> RouteHandler {
    let Some(path) = req.path else {return RouteHandler::http_error(conn, StatusCode::NOT_FOUND)};
    let path = path.trim();
    let path = path.split(['?', '#']).next().unwrap_or("");
    let trimmed = path.trim_matches('/');
    let segments: Vec<&str> = trimmed.split('/').filter(|s| !s.is_empty()).collect();

    type Matcher<'a> = dyn Fn(&[&str]) -> Option<RouteHandler> + 'a;

    let root = |segs: &[&str]| (segs.is_empty()).then(|| RouteHandler::health_check(conn.clone()));

    let db = |segs: &[&str]| {
        // Only match /db/<id> where <id> is non-empty.
        let id = match segs {
            ["db", id] if !id.is_empty() => *id,
            _ => return None,
        };
        
        let Ok(owned_request) = OwnedRequest::from_httparse(&req) else {
            return Some(RouteHandler::http_error(conn.clone(), StatusCode::BAD_REQUEST))
        };

        match req.method? {
            "POST" => Some(RouteHandler::db_post(conn.clone(), owned_request, id.to_owned())),
            "GET"  => Some(RouteHandler::health_check(conn.clone())),
            _      => None,
        }
    };

    let matchers: [&Matcher; 2] = [&root, &db];

    for m in matchers {
        if let Some(route) = m(&segments) {
            return route;
        }
    }

    RouteHandler::http_error(conn, StatusCode::NOT_FOUND)
}

#[derive(Debug)]
pub struct OwnedRequest {
    pub method: String,
    pub path: String,
    pub version: u8,
    pub headers: Vec<(String, Vec<u8>)>,
}

impl OwnedRequest {
    /// Case-insensitive header lookup. Returns the first match, raw bytes.
    ///
    /// Prefer this as the base primitive; higher-level helpers can decode.
    pub fn header_raw(&self, key: &str) -> Option<&[u8]> {
        self.headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
            .map(|(_, value)| value.as_slice())
    }
    /// Case-insensitive header lookup, decoded as UTF-8 and trimmed.
    ///
    /// Returns `None` if header missing or value is not valid UTF-8.
    pub fn header(&self, key: &str) -> Option<&str> {
        let raw = self.header_raw(key)?;
        let s = std::str::from_utf8(raw).ok()?;
        Some(s.trim())
    }
    /// Like `header`, but lowercases the returned value (ASCII) into an owned `String`.
    ///
    /// This is useful for token-ish header values where case-insensitivity applies
    /// (e.g. media types, `chunked`, `keep-alive`). Do **not** use this for
    /// case-sensitive values like bearer tokens.
    pub fn header_lowercase(&self, key: &str) -> Option<String> {
        let v = self.header(key)?;
        Some(v.to_ascii_lowercase())
    }

    pub fn from_httparse(req: &httparse::Request<'_, '_>) -> Result<Self> {
        let method = req
            .method
            .context("httparse request missing method")?
            .to_owned();
        let path = req
            .path
            .context("httparse request missing path")?
            .to_owned();
        let version = req
            .version
            .context("httparse request missing version")?;
        let headers = req
            .headers
            .iter()
            .map(|h| {
                if h.name.is_empty() {
                    return Err(anyhow!("encountered empty header name"));
                }
                Ok((h.name.to_owned(), h.value.to_vec()))
            })
            .collect::<Result<Vec<_>>>()
            .context("failed to copy headers")?;
        Ok(Self { method, path, version, headers, })
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

