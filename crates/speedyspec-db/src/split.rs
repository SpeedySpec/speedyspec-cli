//! Splitting a SQL dump into statements, as the `mysql` command-line client does.
//!
//! The input is read as bytes, a buffer at a time, because a dump can be larger than memory and
//! holds binary data that is not valid UTF-8. A statement ends at a `;` outside quotes and
//! comments. `'…'` and `"…"` take backslash escapes and doubled quotes; `` `…` `` takes doubled
//! backticks. `-- ` (with a space), `#` and `/* … */` comments are dropped; `/*! … */`
//! conditional comments are part of the statement. `DELIMITER` (stored routines) is refused.

use std::fmt;
use std::io::BufRead;

/// How much is read from the input at a time.
const CHUNK: usize = 64 * 1024;

/// Why a dump cannot be split.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplitError {
    /// 1-based line of the input.
    pub line: usize,
    pub message: String,
}

impl fmt::Display for SplitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for SplitError {}

/// The statements of a dump, each with the line it starts on.
pub struct Statements<R> {
    reader: R,
    buf: Vec<u8>,
    pos: usize,
    eof: bool,
    line: usize,
    failed: bool,
}

impl<R: BufRead> Statements<R> {
    pub fn new(reader: R) -> Self {
        Statements {
            reader,
            buf: Vec::new(),
            pos: 0,
            eof: false,
            line: 1,
            failed: false,
        }
    }

    /// The byte `ahead` places from the current one, reading more input when needed.
    fn peek(&mut self, ahead: usize) -> Result<Option<u8>, SplitError> {
        while self.pos + ahead >= self.buf.len() && !self.eof {
            if self.pos > 0 {
                self.buf.drain(..self.pos);
                self.pos = 0;
            }
            let read = {
                let chunk = self.reader.fill_buf().map_err(|e| SplitError {
                    line: self.line,
                    message: format!("could not read the dump: {e}"),
                })?;
                let n = chunk.len().min(CHUNK);
                self.buf.extend_from_slice(&chunk[..n]);
                n
            };
            self.reader.consume(read);
            if read == 0 {
                self.eof = true;
            }
        }
        Ok(self.buf.get(self.pos + ahead).copied())
    }

    /// Takes the current byte, counting lines.
    fn take(&mut self) -> Result<Option<u8>, SplitError> {
        let byte = self.peek(0)?;
        if let Some(b) = byte {
            self.pos += 1;
            if b == b'\n' {
                self.line += 1;
            }
        }
        Ok(byte)
    }

    /// Whether the input continues, case-insensitively, with `word` and then whitespace or the
    /// end.
    fn starts_word(&mut self, word: &[u8]) -> Result<bool, SplitError> {
        for (i, expected) in word.iter().enumerate() {
            match self.peek(i)? {
                Some(b) if b.eq_ignore_ascii_case(expected) => {}
                _ => return Ok(false),
            }
        }
        Ok(self
            .peek(word.len())?
            .is_none_or(|b| b.is_ascii_whitespace()))
    }

    fn skip_line(&mut self) -> Result<(), SplitError> {
        while let Some(b) = self.take()? {
            if b == b'\n' {
                break;
            }
        }
        Ok(())
    }

    /// The next statement, or `None` at the end of the input.
    fn statement(&mut self) -> Result<Option<(usize, Vec<u8>)>, SplitError> {
        let mut statement = Vec::new();
        let mut start: Option<usize> = None;
        loop {
            let Some(b) = self.peek(0)? else {
                return Ok(start.map(|line| (line, trimmed(statement))));
            };
            match b {
                b'\'' | b'"' | b'`' => {
                    start.get_or_insert(self.line);
                    self.quoted(b, &mut statement)?;
                }
                b'-' if self.peek(1)? == Some(b'-')
                    && self.peek(2)?.is_none_or(|c| c.is_ascii_whitespace()) =>
                {
                    self.skip_line()?;
                    if start.is_some() {
                        statement.push(b'\n');
                    }
                }
                b'#' => {
                    self.skip_line()?;
                    if start.is_some() {
                        statement.push(b'\n');
                    }
                }
                b'/' if self.peek(1)? == Some(b'*') => {
                    let conditional = self.peek(2)? == Some(b'!');
                    if conditional {
                        start.get_or_insert(self.line);
                    }
                    self.block_comment(conditional.then_some(&mut statement))?;
                    if !conditional && start.is_some() {
                        statement.push(b' ');
                    }
                }
                b';' => {
                    self.take()?;
                    if let Some(line) = start {
                        return Ok(Some((line, trimmed(statement))));
                    }
                }
                _ if b.is_ascii_whitespace() => {
                    self.take()?;
                    if start.is_some() {
                        statement.push(b);
                    }
                }
                _ => {
                    if start.is_none() {
                        if self.starts_word(b"DELIMITER")? {
                            return Err(SplitError {
                                line: self.line,
                                message: "DELIMITER: stored routines are not supported".to_owned(),
                            });
                        }
                        start = Some(self.line);
                    }
                    self.take()?;
                    statement.push(b);
                }
            }
        }
    }

    /// Copies a quoted string or identifier, quotes and escapes included.
    fn quoted(&mut self, quote: u8, statement: &mut Vec<u8>) -> Result<(), SplitError> {
        let opened = self.line;
        let unterminated = || SplitError {
            line: opened,
            message: format!("unterminated {} that starts here", describe(quote)),
        };
        self.take()?;
        statement.push(quote);
        loop {
            let b = self.take()?.ok_or_else(unterminated)?;
            statement.push(b);
            if b == b'\\' && quote != b'`' {
                let escaped = self.take()?.ok_or_else(unterminated)?;
                statement.push(escaped);
            } else if b == quote {
                if self.peek(0)? == Some(quote) {
                    self.take()?;
                    statement.push(quote);
                } else {
                    return Ok(());
                }
            }
        }
    }

    /// Skips a `/* … */` comment, or copies it into `keep` (a conditional comment).
    fn block_comment(&mut self, mut keep: Option<&mut Vec<u8>>) -> Result<(), SplitError> {
        let opened = self.line;
        let unterminated = || SplitError {
            line: opened,
            message: "unterminated /* comment that starts here".to_owned(),
        };
        // The opening "/*", so that "/*/" does not count as closed.
        for _ in 0..2 {
            let b = self.take()?.ok_or_else(unterminated)?;
            if let Some(keep) = keep.as_deref_mut() {
                keep.push(b);
            }
        }
        let mut previous = 0u8;
        loop {
            let b = self.take()?.ok_or_else(unterminated)?;
            if let Some(keep) = keep.as_deref_mut() {
                keep.push(b);
            }
            if previous == b'*' && b == b'/' {
                return Ok(());
            }
            previous = b;
        }
    }
}

fn describe(quote: u8) -> &'static str {
    match quote {
        b'\'' => "'…' string",
        b'"' => "\"…\" string",
        _ => "`…` identifier",
    }
}

fn trimmed(mut statement: Vec<u8>) -> Vec<u8> {
    while statement.last().is_some_and(u8::is_ascii_whitespace) {
        statement.pop();
    }
    let lead = statement
        .iter()
        .take_while(|b| b.is_ascii_whitespace())
        .count();
    statement.drain(..lead);
    statement
}

impl<R: BufRead> Iterator for Statements<R> {
    type Item = Result<(usize, Vec<u8>), SplitError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        match self.statement() {
            Ok(Some(found)) => Some(Ok(found)),
            Ok(None) => None,
            Err(error) => {
                self.failed = true;
                Some(Err(error))
            }
        }
    }
}
