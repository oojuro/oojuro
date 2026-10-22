//! Bounded positional source cursor, including CRC verification before varint interpretation.
use super::*;
const BUFFER: usize = 64 << 10;
pub(super) struct Cursor<'a> {
    file: &'a File,
    pos: u64,
    end: u64,
    base: u64,
    valid: usize,
    buffer: Vec<u8>,
}
impl<'a> Cursor<'a> {
    pub fn new(file: &'a File, start: u64, end: u64) -> Result<Self> {
        if start > end || end > file.metadata()?.len() {
            return Err(Error::Corrupt("cursor bounds"));
        }
        Ok(Self {
            file,
            pos: start,
            end,
            base: start,
            valid: 0,
            buffer: vec![0; BUFFER],
        })
    }
    pub fn pos(&self) -> u64 {
        self.pos
    }
    pub fn byte(&mut self) -> Result<u8> {
        if self.pos >= self.end {
            return Err(Error::Corrupt("source cursor truncated"));
        }
        if self.pos < self.base || self.pos >= self.base + self.valid as u64 {
            self.base = self.pos;
            self.valid = (self.end - self.pos).min(BUFFER as u64) as usize;
            self.file
                .read_exact_at(&mut self.buffer[..self.valid], self.pos)?;
        }
        let b = self.buffer[(self.pos - self.base) as usize];
        self.pos += 1;
        Ok(b)
    }
    pub fn fixed<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut out = [0; N];
        for b in &mut out {
            *b = self.byte()?;
        }
        Ok(out)
    }
    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.fixed()?))
    }
    pub fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.fixed()?))
    }
    pub fn varint(&mut self) -> Result<u64> {
        let mut out = 0u64;
        for n in 0..10 {
            let b = self.byte()?;
            if n == 9 && b > 1 {
                return Err(Error::Corrupt("varint overflow"));
            }
            out |= ((b & 127) as u64) << (n * 7);
            if b < 128 {
                return Ok(out);
            }
        }
        Err(Error::Corrupt("varint overflow"))
    }
    pub fn finish(self) -> Result<()> {
        if self.pos != self.end {
            return Err(Error::Corrupt("trailing source metadata"));
        }
        Ok(())
    }
}
pub(super) fn crc(file: &File, start: u64, end: u64) -> Result<u32> {
    let mut c = Cursor::new(file, start, end)?;
    let mut crc = crc32fast::Hasher::new();
    while c.pos < c.end {
        let n = (c.end - c.pos).min(BUFFER as u64) as usize;
        file.read_exact_at(&mut c.buffer[..n], c.pos)?;
        crc.update(&c.buffer[..n]);
        c.pos += n as u64;
    }
    Ok(crc.finalize())
}
pub(super) fn hash(file: &File, hasher: &mut blake3::Hasher) -> Result<()> {
    let size = file.metadata()?.len();
    let mut c = Cursor::new(file, 0, size)?;
    while c.pos < c.end {
        let n = (c.end - c.pos).min(BUFFER as u64) as usize;
        file.read_exact_at(&mut c.buffer[..n], c.pos)?;
        hasher.update(&c.buffer[..n]);
        c.pos += n as u64;
    }
    Ok(())
}
