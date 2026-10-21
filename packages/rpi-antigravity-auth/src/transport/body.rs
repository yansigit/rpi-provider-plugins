//! HTTP body framing and bounded incremental gzip output.
use super::{integer_prefix, ResponseHead};
use flate2::{Decompress, FlushDecompress, Status};
use std::io;

pub struct Framer {
    buffer: Vec<u8>,
    chunked: bool,
    remaining: Option<f64>,
    complete: bool,
}
impl Framer {
    pub fn new(head: &ResponseHead) -> Self {
        Self {
            buffer: Vec::new(),
            chunked: head.chunked,
            remaining: head.content_length,
            complete: false,
        }
    }
    pub fn push(&mut self, bytes: &[u8]) {
        if !self.complete {
            self.buffer.extend_from_slice(bytes);
        }
    }
    pub fn is_complete(&self) -> bool {
        self.complete
    }
    pub fn next(&mut self, eof: bool) -> Result<Option<Vec<u8>>, String> {
        if self.complete {
            return Ok(None);
        }
        if !self.chunked {
            let mut bytes = std::mem::take(&mut self.buffer);
            if let Some(remaining) = &mut self.remaining {
                if *remaining <= 0.0 {
                    bytes.clear();
                } else {
                    bytes.truncate((bytes.len() as f64).min(*remaining) as usize);
                    *remaining -= bytes.len() as f64;
                    self.complete = *remaining <= 0.0;
                }
            }
            self.complete |= eof;
            return Ok((!bytes.is_empty()).then_some(bytes));
        }
        loop {
            let Some(line_end) = self.buffer.windows(2).position(|bytes| bytes == b"\r\n") else {
                self.complete = eof;
                return Ok(None);
            };
            let line: String = self.buffer[..line_end]
                .iter()
                .map(|byte| char::from(*byte))
                .collect();
            let size = integer_prefix(line.split(';').next().unwrap_or_default(), 16)
                .ok_or_else(|| format!("Invalid chunk size: {line}"))?;
            let chunk_start = line_end + 2;
            let chunk_end = chunk_start as f64 + size;
            let next_offset = chunk_end + 2.0;
            if (self.buffer.len() as f64) < next_offset {
                self.complete = eof;
                return Ok(None);
            }
            if size == 0.0 {
                self.buffer.clear();
                self.complete = true;
                return Ok(None);
            }
            let offset = |value: f64| {
                if value < 0.0 {
                    (self.buffer.len() as f64 + value).max(0.0) as usize
                } else {
                    value.min(self.buffer.len() as f64) as usize
                }
            };
            let end = offset(chunk_end).max(chunk_start);
            let consumed = offset(next_offset);
            // A non-progressing malformed frame remains stalled, without blocking cancellation.
            if consumed == 0 {
                return Ok(None);
            }
            let bytes = self.buffer[chunk_start..end].to_vec();
            self.buffer.drain(..consumed);
            if !bytes.is_empty() {
                return Ok(Some(bytes));
            }
        }
    }
}

const OUTPUT_CHUNK: usize = 16 * 1024;
pub struct GzipDecoder {
    decoder: Decompress,
    member_end: bool,
    padding: bool,
}
impl Default for GzipDecoder {
    fn default() -> Self {
        Self {
            decoder: Decompress::new_gzip(15),
            member_end: false,
            padding: false,
        }
    }
}
impl GzipDecoder {
    /// Returns consumed compressed bytes and at most 16 KiB of decoded output.
    pub fn push(&mut self, bytes: &[u8]) -> io::Result<(usize, Vec<u8>)> {
        if self.padding {
            return Ok((bytes.len(), Vec::new()));
        }
        if self.member_end && !bytes.is_empty() {
            // Node gunzip ignores everything after a zero byte between members.
            if bytes[0] == 0 {
                self.padding = true;
                return Ok((bytes.len(), Vec::new()));
            }
            self.decoder = Decompress::new_gzip(15);
            self.member_end = false;
        }
        self.decode(bytes, FlushDecompress::None)
    }
    fn decode(&mut self, bytes: &[u8], flush: FlushDecompress) -> io::Result<(usize, Vec<u8>)> {
        let mut output = vec![0; OUTPUT_CHUNK];
        let before_in = self.decoder.total_in();
        let before_out = self.decoder.total_out();
        let status = self
            .decoder
            .decompress(bytes, &mut output, flush)
            .map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    error.message().unwrap_or("invalid compressed data"),
                )
            })?;
        output.truncate((self.decoder.total_out() - before_out) as usize);
        self.member_end = status == Status::StreamEnd;
        if flush == FlushDecompress::Finish && !self.member_end && output.len() < OUTPUT_CHUNK {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "unexpected end of file",
            ));
        }
        Ok(((self.decoder.total_in() - before_in) as usize, output))
    }
    /// Drain between reads or at EOF. Repeat until `done` after forwarding each chunk.
    pub fn drain(&mut self, eof: bool) -> io::Result<(bool, Vec<u8>)> {
        if self.member_end || self.padding {
            return Ok((true, Vec::new()));
        }
        let (_, output) = self.decode(
            &[],
            if eof {
                FlushDecompress::Finish
            } else {
                FlushDecompress::None
            },
        )?;
        Ok((self.member_end || output.len() < OUTPUT_CHUNK, output))
    }
}
