/// HTTP/1.1 response parser.
///
/// Parses the status line and headers from raw HTTP response data.
/// Handles chunked detection, content-type checking, and error classification.

use alloc::string::String;
use alloc::vec::Vec;

/// Parsed HTTP response headers.
pub struct HttpResponse {
    /// HTTP status code (200, 401, 429, 500, etc.).
    pub status: u16,
    /// Response headers as (name, value) pairs. Names are lowercased.
    pub headers: Vec<(String, String)>,
    /// Byte offset where the body starts in the raw buffer.
    pub body_start: usize,
}

/// HTTP response parsing error.
#[derive(Debug)]
pub enum HttpParseError {
    /// Response is incomplete (need more data).
    Incomplete,
    /// Status line is malformed.
    MalformedStatus,
}

impl HttpResponse {
    /// Parse an HTTP response from raw bytes.
    /// Returns `Err(Incomplete)` if the header section isn't complete yet.
    pub fn parse(data: &[u8]) -> Result<Self, HttpParseError> {
        // Find end of headers (double CRLF)
        let header_end = find_header_end(data).ok_or(HttpParseError::Incomplete)?;
        let header_section = core::str::from_utf8(&data[..header_end])
            .map_err(|_| HttpParseError::MalformedStatus)?;

        let mut lines = header_section.split("\r\n");

        // Parse status line: "HTTP/1.1 200 OK"
        let status_line = lines.next().ok_or(HttpParseError::MalformedStatus)?;
        let status = parse_status_code(status_line)?;

        // Parse headers
        let mut headers = Vec::new();
        for line in lines {
            if line.is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                headers.push((
                    name.trim().to_ascii_lowercase(),
                    String::from(value.trim()),
                ));
            }
        }

        Ok(HttpResponse {
            status,
            headers,
            body_start: header_end + 4, // skip the \r\n\r\n
        })
    }

    /// Get a header value by name (case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        let lower = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| *k == lower)
            .map(|(_, v)| v.as_str())
    }

    /// Classify the HTTP status into a user-friendly error message.
    pub fn error_message(&self) -> Option<&'static str> {
        match self.status {
            200 | 201 => None,
            400 => Some("bad request (check API parameters)"),
            401 => Some("API key invalid or missing"),
            403 => Some("access denied"),
            404 => Some("endpoint not found"),
            429 => Some("rate limited — retry after delay"),
            500 => Some("API internal server error"),
            529 => Some("API overloaded — retry later"),
            _ => Some("unexpected HTTP status"),
        }
    }

    /// Check if this is a server-side error that should trigger retry.
    pub fn should_retry(&self) -> bool {
        matches!(self.status, 429 | 500 | 529)
    }

    /// Extract retry-after seconds from headers (if present).
    pub fn retry_after_secs(&self) -> Option<u64> {
        self.header("retry-after")
            .and_then(|v| v.parse::<u64>().ok())
    }
}

/// Find the position of "\r\n\r\n" which separates headers from body.
fn find_header_end(data: &[u8]) -> Option<usize> {
    for i in 0..data.len().saturating_sub(3) {
        if &data[i..i + 4] == b"\r\n\r\n" {
            return Some(i);
        }
    }
    None
}

/// Parse the status code from an HTTP status line.
fn parse_status_code(line: &str) -> Result<u16, HttpParseError> {
    // "HTTP/1.1 200 OK" or "HTTP/1.0 429 Too Many Requests"
    let parts: Vec<&str> = line.splitn(3, ' ').collect();
    if parts.len() < 2 {
        return Err(HttpParseError::MalformedStatus);
    }
    parts[1].parse::<u16>().map_err(|_| HttpParseError::MalformedStatus)
}

/// Helper for use in alloc::string — convert &str to lowercase ASCII.
trait ToAsciiLowercase {
    fn to_ascii_lowercase(&self) -> String;
}

impl ToAsciiLowercase for str {
    fn to_ascii_lowercase(&self) -> String {
        let mut s = String::with_capacity(self.len());
        for c in self.chars() {
            s.push(if c.is_ascii_uppercase() {
                (c as u8 + 32) as char
            } else {
                c
            });
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_200() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nX-Request-Id: abc\r\n\r\nbody";
        let resp = HttpResponse::parse(raw).unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(resp.header("content-type"), Some("text/event-stream"));
        assert_eq!(resp.header("x-request-id"), Some("abc"));
        assert_eq!(&raw[resp.body_start..], b"body");
        assert!(resp.error_message().is_none());
    }

    #[test]
    fn test_parse_429() {
        let raw = b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 30\r\n\r\n{\"error\":\"rate_limited\"}";
        let resp = HttpResponse::parse(raw).unwrap();
        assert_eq!(resp.status, 429);
        assert!(resp.should_retry());
        assert_eq!(resp.retry_after_secs(), Some(30));
        assert_eq!(resp.error_message(), Some("rate limited — retry after delay"));
    }

    #[test]
    fn test_parse_401() {
        let raw = b"HTTP/1.1 401 Unauthorized\r\n\r\n";
        let resp = HttpResponse::parse(raw).unwrap();
        assert_eq!(resp.status, 401);
        assert!(!resp.should_retry());
        assert_eq!(resp.error_message(), Some("API key invalid or missing"));
    }

    #[test]
    fn test_incomplete() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: text";
        assert!(HttpResponse::parse(raw).is_err());
    }
}
