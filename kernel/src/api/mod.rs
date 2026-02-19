/// Claude API client — calls Anthropic's Messages API from bare metal.
///
/// This module makes HTTPS POST requests to api.anthropic.com/v1/messages,
/// streaming the response token by token back to the serial console.
///
/// Architecture:
///   Shell "ask" command
///       ↓
///   claude_request(prompt) → streams tokens to serial
///       ↓
///   build_http_request() → raw HTTP/1.1 bytes
///       ↓
///   NetStack.tcp_connect(api.anthropic.com:443)
///       ↓
///   TLS (TODO — for now, plain HTTP to a local proxy, or direct HTTPS
///         when rustls is integrated)
///
/// ## TLS Reality Check
///
/// Full HTTPS to api.anthropic.com requires TLS 1.3. The path:
/// 1. rustls (pure Rust TLS) can work in no_std with the `ring` crypto backend
/// 2. ring compiles for x86_64-unknown-none (it's mostly assembly + C)
/// 3. But certificate verification needs a CA bundle and ASN.1 parsing
///
/// **Phase 1 (now)**: HTTP to a local proxy on the host machine.
///   QEMU user-mode networking can forward ports:
///   `qemu -netdev user,id=net0,hostfwd=tcp::8080-:80`
///   A simple proxy on the host (nginx, socat) terminates TLS and
///   forwards plain HTTP to/from the guest.
///
/// **Phase 2**: Integrate rustls + webpki for native HTTPS.
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::net::NetStack;
use smoltcp::wire::Ipv4Address;

/// Claude API configuration.
pub struct ClaudeConfig {
    /// API key (sk-ant-...).
    pub api_key: String,
    /// Target IP address (proxy or direct).
    /// Default: 10.0.2.2:8080 (QEMU host, local proxy).
    pub target_ip: Ipv4Address,
    pub target_port: u16,
    /// Model to use.
    pub model: String,
    /// Whether to use plain HTTP (proxy mode) or HTTPS (direct).
    pub use_tls: bool,
}

impl ClaudeConfig {
    /// Default config for QEMU with a local TLS-terminating proxy on port 8080.
    pub fn default_proxy() -> Self {
        Self {
            api_key: String::from(""), // Must be set by user
            target_ip: Ipv4Address::new(10, 0, 2, 2), // QEMU host
            target_port: 8080,
            model: String::from("claude-sonnet-4-5-20250929"),
            use_tls: false,
        }
    }
}

/// Send a message to Claude and stream the response.
///
/// Returns the complete response text, while also calling `on_token`
/// for each chunk received (for real-time display on serial console).
pub fn claude_request<F>(
    net: &mut NetStack,
    config: &ClaudeConfig,
    prompt: &str,
    on_token: F,
) -> Result<String, ApiError>
where
    F: Fn(&str),
{
    // 1. Build the HTTP request
    let body = format!(
        r#"{{"model":"{}","max_tokens":1024,"stream":true,"messages":[{{"role":"user","content":"{}"}}]}}"#,
        config.model,
        escape_json(prompt),
    );

    let request = format!(
        "POST /v1/messages HTTP/1.1\r\n\
         Host: api.anthropic.com\r\n\
         Content-Type: application/json\r\n\
         X-API-Key: {}\r\n\
         Anthropic-Version: 2023-06-01\r\n\
         Accept: text/event-stream\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {}",
        config.api_key,
        body.len(),
        body,
    );

    // 2. Connect
    let handle = net.tcp_connect(config.target_ip, config.target_port)
        .ok_or(ApiError::ConnectionFailed)?;

    // Wait for connection
    let connected = net.poll_until(|n| n.tcp_can_send(handle), 10_000);
    if !connected {
        return Err(ApiError::ConnectionTimeout);
    }

    // 3. Send request
    let request_bytes = request.as_bytes();
    let mut sent = 0;
    while sent < request_bytes.len() {
        net.poll();
        if net.tcp_can_send(handle) {
            let n = net.tcp_send(handle, &request_bytes[sent..]);
            sent += n;
        }
        core::hint::spin_loop();
    }

    // 4. Receive response — parse SSE stream for content_block_delta events
    let mut response = String::new();
    let mut raw_buf = Vec::new();
    let mut recv_buf = [0u8; 4096];

    loop {
        net.poll();

        if net.tcp_can_recv(handle) {
            let n = net.tcp_recv(handle, &mut recv_buf);
            if n > 0 {
                raw_buf.extend_from_slice(&recv_buf[..n]);

                // Try to parse SSE events from the buffer
                while let Some(event_end) = find_sse_event_end(&raw_buf) {
                    let event_bytes = raw_buf[..event_end].to_vec();
                    raw_buf = raw_buf[event_end..].to_vec();

                    if let Some(text) = extract_content_delta(&event_bytes) {
                        on_token(&text);
                        response.push_str(&text);
                    }

                    // Check for message_stop event
                    if is_message_stop(&event_bytes) {
                        net.tcp_close(handle);
                        return Ok(response);
                    }
                }
            }
        }

        if !net.tcp_is_active(handle) && !net.tcp_can_recv(handle) {
            break;
        }

        core::hint::spin_loop();
    }

    net.tcp_close(handle);

    if response.is_empty() {
        // Try to extract error from raw response
        let raw = String::from_utf8_lossy(&raw_buf).into_owned();
        if raw.contains("error") {
            Err(ApiError::ApiError(raw))
        } else if raw.is_empty() {
            Err(ApiError::EmptyResponse)
        } else {
            // Non-streaming response — extract content directly
            if let Some(text) = extract_non_streaming_content(&raw) {
                on_token(&text);
                Ok(text)
            } else {
                Ok(raw)
            }
        }
    } else {
        Ok(response)
    }
}

/// Find the end of an SSE event (delimited by double newline).
fn find_sse_event_end(buf: &[u8]) -> Option<usize> {
    for i in 0..buf.len().saturating_sub(1) {
        if buf[i] == b'\n' && buf[i + 1] == b'\n' {
            return Some(i + 2);
        }
    }
    None
}

/// Extract text content from an SSE content_block_delta event.
///
/// SSE format:
/// ```
/// event: content_block_delta
/// data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}
/// ```
fn extract_content_delta(event: &[u8]) -> Option<String> {
    let s = core::str::from_utf8(event).ok()?;

    if !s.contains("content_block_delta") {
        return None;
    }

    // Find "text":" in the delta object
    let marker = r#""text":""#;
    let start = s.find(marker)? + marker.len();
    let rest = &s[start..];

    // Find the closing quote (handle escaped quotes)
    let mut end = 0;
    let bytes = rest.as_bytes();
    while end < bytes.len() {
        if bytes[end] == b'"' && (end == 0 || bytes[end - 1] != b'\\') {
            break;
        }
        end += 1;
    }

    let text = &rest[..end];
    Some(unescape_json(text))
}

/// Check if this SSE event is a message_stop.
fn is_message_stop(event: &[u8]) -> bool {
    let s = core::str::from_utf8(event).unwrap_or("");
    s.contains("message_stop")
}

/// Extract content from a non-streaming JSON response.
fn extract_non_streaming_content(raw: &str) -> Option<String> {
    // Look for "text":"..." in the content array
    let marker = r#""text":""#;
    let start = raw.find(marker)? + marker.len();
    let rest = &raw[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Escape a string for JSON embedding.
fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str(r#"\""#),
            '\\' => out.push_str(r#"\\"#),
            '\n' => out.push_str(r#"\n"#),
            '\r' => out.push_str(r#"\r"#),
            '\t' => out.push_str(r#"\t"#),
            c => out.push(c),
        }
    }
    out
}

/// Unescape a JSON string value.
fn unescape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some(c) => { out.push('\\'); out.push(c); }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// API client errors.
#[derive(Debug)]
pub enum ApiError {
    ConnectionFailed,
    ConnectionTimeout,
    SendFailed,
    EmptyResponse,
    ApiError(String),
}

impl core::fmt::Display for ApiError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ApiError::ConnectionFailed => write!(f, "TCP connection failed"),
            ApiError::ConnectionTimeout => write!(f, "connection timeout"),
            ApiError::SendFailed => write!(f, "failed to send request"),
            ApiError::EmptyResponse => write!(f, "empty response from API"),
            ApiError::ApiError(msg) => write!(f, "API error: {}", msg),
        }
    }
}

// ---- Static API key storage ----
// In production, this would be stored encrypted in SQLite.
// For development, set it via the shell: `apikey sk-ant-...`

use spin::Mutex;
static API_KEY: Mutex<Option<String>> = Mutex::new(None);

pub fn set_api_key(key: &str) {
    *API_KEY.lock() = Some(String::from(key));
}

pub fn get_api_key() -> Option<String> {
    API_KEY.lock().clone()
}
