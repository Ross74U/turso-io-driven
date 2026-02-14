use anyhow::{anyhow, Result};
use crate::unwrap_completion;
use crate::io::generic::ClientConnection;
use crate::io::completion::{Completion, SharedCompletion, AppCompletion};
use super::{ProgramWaker, StepResult};
use std::sync::Arc;
use serde::de::DeserializeOwned;

pub struct ReceivingBodyProgram {
    buf: Vec<u8>,
    chunked: bool,
    received: u64,
    content_length: Option<u64>,
    conn: Arc<dyn ClientConnection>,
    completion: Option<SharedCompletion>,
    // Encoded bytes read from socket that haven't been chunk-decoded yet
    in_buf: Vec<u8>,
    chunk_dec: Option<ChunkedDecoder>
}

impl ReceivingBodyProgram {
    pub fn new(conn: Arc<dyn ClientConnection>, chunked: bool, content_length: Option<u64>, buf: Vec<u8>) -> Self {
        let chunk_dec = if chunked {
            // pick a limit appropriate for your API
            Some(ChunkedDecoder::new(1 * 1024 * 1024)) // 1 MiB
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
    
    pub fn decode<T: DeserializeOwned>(&self) -> Result<T> {
        Ok(serde_json::from_slice::<T>(&self.buf)?)
    }
    
    pub fn raw(&self) -> &[u8] {
        &self.buf
    }

     
    pub fn step(&mut self, waker: ProgramWaker) -> Result<StepResult<()>> {
        match (self.chunked, self.content_length) {
            (true, None) => self.step_chunked(waker),
            (false, Some(_)) => self.step_content_length(waker),
            _ => unreachable!()
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
                            // IMPORTANT: these are encoded bytes (chunk framing + data)
                            self.in_buf.extend_from_slice(c.buf());
                        }
                        None => { unreachable!("spurious wakeup") }
                    }
                },
                { unreachable!() }
            );
            self.completion = None;
        }

        // Drive decoder with whatever we have buffered
        let dec = self
            .chunk_dec
            .as_mut()
            .ok_or_else(|| anyhow!("step_chunked called but chunk decoder missing"))?;

        loop {
            match dec.advance(&mut self.in_buf, &mut self.buf)? {
                Advance::Progress => continue, // keep consuming buffered encoded bytes
                Advance::Done => return Ok(StepResult::Complete(())),
                Advance::NeedMore => break,
            }
        }

        const READ_SIZE: usize = 16 * 1024;

        let recvc = Arc::new(Completion::AppCompletion(
            AppCompletion::new_recv(waker.clone(), READ_SIZE),
        ));
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

        let recvc = Arc::new(Completion::AppCompletion(
            AppCompletion::new_recv(waker.clone(), remaining as usize)
        ));
        self.conn.recv(recvc.clone())?;
        self.completion = Some(recvc);
        Ok(StepResult::Pending)
    }

}

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
                        return Ok(if made_progress { Advance::Progress } else { Advance::NeedMore });
                    };
                    if crlf_at > self.max_size_line {
                        return Err(anyhow!("chunk size line too long"));
                    }

                    let line = drain_line(in_buf, crlf_at); // excludes CRLF
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
                        return Ok(if made_progress { Advance::Progress } else { Advance::NeedMore });
                    }

                    if self.decoded_total.saturating_add(remaining) > self.max_decoded {
                        return Err(anyhow!("decoded body exceeds limit ({} bytes)", self.max_decoded));
                    }

                    out.extend_from_slice(&in_buf[..remaining]);
                    in_buf.drain(..remaining);

                    self.decoded_total += remaining;
                    self.state = ChunkState::ReadDataCrlf;
                    made_progress = true;
                }

                ChunkState::ReadDataCrlf => {
                    if in_buf.len() < 2 {
                        return Ok(if made_progress { Advance::Progress } else { Advance::NeedMore });
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
                        return Ok(if made_progress { Advance::Progress } else { Advance::NeedMore });
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

                    // ignore trailer header line
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
        n = n.checked_mul(16).ok_or_else(|| anyhow!("chunk size overflow"))?;
        n = n.checked_add(v).ok_or_else(|| anyhow!("chunk size overflow"))?;
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
