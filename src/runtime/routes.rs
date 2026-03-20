use crate::io::generic::ClientConnection;
use crate::io::completion::{Completion, SharedCompletion, AppCompletion};
use super::{ProgramWaker, StepResult, 
    receive_body::{ReceivingBodyProgram, PartHeaders, PartDisposition, MultipartBodyProgram}
};
use std::sync::Arc;
use std::mem; 
use http::{Response, StatusCode};
use anyhow::{Result, Context, anyhow, bail};
use rustix::path::Arg;
use serde::Deserialize;
use tracing::{info, error};

pub enum RouteHandler {
    HealthCheck(HealthCheckProgram),
    HttpError(HttpErrorProgram),
    Embed(EmbedFileProgram)
}

impl RouteHandler {
    pub fn health_check(conn: Arc<dyn ClientConnection>) -> Self {
        Self::HealthCheck(HealthCheckProgram::new(conn))
    }
    pub fn http_error(conn: Arc<dyn ClientConnection>, status: StatusCode) -> Self {
        Self::HttpError(HttpErrorProgram::new(conn, status))
    }
    pub fn db_post(conn: Arc<dyn ClientConnection>, req: OwnedRequest, id: String) -> Self {
        Self::Embed(EmbedFileProgram::new(conn, req, id))
    }

    pub fn step(&mut self, waker: ProgramWaker) -> Result<StepResult<()>> {
        match self {
            Self::HealthCheck(p) => {p.step(waker)}
            Self::HttpError(p) => {p.step(waker)}
            Self::Embed(p) => {p.step(waker)}
        }
    } 
}

#[derive(Deserialize, Debug)]
struct PostOpBody {
    doc_id: String,
    text: String,
}

enum PostOpProgramState {
    ReceivingBody(MultipartBodyProgram),
    Initializing,
    Updating,
    Responding,
    RespondingError(HttpErrorProgram),
}


pub struct EmbedFileProgram {
    conn: Arc<dyn ClientConnection>,
    req: OwnedRequest,
    id: String,
    completion: Option<SharedCompletion>,
    state: PostOpProgramState
}

impl EmbedFileProgram {
    fn new(conn: Arc<dyn ClientConnection>, mut req: OwnedRequest, id: String) -> Self {
        let state = (|| {
            let Ok(ContentType::MultiPart(boundary)) = req.content_type() else {
                return PostOpProgramState::RespondingError(HttpErrorProgram::new(conn.clone(), StatusCode::BAD_REQUEST));
            };

						let part_router = |h: &PartHeaders| -> PartDisposition {
						    if h.content_type.as_deref().map_or(false, |c| c.contains("json")) {
						        PartDisposition::Buffer
						    } else if h.filename.is_some() {
						        let name = h.filename.as_deref().unwrap_or("file");
						        PartDisposition::StreamToFile(format!("/tmp/{name}").into())
						    } else {
						        PartDisposition::Discard
						    }
						};

            let chunked = req.is_chunked();
            let content_length = req
                .header("content-length")
                .and_then(|s| s.parse::<u64>().ok());

            // If both are present, reject
            if chunked && content_length.is_some() {
                return PostOpProgramState::RespondingError(HttpErrorProgram::new(conn.clone(), StatusCode::BAD_REQUEST));
            }
            
            // from this point req.buf is no longer meaningful
            let mut body_buf = mem::take(&mut req.buf); 
            
            let body_buf = body_buf.split_off(req.body_offset); 
						let program = MultipartBodyProgram::new(
						    conn.clone(), chunked, content_length, &boundary, body_buf,
						    Box::new(part_router),
						);
            PostOpProgramState::ReceivingBody(program)
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
                match state.step(waker.clone()) {
                    Ok(StepResult::Complete(())) => {
                        info!("{}", state.raw().as_str()?);
                        dbg!(state.raw().len());
                        let json = state.deserialize::<PostOpBody>()?;
                        info!("body: {:?}", json);
                        self.raise_http_error(StatusCode::OK, waker);
                        return Ok(StepResult::Pending);
                    }
                    Ok(StepResult::Pending) => {
                        return Ok(StepResult::Pending);
                    }
                    Err(error) => {
                        error!("Error occured while receiving body: {error}");
                        self.raise_http_error(StatusCode::BAD_REQUEST, waker);
                        return Ok(StepResult::Pending);
                    }
                }
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
    
    fn raise_http_error(&mut self, status: StatusCode, waker: ProgramWaker) {
        let mut program = HttpErrorProgram::new(self.conn.clone(), status);
        program.step(waker);
        self.state = PostOpProgramState::RespondingError(program);
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

pub fn get_route_handler(
    req: OwnedRequest,
    conn: Arc<dyn ClientConnection>,
) -> RouteHandler {
    let method = req.method.clone(); // or &req.method if you prefer
    let path = req.path.trim();
    let path = path.split(['?', '#']).next().unwrap_or("");
    let trimmed = path.trim_matches('/');
    let segments: Vec<&str> = trimmed.split('/').filter(|s| !s.is_empty()).collect();

    // /
    if segments.is_empty() {
        return RouteHandler::health_check(conn.clone());
    }

    // /db/<id>
    if let ["db", id] = segments.as_slice() {
        if id.is_empty() {
            return RouteHandler::http_error(conn, StatusCode::NOT_FOUND);
        }
        let id = (*id).to_owned();
        return match method.as_str() {
            "POST" => RouteHandler::db_post(conn.clone(), req, id),
            "GET"  => RouteHandler::health_check(conn.clone()),
            _      => RouteHandler::http_error(conn, StatusCode::METHOD_NOT_ALLOWED),
        };
    }

    RouteHandler::http_error(conn, StatusCode::NOT_FOUND)
}

#[derive(Debug)]
pub enum ContentType {
    Json,
    MultiPart(String) // wrapper for boundary string
}

#[derive(Debug)]
pub struct OwnedRequest {
    pub method: String,
    pub path: String,
    pub version: u8,
    pub headers: Vec<(String, Vec<u8>)>,
    pub buf: Vec<u8>,
    pub body_offset: usize
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
    
    pub fn is_chunked(&self) -> bool {
        // Accept comma-separated codings, any casing, extra whitespace.
        // e.g. "gzip, chunked"
        let Some(te) = self.header("transfer-encoding") else { return false; };

        te.split(',')
            .map(|v| v.trim())
            .any(|coding| coding.eq_ignore_ascii_case("chunked"))
    }
    
    pub fn content_type(&self) -> Result<ContentType> {
        let Some(ct) = self.header("content-type") else {
            bail!("content-type header missing")
        };

        let ct = ct.trim();

        let mut parts = ct.split(';').map(|s| s.trim()).filter(|s| !s.is_empty());
        let media_type = parts
            .next()
            .ok_or_else(|| anyhow::anyhow!("invalid content-type header: empty"))?
            .to_ascii_lowercase();

        if media_type == "application/json" {
            return Ok(ContentType::Json);
        }

        if media_type == "multipart/form-data" {
            // Find boundary parameter (case-insensitive on the key)
            for p in parts {
                let mut kv = p.splitn(2, '=').map(|s| s.trim());
                let k = kv.next().unwrap_or("").to_ascii_lowercase();
                let v = kv.next();
                if k == "boundary" {
                    let Some(v) = v else {
                        bail!("multipart/form-data missing boundary value");
                    };
                    let boundary = v.trim().trim_matches('"').to_string();
                    if boundary.is_empty() {
                        bail!("multipart/form-data boundary is empty");
                    }
                    return Ok(ContentType::MultiPart(boundary));
                }
            }
            bail!("multipart/form-data missing boundary parameter");
        }
        bail!("unsupported content-type: {}", ct)
    }

    pub fn from_buf(req_buf: Vec<u8>) -> Result<Self> {
        let mut headers = [httparse::EMPTY_HEADER; 64];
        let mut req = httparse::Request::new(&mut headers);

        let status = req
            .parse(&req_buf)
            .context("failed to parse HTTP request")?;

        let body_offset = match status {
            httparse::Status::Complete(n) => n,
            httparse::Status::Partial => return Err(anyhow!("request is partial")),
        };

        let method = req.method.context("httparse request missing method")?.to_owned();
        let path = req.path.context("httparse request missing path")?.to_owned();
        let version = req.version.context("httparse request missing version")?;

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

        Ok(Self {
            method,
            path,
            version,
            headers,
            body_offset,
            buf: req_buf,
        })
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

