use anyhow::{anyhow, Result};
use crate::unwrap_completion;
use crate::io::generic::ClientConnection;
use crate::io::completion::{Completion, SharedCompletion, AppCompletion};
use super::{ProgramWaker, StepResult};
use std::sync::Arc;
use std::path::{Path, PathBuf};
use std::io::Write;
use serde::de::DeserializeOwned;

// ===========================================================================
//  ReceivingBodyProgram  (unchanged)
// ===========================================================================

pub struct ReceivingBodyProgram {
    buf: Vec<u8>,
    chunked: bool,
    received: u64,
    content_length: Option<u64>,
    conn: Arc<dyn ClientConnection>,
    completion: Option<SharedCompletion>,
    in_buf: Vec<u8>,
    chunk_dec: Option<ChunkedDecoder>,
}

impl ReceivingBodyProgram {
    pub fn new(
        conn: Arc<dyn ClientConnection>,
        chunked: bool,
        content_length: Option<u64>,
        buf: Vec<u8>,
    ) -> Self {
        let chunk_dec = if chunked {
            Some(ChunkedDecoder::new(1 * 1024 * 1024))
        } else {
            None
        };
        let received = buf.len() as u64;

        ReceivingBodyProgram {
            buf,
            in_buf: Vec::new(),
            chunked,
            content_length,
            received,
            conn,
            completion: None,
            chunk_dec,
        }
    }

    pub fn deserialize<T: DeserializeOwned>(&self) -> Result<T> {
        if let Some(content_length) = self.content_length {
            let mut body = self.buf.clone();
            body.truncate(content_length as _);
            Ok(serde_json::from_slice::<T>(&body)?)
        } else {
            unimplemented!()
        }
    }

    pub fn raw(&self) -> &[u8] {
        &self.buf
    }

    pub fn step(&mut self, waker: ProgramWaker) -> Result<StepResult<()>> {
        match (self.chunked, self.content_length) {
            (true, None) => self.step_chunked(waker),
            (false, Some(_)) => self.step_content_length(waker),
            _ => unreachable!(),
        }
    }

    fn step_chunked(&mut self, waker: ProgramWaker) -> Result<StepResult<()>> {
        if let Some(c) = self.completion.as_ref() {
            unwrap_completion!(
                c == AppCompletion::Recv,
                |c| {
                    match c.result() {
                        Some(0) => {
                            return Err(anyhow!("premature client disconnection"));
                        }
                        Some(-1) => {
                            return Err(anyhow!("socket error occured while recv body"));
                        }
                        Some(n) => {
                            self.received += n as u64;
                            self.in_buf.extend_from_slice(c.buf());
                        }
                        None => {
                            unreachable!("spurious wakeup")
                        }
                    }
                },
                { unreachable!() }
            );
            self.completion = None;
        }

        let dec = self
            .chunk_dec
            .as_mut()
            .ok_or_else(|| anyhow!("step_chunked called but chunk decoder missing"))?;

        loop {
            match dec.advance(&mut self.in_buf, &mut self.buf)? {
                Advance::Progress => continue,
                Advance::Done => return Ok(StepResult::Complete(())),
                Advance::NeedMore => break,
            }
        }

        const READ_SIZE: usize = 16 * 1024;

        let recvc = Arc::new(Completion::AppCompletion(AppCompletion::new_recv(
            waker.clone(),
            READ_SIZE,
        )));
        self.conn.recv(recvc.clone())?;
        self.completion = Some(recvc);

        Ok(StepResult::Pending)
    }

    fn step_content_length(&mut self, waker: ProgramWaker) -> Result<StepResult<()>> {
        if let Some(c) = self.completion.as_ref() {
            unwrap_completion!(
                c == AppCompletion::Recv,
                |c| {
                    if let Some(read) = c.result() {
                        if read == 0 {
                            return Err(anyhow!("premature client disconnection"));
                        }
                        self.received += read as u64;
                        self.buf.extend_from_slice(c.buf());
                    }
                },
                { unreachable!() }
            );
            self.completion = None;
        }

        let remaining = self.content_length.unwrap().saturating_sub(self.received);
        if remaining == 0 {
            return Ok(StepResult::Complete(()));
        }

        let recvc = Arc::new(Completion::AppCompletion(AppCompletion::new_recv(
            waker.clone(),
            remaining as usize,
        )));
        self.conn.recv(recvc.clone())?;
        self.completion = Some(recvc);
        Ok(StepResult::Pending)
    }
}

// ===========================================================================
//  Chunked transfer-encoding decoder  (unchanged)
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Advance {
    NeedMore,
    Progress,
    Done,
}

#[derive(Debug)]
enum ChunkState {
    ReadSizeLine,
    ReadData { remaining: usize },
    ReadDataCrlf,
    ReadTrailers,
    Done,
}

#[derive(Debug)]
pub struct ChunkedDecoder {
    state: ChunkState,
    decoded_total: usize,
    max_decoded: usize,
    max_size_line: usize,
    max_trailer_bytes: usize,
    trailer_bytes: usize,
}

impl ChunkedDecoder {
    pub fn new(max_decoded: usize) -> Self {
        Self {
            state: ChunkState::ReadSizeLine,
            decoded_total: 0,
            max_decoded,
            max_size_line: 8 * 1024,
            max_trailer_bytes: 32 * 1024,
            trailer_bytes: 0,
        }
    }

    pub fn is_done(&self) -> bool {
        matches!(self.state, ChunkState::Done)
    }

    pub fn advance(&mut self, in_buf: &mut Vec<u8>, out: &mut Vec<u8>) -> Result<Advance> {
        if self.is_done() {
            return Ok(Advance::Done);
        }

        let mut made_progress = false;

        loop {
            match self.state {
                ChunkState::ReadSizeLine => {
                    let Some(crlf_at) = find_crlf(in_buf) else {
                        return Ok(if made_progress {
                            Advance::Progress
                        } else {
                            Advance::NeedMore
                        });
                    };
                    if crlf_at > self.max_size_line {
                        return Err(anyhow!("chunk size line too long"));
                    }

                    let line = drain_line(in_buf, crlf_at);
                    let size = parse_chunk_size(&line)?;

                    if size == 0 {
                        self.state = ChunkState::ReadTrailers;
                    } else {
                        self.state = ChunkState::ReadData { remaining: size };
                    }
                    made_progress = true;
                }

                ChunkState::ReadData { remaining } => {
                    if in_buf.len() < remaining {
                        return Ok(if made_progress {
                            Advance::Progress
                        } else {
                            Advance::NeedMore
                        });
                    }

                    if self.decoded_total.saturating_add(remaining) > self.max_decoded {
                        return Err(anyhow!(
                            "decoded body exceeds limit ({} bytes)",
                            self.max_decoded
                        ));
                    }

                    out.extend_from_slice(&in_buf[..remaining]);
                    in_buf.drain(..remaining);

                    self.decoded_total += remaining;
                    self.state = ChunkState::ReadDataCrlf;
                    made_progress = true;
                }

                ChunkState::ReadDataCrlf => {
                    if in_buf.len() < 2 {
                        return Ok(if made_progress {
                            Advance::Progress
                        } else {
                            Advance::NeedMore
                        });
                    }
                    if &in_buf[..2] != b"\r\n" {
                        return Err(anyhow!("missing CRLF after chunk data"));
                    }
                    in_buf.drain(..2);
                    self.state = ChunkState::ReadSizeLine;
                    made_progress = true;
                }

                ChunkState::ReadTrailers => {
                    let Some(crlf_at) = find_crlf(in_buf) else {
                        return Ok(if made_progress {
                            Advance::Progress
                        } else {
                            Advance::NeedMore
                        });
                    };

                    self.trailer_bytes += crlf_at + 2;
                    if self.trailer_bytes > self.max_trailer_bytes {
                        return Err(anyhow!("trailers too large"));
                    }

                    let line = drain_line(in_buf, crlf_at);
                    if line.is_empty() {
                        self.state = ChunkState::Done;
                        return Ok(Advance::Done);
                    }

                    made_progress = true;
                }

                ChunkState::Done => return Ok(Advance::Done),
            }
        }
    }
}

fn find_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\r\n")
}

fn drain_line(buf: &mut Vec<u8>, crlf_at: usize) -> Vec<u8> {
    let line = buf[..crlf_at].to_vec();
    buf.drain(..crlf_at + 2);
    line
}

fn parse_chunk_size(line: &[u8]) -> Result<usize> {
    let size_part = line.split(|&b| b == b';').next().unwrap_or(&[]);
    let size_part = trim_ows(size_part);

    if size_part.is_empty() {
        return Err(anyhow!("invalid chunk size line: empty"));
    }

    let mut n: usize = 0;
    for &b in size_part {
        let v = match b {
            b'0'..=b'9' => (b - b'0') as usize,
            b'a'..=b'f' => (b - b'a' + 10) as usize,
            b'A'..=b'F' => (b - b'A' + 10) as usize,
            _ => return Err(anyhow!("invalid hex digit in chunk size")),
        };
        n = n
            .checked_mul(16)
            .ok_or_else(|| anyhow!("chunk size overflow"))?;
        n = n
            .checked_add(v)
            .ok_or_else(|| anyhow!("chunk size overflow"))?;
    }
    Ok(n)
}

fn trim_ows(mut s: &[u8]) -> &[u8] {
    while matches!(s.first(), Some(b' ' | b'\t')) {
        s = &s[1..];
    }
    while matches!(s.last(), Some(b' ' | b'\t')) {
        s = &s[..s.len() - 1];
    }
    s
}

// ===========================================================================
//  Multipart support
// ===========================================================================

// ---- public types --------------------------------------------------------

/// Parsed MIME headers for a single multipart part.
#[derive(Debug, Clone, Default)]
pub struct PartHeaders {
    /// The `name` parameter from `Content-Disposition`.
    pub name: Option<String>,
    /// The `filename` parameter from `Content-Disposition`, if present.
    pub filename: Option<String>,
    /// The `Content-Type` of this part (e.g. `application/json`).
    pub content_type: Option<String>,
}

/// How a completed part's body was stored.
#[derive(Debug)]
pub enum PartContent {
    /// Entire body buffered in memory – suitable for JSON and small payloads.
    Buffered(Vec<u8>),
    /// Body was incrementally written to a file on disk.
    StreamedToFile { path: PathBuf, bytes_written: u64 },
    /// Body was discarded without storing.
    Discarded,
}

/// A fully received multipart part.
#[derive(Debug)]
pub struct MultipartPart {
    pub headers: PartHeaders,
    pub content: PartContent,
}

impl MultipartPart {
    /// Deserialize a buffered part's body from JSON.
    ///
    /// Returns an error if the part was streamed to a file or discarded.
    pub fn json<T: DeserializeOwned>(&self) -> Result<T> {
        match &self.content {
            PartContent::Buffered(data) => {
                Ok(serde_json::from_slice(data)?)
            }
            PartContent::StreamedToFile { path, .. } => {
                Err(anyhow!(
                    "part was streamed to {}; read from disk instead",
                    path.display()
                ))
            }
            PartContent::Discarded => Err(anyhow!("part body was discarded")),
        }
    }

    /// Access the raw buffered bytes, if buffered.
    pub fn bytes(&self) -> Option<&[u8]> {
        match &self.content {
            PartContent::Buffered(d) => Some(d),
            _ => None,
        }
    }

    /// If the part was streamed to a file, returns the path.
    pub fn file_path(&self) -> Option<&Path> {
        match &self.content {
            PartContent::StreamedToFile { path, .. } => Some(path),
            _ => None,
        }
    }

    /// Number of bytes written to disk (0 if not file-streamed).
    pub fn file_bytes_written(&self) -> u64 {
        match &self.content {
            PartContent::StreamedToFile { bytes_written, .. } => *bytes_written,
            _ => 0,
        }
    }

    pub fn is_json(&self) -> bool {
        self.content_type_contains("application/json")
    }

    pub fn is_pdf(&self) -> bool {
        self.content_type_contains("application/pdf")
    }

    pub fn is_docx(&self) -> bool {
        self.content_type_contains(
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        )
    }

    fn content_type_contains(&self, needle: &str) -> bool {
        self.headers
            .content_type
            .as_deref()
            .map_or(false, |ct| ct.to_lowercase().contains(needle))
    }
}

/// Controls how each part's body is consumed.
#[derive(Debug, Clone)]
pub enum PartDisposition {
    /// Buffer the full body in memory (good for JSON, small payloads).
    Buffer,
    /// Stream the body incrementally to a file at the given path.
    /// The file is created when the part headers are parsed and written to
    /// as body chunks arrive – the full part is never held in memory.
    StreamToFile(PathBuf),
    /// Discard the body (skip over it without storing).
    Discard,
}

/// Decides how each part should be handled based on its headers.
///
/// Implement this yourself, or use a closure:
///
/// ```rust,ignore
/// let router = |h: &PartHeaders| -> PartDisposition {
///     match h.content_type.as_deref() {
///         Some(ct) if ct.contains("application/json") => PartDisposition::Buffer,
///         Some(ct) if ct.contains("application/pdf") => {
///             let name = h.filename.as_deref().unwrap_or("upload.pdf");
///             PartDisposition::StreamToFile(PathBuf::from(format!("/tmp/{name}")))
///         }
///         Some(ct) if ct.contains("officedocument.wordprocessingml") => {
///             let name = h.filename.as_deref().unwrap_or("upload.docx");
///             PartDisposition::StreamToFile(PathBuf::from(format!("/tmp/{name}")))
///         }
///         _ => PartDisposition::Discard,
///     }
/// };
/// ```
pub trait PartRouter: Send {
    fn route(&mut self, headers: &PartHeaders) -> PartDisposition;
}

impl<F> PartRouter for F
where
    F: FnMut(&PartHeaders) -> PartDisposition + Send,
{
    fn route(&mut self, headers: &PartHeaders) -> PartDisposition {
        (self)(headers)
    }
}

// ---- extract_boundary helper ---------------------------------------------

/// Extract the `boundary` parameter from a `Content-Type` header value.
///
/// ```text
/// multipart/form-data; boundary=----WebKitFormBoundary7MA4YWxk
/// ```
pub fn extract_boundary(content_type: &str) -> Result<String> {
    let lower = content_type.to_lowercase();
    if !lower.contains("multipart/") {
        return Err(anyhow!(
            "not a multipart Content-Type: {:?}",
            content_type
        ));
    }

    let value = content_type
        .split(';')
        .map(str::trim)
        .find_map(|seg| {
            let l = seg.to_lowercase();
            if l.starts_with("boundary=") {
                Some(&seg["boundary=".len()..])
            } else {
                None
            }
        })
        .ok_or_else(|| anyhow!("missing boundary= in Content-Type"))?
        .trim();

    // Strip optional surrounding quotes.
    let value = if value.starts_with('"') && value.ends_with('"') && value.len() >= 2 {
        &value[1..value.len() - 1]
    } else {
        value
    };

    if value.is_empty() {
        return Err(anyhow!("empty boundary"));
    }

    Ok(value.to_string())
}

// ---- internal parser state -----------------------------------------------

#[derive(Debug, PartialEq, Eq)]
enum MultipartPhase {
    /// Before the first boundary; any preamble data is discarded.
    Preamble,
    /// Reading the MIME headers of the current part.
    Headers,
    /// Reading the body of the current part.
    Body,
    /// Terminal boundary seen – all parts received.
    Done,
}

// ---- MultipartBodyProgram ------------------------------------------------

/// Incrementally receives and parses a `multipart/form-data` body.
///
/// Like [`ReceivingBodyProgram`] this is driven by calling [`step`](Self::step)
/// in a loop until it returns [`StepResult::Complete`].  Large file parts
/// (PDF, DOCX, …) are streamed to disk as data arrives rather than buffered
/// in memory.
///
/// # Example
///
/// ```rust,ignore
/// let boundary = extract_boundary(&content_type_header)?;
///
/// let router = |h: &PartHeaders| -> PartDisposition {
///     if h.content_type.as_deref().map_or(false, |c| c.contains("json")) {
///         PartDisposition::Buffer
///     } else if h.filename.is_some() {
///         let name = h.filename.as_deref().unwrap_or("file");
///         PartDisposition::StreamToFile(format!("/tmp/{name}").into())
///     } else {
///         PartDisposition::Discard
///     }
/// };
///
/// let mut program = MultipartBodyProgram::new(
///     conn, chunked, content_length, &boundary, initial_buf,
///     Box::new(router),
/// );
///
/// loop {
///     match program.step(waker.clone())? {
///         StepResult::Complete(()) => break,
///         StepResult::Pending => { /* wait for waker */ }
///     }
/// }
///
/// for part in program.parts() {
///     if part.is_json() {
///         let payload: MyRequest = part.json()?;
///     } else if part.is_pdf() || part.is_docx() {
///         let path = part.file_path().unwrap();
///         println!("saved {} bytes to {}", part.file_bytes_written(), path.display());
///     }
/// }
/// ```
pub struct MultipartBodyProgram {
    // --- network plumbing (same pattern as ReceivingBodyProgram) -----------
    conn: Arc<dyn ClientConnection>,
    chunked: bool,
    content_length: Option<u64>,
    received: u64,
    completion: Option<SharedCompletion>,
    /// Raw (chunk-encoded) bytes not yet decoded.
    raw_buf: Vec<u8>,
    chunk_dec: Option<ChunkedDecoder>,
    network_done: bool,

    // --- incremental parser -----------------------------------------------
    /// The boundary delimiter we scan for: `\r\n--<boundary>`.
    delimiter: Vec<u8>,
    /// Decoded body bytes that haven't been consumed by the parser yet.
    parse_buf: Vec<u8>,
    phase: MultipartPhase,

    // --- state for the part currently being received -----------------------
    cur_headers: PartHeaders,
    cur_disposition: PartDisposition,
    cur_buf: Vec<u8>,
    cur_file: Option<std::fs::File>,
    cur_file_path: Option<PathBuf>,
    cur_file_bytes: u64,

    // --- completed parts --------------------------------------------------
    parts: Vec<MultipartPart>,

    // --- configuration ----------------------------------------------------
    router: Box<dyn PartRouter>,
    max_buffered_part: usize,
    max_total_body: u64,
}

impl MultipartBodyProgram {
    /// Create a new multipart body receiver.
    ///
    /// * `boundary` – the raw boundary string extracted from the `Content-Type`
    ///   header (see [`extract_boundary`]).
    /// * `initial_buf` – any body bytes already read from the socket during
    ///   header parsing.
    /// * `router` – decides per-part whether to buffer, stream to file, or
    ///   discard (see [`PartRouter`]).
    pub fn new(
        conn: Arc<dyn ClientConnection>,
        chunked: bool,
        content_length: Option<u64>,
        boundary: &str,
        initial_buf: Vec<u8>,
        router: Box<dyn PartRouter>,
    ) -> Self {
        let chunk_dec = if chunked {
            Some(ChunkedDecoder::new(64 * 1024 * 1024))
        } else {
            None
        };

        // Build the delimiter: \r\n--<boundary>
        // We also prepend \r\n to the parse buffer so that the *first* boundary
        // (which normally appears as `--<boundary>\r\n` at offset 0) can be
        // found with the same delimiter string.
        let mut delimiter = Vec::with_capacity(boundary.len() + 4);
        delimiter.extend_from_slice(b"\r\n--");
        delimiter.extend_from_slice(boundary.as_bytes());

        let mut parse_buf = Vec::with_capacity(initial_buf.len() + 2);
        parse_buf.extend_from_slice(b"\r\n");
        parse_buf.extend_from_slice(&initial_buf);

        let received = initial_buf.len() as u64;

        MultipartBodyProgram {
            conn,
            chunked,
            content_length,
            received,
            completion: None,
            raw_buf: Vec::new(),
            chunk_dec,
            network_done: false,

            delimiter,
            parse_buf,
            phase: MultipartPhase::Preamble,

            cur_headers: PartHeaders::default(),
            cur_disposition: PartDisposition::Discard,
            cur_buf: Vec::new(),
            cur_file: None,
            cur_file_path: None,
            cur_file_bytes: 0,

            parts: Vec::new(),
            router,
            max_buffered_part: 2 * 1024 * 1024,
            max_total_body: 128 * 1024 * 1024,
        }
    }

    /// Override the maximum size for a single in-memory-buffered part
    /// (default 2 MiB).
    pub fn set_max_buffered_part(&mut self, bytes: usize) {
        self.max_buffered_part = bytes;
    }

    /// Override the maximum total body size across all parts (default 128 MiB).
    pub fn set_max_total_body(&mut self, bytes: u64) {
        self.max_total_body = bytes;
    }

    /// Borrow the completed parts received so far.
    pub fn parts(&self) -> &[MultipartPart] {
        &self.parts
    }

    /// Consume the program and return all completed parts.
    pub fn into_parts(self) -> Vec<MultipartPart> {
        self.parts
    }

    /// Take all completed parts, leaving the internal vec empty.
    pub fn take_parts(&mut self) -> Vec<MultipartPart> {
        std::mem::take(&mut self.parts)
    }

    /// Find the first completed part whose `name` matches.
    pub fn part_by_name(&self, name: &str) -> Option<&MultipartPart> {
        self.parts
            .iter()
            .find(|p| p.headers.name.as_deref() == Some(name))
    }

    // ---- step entry point ------------------------------------------------

    pub fn step(&mut self, waker: ProgramWaker) -> Result<StepResult<()>> {
        // 1. Collect any completed recv.
        self.collect_recv()?;

        // 2. Check whether we've received all expected bytes.
        self.check_network_done();

        // 3. Decode chunked framing into parse_buf if applicable.
        self.decode_chunks()?;

        // 4. Drive the parser as far as possible with available data.
        self.parse()?;

        if self.phase == MultipartPhase::Done {
            return Ok(StepResult::Complete(()));
        }

        if self.network_done {
            return Err(anyhow!(
                "multipart body ended before the terminal boundary was found"
            ));
        }

        // 5. Issue the next recv.
        self.issue_recv(waker)?;
        Ok(StepResult::Pending)
    }

    // ---- network I/O -----------------------------------------------------

    fn collect_recv(&mut self) -> Result<()> {
        let Some(c) = self.completion.as_ref() else {
            return Ok(());
        };
        unwrap_completion!(
            c == AppCompletion::Recv,
            |c| {
                match c.result() {
                    Some(0) => {
                        self.network_done = true;
                    }
                    Some(-1) => {
                        return Err(anyhow!("socket error during multipart recv"));
                    }
                    Some(n) => {
                        self.received += n as u64;
                        if self.received > self.max_total_body {
                            return Err(anyhow!(
                                "multipart body exceeds {} byte limit",
                                self.max_total_body
                            ));
                        }
                        if self.chunked {
                            self.raw_buf.extend_from_slice(c.buf());
                        } else {
                            self.parse_buf.extend_from_slice(c.buf());
                        }
                    }
                    None => unreachable!("spurious wakeup"),
                }
            },
            { unreachable!() }
        );
        self.completion = None;
        Ok(())
    }

    fn check_network_done(&mut self) {
        if self.network_done {
            return;
        }
        if !self.chunked {
            if let Some(cl) = self.content_length {
                if self.received >= cl {
                    self.network_done = true;
                }
            }
        }
    }

    fn decode_chunks(&mut self) -> Result<()> {
        if let Some(dec) = self.chunk_dec.as_mut() {
            loop {
                match dec.advance(&mut self.raw_buf, &mut self.parse_buf)? {
                    Advance::Progress => continue,
                    Advance::Done => {
                        self.network_done = true;
                        break;
                    }
                    Advance::NeedMore => break,
                }
            }
        }
        Ok(())
    }

    fn issue_recv(&mut self, waker: ProgramWaker) -> Result<()> {
        const DEFAULT_READ: usize = 32 * 1024;

        let read_size = if !self.chunked {
            if let Some(cl) = self.content_length {
                let remaining = cl.saturating_sub(self.received) as usize;
                remaining.min(DEFAULT_READ)
            } else {
                DEFAULT_READ
            }
        } else {
            DEFAULT_READ
        };

        let c = Arc::new(Completion::AppCompletion(AppCompletion::new_recv(
            waker,
            read_size,
        )));
        self.conn.recv(c.clone())?;
        self.completion = Some(c);
        Ok(())
    }

    // ---- multipart parser ------------------------------------------------

    fn parse(&mut self) -> Result<()> {
        loop {
            match self.phase {
                MultipartPhase::Preamble => {
                    if !self.parse_preamble()? {
                        return Ok(());
                    }
                }
                MultipartPhase::Headers => {
                    if !self.parse_headers()? {
                        return Ok(());
                    }
                }
                MultipartPhase::Body => {
                    if !self.parse_body()? {
                        return Ok(());
                    }
                }
                MultipartPhase::Done => return Ok(()),
            }
        }
    }

    /// Scan past any preamble and consume the first boundary line.
    /// Returns `true` when it transitions to the next phase.
    fn parse_preamble(&mut self) -> Result<bool> {
        let Some(pos) = find_subsequence(&self.parse_buf, &self.delimiter) else {
            // Discard everything except what might be a partial delimiter match.
            let keep = self.delimiter.len().saturating_sub(1);
            let drain = self.parse_buf.len().saturating_sub(keep);
            if drain > 0 {
                self.parse_buf.drain(..drain);
            }
            return Ok(false);
        };

        let after = pos + self.delimiter.len();

        // We need at least 2 bytes after the delimiter to know if it is
        // `\r\n` (next part) or `--` (terminal, empty multipart).
        if self.parse_buf.len() < after + 2 {
            return Ok(false);
        }

        if &self.parse_buf[after..after + 2] == b"--" {
            self.parse_buf.drain(..after + 2);
            // Consume optional trailing CRLF.
            if self.parse_buf.starts_with(b"\r\n") {
                self.parse_buf.drain(..2);
            }
            self.phase = MultipartPhase::Done;
        } else if &self.parse_buf[after..after + 2] == b"\r\n" {
            self.parse_buf.drain(..after + 2);
            self.phase = MultipartPhase::Headers;
        } else {
            return Err(anyhow!("malformed multipart: unexpected bytes after boundary"));
        }

        Ok(true)
    }

    /// Parse the MIME headers of the current part (terminated by `\r\n\r\n`).
    fn parse_headers(&mut self) -> Result<bool> {
        let Some(end) = find_subsequence(&self.parse_buf, b"\r\n\r\n") else {
            if self.parse_buf.len() > 64 * 1024 {
                return Err(anyhow!("part headers exceed 64 KiB"));
            }
            return Ok(false);
        };

        let header_bytes = self.parse_buf[..end].to_vec();
        self.parse_buf.drain(..end + 4);

        let headers = parse_part_headers(&header_bytes)?;
        let disposition = self.router.route(&headers);

        // Reset current-part state.
        self.cur_headers = headers;
        self.cur_buf.clear();
        self.cur_file = None;
        self.cur_file_path = None;
        self.cur_file_bytes = 0;

        if let PartDisposition::StreamToFile(ref path) = disposition {
            // Create (or truncate) the destination file immediately.
            let f = std::fs::File::create(path)
                .map_err(|e| anyhow!("create {}: {}", path.display(), e))?;
            self.cur_file = Some(f);
            self.cur_file_path = Some(path.clone());
        }

        self.cur_disposition = disposition;
        self.phase = MultipartPhase::Body;
        Ok(true)
    }

    /// Incrementally consume body bytes until the next boundary is found.
    ///
    /// For file-streamed parts, data is flushed to disk as soon as it is
    /// confirmed to not be part of a boundary delimiter (we hold back
    /// `delimiter.len() - 1` bytes at the tail of the buffer to handle
    /// boundaries that straddle recv boundaries).
    fn parse_body(&mut self) -> Result<bool> {
        if let Some(pos) = find_subsequence(&self.parse_buf, &self.delimiter) {
            let after = pos + self.delimiter.len();

            // Need 2 bytes after the delimiter to distinguish `\r\n` from `--`.
            if self.parse_buf.len() < after + 2 {
                return Ok(false);
            }

            // Everything before `pos` is body data for this part.
            if pos > 0 {
                let data = self.parse_buf[..pos].to_vec();
                self.emit_body_bytes(&data)?;
            }

            self.finalize_current_part()?;

            if &self.parse_buf[after..after + 2] == b"--" {
                self.parse_buf.drain(..after + 2);
                if self.parse_buf.starts_with(b"\r\n") {
                    self.parse_buf.drain(..2);
                }
                self.phase = MultipartPhase::Done;
            } else if &self.parse_buf[after..after + 2] == b"\r\n" {
                self.parse_buf.drain(..after + 2);
                self.phase = MultipartPhase::Headers;
            } else {
                return Err(anyhow!("malformed multipart: unexpected bytes after boundary"));
            }

            Ok(true)
        } else {
            // No boundary yet – flush safe bytes and hold back enough to
            // catch a boundary that straddles two recv calls.
            let hold_back = self.delimiter.len() - 1;
            let safe = self.parse_buf.len().saturating_sub(hold_back);
            if safe > 0 {
                let data = self.parse_buf[..safe].to_vec();
                self.emit_body_bytes(&data)?;
                self.parse_buf.drain(..safe);
            }
            Ok(false)
        }
    }

    // ---- body byte dispatch ----------------------------------------------

    fn emit_body_bytes(&mut self, data: &[u8]) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        match &self.cur_disposition {
            PartDisposition::Buffer => {
                if self.cur_buf.len() + data.len() > self.max_buffered_part {
                    return Err(anyhow!(
                        "buffered part {:?} exceeds {} byte limit",
                        self.cur_headers.name,
                        self.max_buffered_part
                    ));
                }
                self.cur_buf.extend_from_slice(data);
            }
            PartDisposition::StreamToFile(_) => {
                if let Some(f) = self.cur_file.as_mut() {
                    f.write_all(data)
                        .map_err(|e| anyhow!("write to part file: {e}"))?;
                    self.cur_file_bytes += data.len() as u64;
                }
            }
            PartDisposition::Discard => { /* drop on the floor */ }
        }
        Ok(())
    }

    fn finalize_current_part(&mut self) -> Result<()> {
        let disposition =
            std::mem::replace(&mut self.cur_disposition, PartDisposition::Discard);

        let content = match disposition {
            PartDisposition::Buffer => {
                PartContent::Buffered(std::mem::take(&mut self.cur_buf))
            }
            PartDisposition::StreamToFile(path) => {
                if let Some(mut f) = self.cur_file.take() {
                    f.flush()
                        .map_err(|e| anyhow!("flush {}: {e}", path.display()))?;
                }
                PartContent::StreamedToFile {
                    path,
                    bytes_written: self.cur_file_bytes,
                }
            }
            PartDisposition::Discard => PartContent::Discarded,
        };

        self.parts.push(MultipartPart {
            headers: std::mem::take(&mut self.cur_headers),
            content,
        });

        self.cur_file_path = None;
        self.cur_file_bytes = 0;
        Ok(())
    }
}

// ---- multipart helper functions ------------------------------------------

/// Find the first occurrence of `needle` in `haystack`.
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Parse the MIME headers of a single part into [`PartHeaders`].
fn parse_part_headers(raw: &[u8]) -> Result<PartHeaders> {
    let text = std::str::from_utf8(raw)
        .map_err(|_| anyhow!("part headers are not valid UTF-8"))?;

    let mut headers = PartHeaders::default();

    for line in text.split("\r\n") {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim().to_lowercase();
        let value = value.trim();

        match key.as_str() {
            "content-type" => {
                headers.content_type = Some(value.to_string());
            }
            "content-disposition" => {
                if let Some(name) = extract_disposition_param(value, "name") {
                    headers.name = Some(name);
                }
                if let Some(filename) = extract_disposition_param(value, "filename") {
                    headers.filename = Some(filename);
                }
            }
            _ => { /* other headers ignored */ }
        }
    }

    Ok(headers)
}

/// Extract a named parameter from a `Content-Disposition` value.
///
/// Handles both `param="quoted value"` and `param=token` forms.
fn extract_disposition_param(header_value: &str, param_name: &str) -> Option<String> {
    let target = param_name.to_lowercase();
    for segment in header_value.split(';') {
        let segment = segment.trim();
        // Compare the key part case-insensitively.
        let lower_seg = segment.to_lowercase();
        let prefix = format!("{}=", target);
        if !lower_seg.starts_with(&prefix) {
            continue;
        }
        let value = segment[prefix.len()..].trim();
        // Strip surrounding quotes if present.
        if value.starts_with('"') && value.ends_with('"') && value.len() >= 2 {
            return Some(value[1..value.len() - 1].to_string());
        }
        return Some(value.to_string());
    }
    None
}
