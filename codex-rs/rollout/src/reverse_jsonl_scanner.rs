use std::io;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;

use serde::de::DeserializeOwned;

const READ_CHUNK_SIZE: usize = 64 * 1024;

#[derive(Debug)]
pub enum ScanOutcome<T> {
    /// The record was valid JSON and deserialized as the requested type.
    Parsed(T),
    /// The record was not valid JSON for the requested type.
    #[allow(dead_code)]
    Rejected(serde_json::Error),
}

/// Read-only scanner for newline-delimited JSON records, starting from the end.
pub struct ReverseJsonlScanner<R> {
    reader: R,
    scan_start: u64,
    next_chunk_end: u64,
    chunk_position: usize,
    chunk: Vec<u8>,
    record_reversed: Vec<u8>,
    bytes_read: u64,
    reached_start: bool,
    last_record_start_offset: Option<u64>,
    max_record_bytes: Option<usize>,
    discarding_oversized_record: bool,
}

impl<R> ReverseJsonlScanner<R>
where
    R: Read + Seek,
{
    pub fn new(mut reader: R) -> io::Result<Self> {
        let next_chunk_end = reader.seek(SeekFrom::End(0))?;
        Self::new_at(reader, next_chunk_end)
    }

    /// Creates a reverse scanner whose logical end is the given byte offset.
    ///
    /// This lets callers scan a frozen JSONL prefix without reading records appended after that
    /// prefix was captured.
    pub fn new_at(reader: R, end_byte_offset: u64) -> io::Result<Self> {
        Self::new_at_with_start(reader, end_byte_offset, /*scan_start*/ 0)
    }

    /// Creates a reverse scanner whose physical reads are capped to `max_bytes`.
    ///
    /// If the byte limit lands inside a record, that partial record is discarded. Callers can
    /// inspect [`Self::reached_start`] to distinguish an exhausted source from a bounded cutoff.
    pub fn new_at_with_byte_limit(
        reader: R,
        end_byte_offset: u64,
        max_bytes: u64,
    ) -> io::Result<Self> {
        let scan_start = end_byte_offset.saturating_sub(max_bytes);
        Self::new_at_with_start(reader, end_byte_offset, scan_start)
    }

    fn new_at_with_start(mut reader: R, end_byte_offset: u64, scan_start: u64) -> io::Result<Self> {
        let file_len = reader.seek(SeekFrom::End(0))?;
        if end_byte_offset > file_len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "reverse JSONL scan end is past the file",
            ));
        }
        if scan_start > end_byte_offset {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "reverse JSONL scan start is past its end",
            ));
        }
        Ok(Self {
            reader,
            scan_start,
            next_chunk_end: end_byte_offset,
            chunk_position: 0,
            chunk: vec![0; READ_CHUNK_SIZE],
            record_reversed: Vec::new(),
            bytes_read: 0,
            reached_start: end_byte_offset == 0,
            last_record_start_offset: None,
            max_record_bytes: None,
            discarding_oversized_record: false,
        })
    }

    /// Returns the number of physical source bytes read by this scanner.
    pub fn bytes_read(&self) -> u64 {
        self.bytes_read
    }

    /// Returns whether scanning reached the real beginning of the source.
    pub fn reached_start(&self) -> bool {
        self.reached_start
    }

    /// Returns the absolute byte offset of the last nonblank record returned by the scanner.
    pub fn last_record_start_offset(&self) -> Option<u64> {
        self.last_record_start_offset
    }

    /// Skips records larger than the configured limit without buffering or parsing them.
    pub fn with_max_record_bytes(mut self, max_record_bytes: usize) -> Self {
        self.max_record_bytes = Some(max_record_bytes);
        self
    }

    /// Scans the next nonblank record.
    ///
    /// I/O failures are returned as [`Err`]. Invalid JSON records are returned as
    /// [`ScanOutcome::Rejected`], and the scanner remains usable.
    pub fn scan_next<T>(&mut self) -> io::Result<Option<ScanOutcome<T>>>
    where
        T: DeserializeOwned,
    {
        loop {
            let Some(byte) = self.read_previous_byte()? else {
                if !self.reached_start {
                    self.record_reversed.clear();
                    self.discarding_oversized_record = false;
                    return Ok(None);
                }
                if self.discarding_oversized_record {
                    self.record_reversed.clear();
                    self.discarding_oversized_record = false;
                    return Ok(None);
                }
                if let Some(outcome) = self.finish_record(/*record_start_offset*/ 0) {
                    return Ok(Some(outcome));
                }
                return Ok(None);
            };

            if byte == b'\n' {
                let record_start_offset = self.next_chunk_end + self.chunk_position as u64 + 1;
                if self.discarding_oversized_record {
                    self.record_reversed.clear();
                    self.discarding_oversized_record = false;
                    continue;
                }
                if let Some(outcome) = self.finish_record(record_start_offset) {
                    return Ok(Some(outcome));
                }
                continue;
            }

            if self.discarding_oversized_record {
                continue;
            }
            if self
                .max_record_bytes
                .is_some_and(|max| self.record_reversed.len().saturating_add(1) > max)
            {
                self.record_reversed.clear();
                self.discarding_oversized_record = true;
            } else {
                self.record_reversed.push(byte);
            }
        }
    }

    fn read_previous_byte(&mut self) -> io::Result<Option<u8>> {
        if self.chunk_position == 0 {
            if self.next_chunk_end == self.scan_start {
                self.reached_start = self.scan_start == 0;
                return Ok(None);
            }

            let unread_bytes = self.next_chunk_end - self.scan_start;
            let read_size = usize::try_from(unread_bytes.min(READ_CHUNK_SIZE as u64))
                .map_err(io::Error::other)?;
            self.next_chunk_end -= read_size as u64;
            self.reader.seek(SeekFrom::Start(self.next_chunk_end))?;
            self.reader.read_exact(&mut self.chunk[..read_size])?;
            self.chunk_position = read_size;
            self.bytes_read += read_size as u64;
        }

        self.chunk_position -= 1;
        Ok(Some(self.chunk[self.chunk_position]))
    }

    fn finish_record<T>(&mut self, record_start_offset: u64) -> Option<ScanOutcome<T>>
    where
        T: DeserializeOwned,
    {
        self.record_reversed.reverse();
        let outcome = if self.record_reversed.iter().all(u8::is_ascii_whitespace) {
            None
        } else {
            self.last_record_start_offset = Some(record_start_offset);
            Some(match serde_json::from_slice::<T>(&self.record_reversed) {
                Ok(value) => ScanOutcome::Parsed(value),
                Err(error) => ScanOutcome::Rejected(error),
            })
        };
        self.record_reversed.clear();
        outcome
    }
}

#[cfg(test)]
#[path = "reverse_jsonl_scanner_tests.rs"]
mod tests;
