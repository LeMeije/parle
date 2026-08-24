//! The few bytes of framing that have to exist *before* a session does.
//!
//! `echokey-sync` is transport-agnostic: pairing hands back opaque byte blobs
//! and it is the app's job to move them. This is that transport, and nothing
//! more — length-prefixed frames over plain TCP, used only until a paired key
//! exists, after which everything moves inside the Noise session.
//!
//! Everything here is untrusted input from whatever connected to our port, so
//! every read is bounded and every length is checked before it is allocated.

use std::io::{Read, Write};
use std::net::TcpStream;

/// First byte of every inbound connection: what the caller wants.
pub const MODE_PAIR: u8 = 0x01;
pub const MODE_SESSION: u8 = 0x02;

/// Nothing exchanged before a session is remotely this big. A SPAKE2 message
/// is ~32-64 bytes and a device id is a UUID; the cap exists so a hostile
/// peer cannot make us allocate.
const MAX_FRAME: usize = 4096;

#[derive(Debug, thiserror::Error)]
pub enum TcpError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("frame of {0} bytes exceeds the {MAX_FRAME} byte limit")]
    TooLarge(usize),
    #[error("peer closed the connection mid-frame")]
    Truncated,
}

pub fn write_frame(s: &mut TcpStream, buf: &[u8]) -> Result<(), TcpError> {
    if buf.len() > MAX_FRAME {
        return Err(TcpError::TooLarge(buf.len()));
    }
    s.write_all(&(buf.len() as u16).to_be_bytes())?;
    s.write_all(buf)?;
    s.flush()?;
    Ok(())
}

pub fn read_frame(s: &mut TcpStream) -> Result<Vec<u8>, TcpError> {
    let mut len = [0u8; 2];
    read_exact(s, &mut len)?;
    let n = u16::from_be_bytes(len) as usize;
    // Checked BEFORE allocating: the length is attacker-controlled.
    if n > MAX_FRAME {
        return Err(TcpError::TooLarge(n));
    }
    let mut buf = vec![0u8; n];
    read_exact(s, &mut buf)?;
    Ok(buf)
}

pub fn write_byte(s: &mut TcpStream, b: u8) -> Result<(), TcpError> {
    s.write_all(&[b])?;
    s.flush()?;
    Ok(())
}

pub fn read_byte(s: &mut TcpStream) -> Result<u8, TcpError> {
    let mut b = [0u8; 1];
    read_exact(s, &mut b)?;
    Ok(b[0])
}

fn read_exact(s: &mut TcpStream, buf: &mut [u8]) -> Result<(), TcpError> {
    match s.read_exact(buf) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Err(TcpError::Truncated),
        Err(e) => Err(TcpError::Io(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};

    fn pair() -> (TcpStream, TcpStream) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let c = TcpStream::connect(addr).unwrap();
        let (s, _) = l.accept().unwrap();
        (c, s)
    }

    #[test]
    fn frames_round_trip() {
        let (mut a, mut b) = pair();
        write_frame(&mut a, b"hello").unwrap();
        assert_eq!(read_frame(&mut b).unwrap(), b"hello");
    }

    #[test]
    fn an_empty_frame_is_legal() {
        let (mut a, mut b) = pair();
        write_frame(&mut a, b"").unwrap();
        assert_eq!(read_frame(&mut b).unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn an_oversized_declared_length_is_refused_before_allocating() {
        let (mut a, mut b) = pair();
        // Hand-craft a header claiming 65535 bytes and then send nothing, the
        // shape of a peer trying to make us reserve memory on demand.
        a.write_all(&u16::MAX.to_be_bytes()).unwrap();
        a.flush().unwrap();
        match read_frame(&mut b) {
            Err(TcpError::TooLarge(n)) => assert_eq!(n, u16::MAX as usize),
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }

    #[test]
    fn we_refuse_to_send_an_oversized_frame_too() {
        let (mut a, _b) = pair();
        let big = vec![0u8; MAX_FRAME + 1];
        assert!(matches!(write_frame(&mut a, &big), Err(TcpError::TooLarge(_))));
    }

    #[test]
    fn a_truncated_frame_is_an_error_not_a_short_read() {
        let (mut a, mut b) = pair();
        a.write_all(&10u16.to_be_bytes()).unwrap();
        a.write_all(b"abc").unwrap();
        a.flush().unwrap();
        drop(a);
        assert!(matches!(read_frame(&mut b), Err(TcpError::Truncated)));
    }
}
