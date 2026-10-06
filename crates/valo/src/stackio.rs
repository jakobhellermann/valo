use std::io::{BufRead, Read, Result, Seek, SeekFrom};

struct Buffer<const N: usize> {
    buf: [u8; N],
    // seek offset, <= filled
    pos: usize,
    // from the start of buf
    filled: usize,
}
impl<const N: usize> Buffer<N> {
    fn fill_buf(&mut self, mut reader: impl Read) -> Result<&[u8]> {
        // fetch new
        if self.pos >= self.filled {
            debug_assert!(self.pos == self.filled);

            self.pos = 0;
            self.filled = 0;
            let n = reader.read(self.unfilled())?;
            self.filled = n;
        }

        Ok(self.buffer())
    }

    fn consume(&mut self, amount: usize) {
        self.pos = std::cmp::min(self.pos + amount, self.filled);
    }

    fn unfilled(&mut self) -> &mut [u8] {
        &mut self.buf[self.filled..]
    }
    fn buffer(&self) -> &[u8] {
        &self.buf[self.pos..self.filled]
    }

    fn backshift(&mut self) {
        self.buf.copy_within(self.pos..self.filled, 0);
        self.filled -= self.pos;
        self.pos = 0;
    }

    fn read_more(&mut self, mut reader: impl Read) -> Result<()> {
        let n = reader.read(self.unfilled())?;
        self.filled += n;
        Ok(())
    }

    fn is_full(&self) -> bool {
        self.filled == N
    }
}

pub struct StackBufReader<R, const N: usize> {
    reader: R,
    buffer: Buffer<N>,
}

impl<R, const N: usize> StackBufReader<R, N> {
    pub fn new(reader: R) -> StackBufReader<R, N> {
        StackBufReader {
            reader,
            buffer: Buffer {
                buf: [0; N],
                pos: 0,
                filled: 0,
            },
        }
    }
}

impl<R: Read, const N: usize> Read for StackBufReader<R, N> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let mut rem = self.fill_buf()?;
        let n = rem.read(buf)?;
        self.consume(n);
        Ok(n)
    }
}

impl<R: Read, const N: usize> BufRead for StackBufReader<R, N> {
    fn fill_buf(&mut self) -> Result<&[u8]> {
        self.buffer.fill_buf(&mut self.reader)
    }

    fn consume(&mut self, amount: usize) {
        self.buffer.consume(amount)
    }
}

const ERR_INSUFFICIENT_SIZE: std::io::Error = std::io::const_error!(
    std::io::ErrorKind::Other,
    "StackBufReader isn't big enough to fit a line"
);

impl<R: Read, const N: usize> StackBufReader<R, N> {
    pub fn read_more(&mut self) -> Result<()> {
        self.buffer.read_more(&mut self.reader)
    }

    pub fn read_until(&mut self, separator: u8) -> Result<Option<&[u8]>> {
        let mut buf = self.fill_buf()?;
        loop {
            match memchr::memchr(separator, buf) {
                Some(found) => {
                    let pos = self.buffer.pos;
                    self.consume(found + 1);
                    let line = &self.buffer.buf[pos..pos + found];
                    return Ok(Some(line));
                }
                None => {
                    if self.buffer.is_full() && self.buffer.pos == 0 {
                        return Err(ERR_INSUFFICIENT_SIZE);
                    }

                    self.buffer.backshift();
                    self.read_more()?;

                    buf = self.buffer.buffer();
                    if buf.is_empty() {
                        return Ok(None);
                    }
                }
            }
        }
    }
}

impl<R: Read + Seek, const N: usize> StackBufReader<R, N> {
    pub fn reset(&mut self) -> Result<()> {
        self.buffer.pos = 0;
        self.buffer.filled = 0;
        self.reader.seek(SeekFrom::Start(0))?;
        Ok(())
    }
}
