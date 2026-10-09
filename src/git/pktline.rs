//! Git pkt-line framing.

use std::io::{self, Read, Write};

pub const FLUSH: &[u8] = b"0000";
const MAX_PKT: usize = 65520;

#[derive(Debug, PartialEq, Eq)]
pub enum Pkt {
    Data(Vec<u8>),
    Flush,
    Delim,
    ResponseEnd,
}

pub fn encode(data: &[u8]) -> Vec<u8> {
    assert!(data.len() + 4 <= MAX_PKT, "pkt-line too long");
    let mut out = format!("{:04x}", data.len() + 4).into_bytes();
    out.extend_from_slice(data);
    out
}

pub fn write_data<W: Write>(w: &mut W, data: &[u8]) -> io::Result<()> {
    w.write_all(&encode(data))
}

pub fn write_flush<W: Write>(w: &mut W) -> io::Result<()> {
    w.write_all(FLUSH)?;
    w.flush()
}

/// Reads one pkt-line. Returns `None` at a clean end of input.
pub fn read<R: Read>(r: &mut R) -> io::Result<Option<Pkt>> {
    let mut len = [0u8; 4];
    let mut got = 0;
    while got < 4 {
        let n = r.read(&mut len[got..])?;
        if n == 0 {
            return if got == 0 {
                Ok(None)
            } else {
                Err(io::ErrorKind::UnexpectedEof.into())
            };
        }
        got += n;
    }
    let s = std::str::from_utf8(&len).map_err(|_| invalid("bad pkt-line length"))?;
    let n = usize::from_str_radix(s, 16).map_err(|_| invalid("bad pkt-line length"))?;
    match n {
        0 => Ok(Some(Pkt::Flush)),
        1 => Ok(Some(Pkt::Delim)),
        2 => Ok(Some(Pkt::ResponseEnd)),
        3 => Err(invalid("bad pkt-line length")),
        n if n > MAX_PKT => Err(invalid("pkt-line too long")),
        n => {
            let mut buf = vec![0u8; n - 4];
            r.read_exact(&mut buf)?;
            Ok(Some(Pkt::Data(buf)))
        }
    }
}

/// Reads data lines until a flush. Trailing newlines are removed.
pub fn read_until_flush<R: Read>(r: &mut R) -> io::Result<Vec<Vec<u8>>> {
    let mut out = Vec::new();
    loop {
        match read(r)? {
            Some(Pkt::Data(mut d)) => {
                if d.last() == Some(&b'\n') {
                    d.pop();
                }
                out.push(d);
            }
            Some(Pkt::Flush) => return Ok(out),
            Some(_) => return Err(invalid("unexpected special pkt-line")),
            None => return Err(io::ErrorKind::UnexpectedEof.into()),
        }
    }
}

fn invalid(m: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, m.to_string())
}
