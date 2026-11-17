//! A tiny HTTP/1.1 client over any stream: the Docker socket (Unix) and the League
//! client's local API (TLS on 127.0.0.1). One request per connection.

use std::io::{self, Read, Write};

pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

impl Response {
    pub fn json(&self) -> Option<serde_json::Value> {
        serde_json::from_slice(&self.body).ok()
    }

    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

pub fn request<S: Read + Write>(stream: &mut S, method: &str, host: &str, path: &str, auth: Option<&str>, body: Option<&[u8]>) -> io::Result<Response> {
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nAccept: application/json\r\nUser-Agent: quaked\r\n");
    if let Some(a) = auth {
        head.push_str(&format!("Authorization: Basic {a}\r\n"));
    }
    let body = body.unwrap_or(&[]);
    if !body.is_empty() || method != "GET" {
        head.push_str(&format!("Content-Type: application/json\r\nContent-Length: {}\r\n", body.len()));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()?;
    // Read until the response is complete. Some servers (Kiri) keep the connection open
    // despite "Connection: close", so stop at Content-Length / the last chunk, not EOF.
    let mut raw = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    while !complete(&raw) {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => raw.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            // TLS peers often close without close_notify; what we got is still the response.
            Err(e) if !raw.is_empty() && matches!(e.kind(), io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionAborted | io::ErrorKind::ConnectionReset | io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => break,
            Err(e) => return Err(e),
        }
    }
    parse(&raw).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad HTTP response"))
}

/// True once `raw` holds a whole response (headers plus the body they promise).
fn complete(raw: &[u8]) -> bool {
    let Some(split) = raw.windows(4).position(|w| w == b"\r\n\r\n") else { return false };
    let head = String::from_utf8_lossy(&raw[..split]).to_ascii_lowercase();
    let body = &raw[split + 4..];
    let status: u16 = head.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    if status == 204 || status == 304 || (100..200).contains(&status) {
        return true;
    }
    for line in head.split("\r\n") {
        if let Some(v) = line.strip_prefix("content-length:") {
            return v.trim().parse::<usize>().map(|n| body.len() >= n).unwrap_or(false);
        }
        if line.starts_with("transfer-encoding:") && line.contains("chunked") {
            return body.ends_with(b"0\r\n\r\n") && (body.len() == 5 || body.windows(7).any(|w| w == b"\r\n0\r\n\r\n"));
        }
    }
    false // no length given: read to EOF
}

fn parse(raw: &[u8]) -> Option<Response> {
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = std::str::from_utf8(&raw[..split]).ok()?;
    let mut lines = head.split("\r\n");
    let status: u16 = lines.next()?.split_whitespace().nth(1)?.parse().ok()?;
    let chunked = lines.any(|l| {
        let l = l.to_ascii_lowercase();
        l.starts_with("transfer-encoding:") && l.contains("chunked")
    });
    let body = &raw[split + 4..];
    let body = if chunked { dechunk(body) } else { body.to_vec() };
    Some(Response { status, body })
}

fn dechunk(mut b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(nl) = b.windows(2).position(|w| w == b"\r\n") {
        let size_str = std::str::from_utf8(&b[..nl]).unwrap_or("0");
        let size = usize::from_str_radix(size_str.split(';').next().unwrap_or("0").trim(), 16).unwrap_or(0);
        b = &b[nl + 2..];
        if size == 0 || size > b.len() {
            if size > 0 {
                out.extend_from_slice(b);
            }
            break;
        }
        out.extend_from_slice(&b[..size]);
        b = &b[(size + 2).min(b.len())..];
    }
    out
}

pub fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_and_chunked() {
        let r = parse(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}").unwrap();
        assert_eq!((r.status, r.body.as_slice()), (200, b"{}".as_slice()));
        let r = parse(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n").unwrap();
        assert_eq!(r.body, b"Wikipedia");
        assert_eq!(parse(b"HTTP/1.1 204 No Content\r\n\r\n").unwrap().status, 204);
    }

    #[test]
    fn knows_when_a_response_is_complete() {
        assert!(!complete(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nab"));
        assert!(complete(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nabcd"));
        assert!(complete(b"HTTP/1.1 204 No Content\r\n\r\n"));
        assert!(!complete(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nWiki\r\n"));
        assert!(complete(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nWiki\r\n0\r\n\r\n"));
        assert!(!complete(b"HTTP/1.1 200 OK\r\n\r\nstreaming"));
    }

    #[test]
    fn base64_matches() {
        assert_eq!(base64(b"riot:abc"), "cmlvdDphYmM=");
        assert_eq!(base64(b"ab"), "YWI=");
    }
}
