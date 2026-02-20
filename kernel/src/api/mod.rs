/// Claude API client — calls Anthropic's Messages API from bare metal.
///
/// Supports two modes:
/// - **TLS mode** (`use_tls: true`): Direct HTTPS to api.anthropic.com:443
///   using `embedded-tls` for in-kernel TLS 1.3 (AES-128-GCM + P-256).
///   Requires QEMU user-mode networking (NAT to internet).
///
/// - **Proxy mode** (`use_tls: false`): Plain HTTP to a local socat/nginx proxy
///   on the QEMU host that terminates TLS. Fallback for debugging.
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use crate::net::NetStack;
use smoltcp::wire::Ipv4Address;

/// Claude API configuration.
pub struct ClaudeConfig {
    /// API key (sk-ant-...).
    pub api_key: String,
    /// Target IP address.
    /// TLS mode: IP of api.anthropic.com (resolved externally or hardcoded).
    /// Proxy mode: QEMU host (10.0.2.2).
    pub target_ip: Ipv4Address,
    pub target_port: u16,
    /// Model to use.
    pub model: String,
    /// Whether to use TLS (direct HTTPS) or plain HTTP (proxy mode).
    pub use_tls: bool,
}

impl ClaudeConfig {
    /// Default config for QEMU with a local TLS-terminating proxy on port 8080.
    pub fn default_proxy() -> Self {
        Self {
            api_key: String::from(""),
            target_ip: Ipv4Address::new(10, 0, 2, 2),
            target_port: 8080,
            model: String::from("claude-sonnet-4-5-20250929"),
            use_tls: false,
        }
    }

    /// Config for direct HTTPS to api.anthropic.com via QEMU NAT.
    /// Uses the QEMU gateway (10.0.2.2) as the DNS forwarder isn't
    /// implemented yet, so the caller must provide the resolved IP.
    pub fn direct_tls(target_ip: Ipv4Address) -> Self {
        Self {
            api_key: String::from(""),
            target_ip,
            target_port: 443,
            model: String::from("claude-sonnet-4-5-20250929"),
            use_tls: true,
        }
    }
}

/// Build the HTTP request body + headers.
fn build_http_request(config: &ClaudeConfig, prompt: &str) -> Result<String, ApiError> {
    // Validate inputs — reject CRLF to prevent header injection
    if config.model.contains('\r') || config.model.contains('\n') {
        return Err(ApiError::SendFailed);
    }
    if config.api_key.contains('\r') || config.api_key.contains('\n') {
        return Err(ApiError::SendFailed);
    }

    let body = format!(
        r#"{{"model":"{}","max_tokens":1024,"stream":true,"messages":[{{"role":"user","content":"{}"}}]}}"#,
        escape_json(&config.model),
        escape_json(prompt),
    );

    Ok(format!(
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
    ))
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
    let request = build_http_request(config, prompt)?;

    if config.use_tls {
        claude_request_tls(net, config, &request, on_token)
    } else {
        claude_request_plain(net, config, &request, on_token)
    }
}

/// TLS path — direct HTTPS using embedded-tls.
///
/// SECURITY WARNING: Certificate verification is NOT available in no_std
/// environments with embedded-tls. The `UnsecureProvider` skips all
/// certificate validation. A network MITM can intercept the API key and
/// all request/response data. Only use this over trusted networks
/// (e.g., QEMU NAT to localhost). See AUDIT.md finding #1.
fn claude_request_tls<F>(
    net: &mut NetStack,
    config: &ClaudeConfig,
    request: &str,
    on_token: F,
) -> Result<String, ApiError>
where
    F: Fn(&str),
{
    use crate::crypto::RdRandRng;
    use crate::net::tls::TcpStream;
    use embedded_tls::blocking::TlsConnection;
    use embedded_tls::{Aes128GcmSha256, TlsConfig, TlsContext, UnsecureProvider};

    // SECURITY: Log a warning every time we use unverified TLS
    crate::serial_println!(
        "[SECURITY WARNING] TLS connection without certificate verification — \
         API key may be exposed to MITM attacks"
    );

    // 1. TCP connect + wait for established
    let handle = net.tcp_connect(config.target_ip, config.target_port)
        .ok_or(ApiError::ConnectionFailed)?;

    let connected = net.poll_until(|n| n.tcp_can_send(handle), 10_000);
    if !connected {
        net.tcp_close(handle);
        return Err(ApiError::ConnectionTimeout);
    }

    // 2. Wrap in embedded-io adapter
    let tcp = TcpStream::new(net, handle);

    // 3. TLS handshake
    // NOTE: embedded-tls requires the `webpki` + `std` features for
    // certificate verification (CertVerifier). In no_std bare-metal, only
    // UnsecureProvider is available. When embedded-tls adds no_std cert
    // verification, replace UnsecureProvider with a pinning verifier that
    // checks the SHA-256 fingerprint of api.anthropic.com's certificate.
    let mut read_buf = vec![0u8; 16640];
    let mut write_buf = vec![0u8; 16640];

    let tls_config = TlsConfig::new()
        .with_server_name("api.anthropic.com")
        .enable_rsa_signatures();

    let mut tls = TlsConnection::new(tcp, &mut read_buf, &mut write_buf);

    let rng = RdRandRng::new();
    tls.open(TlsContext::new(
        &tls_config,
        UnsecureProvider::new::<Aes128GcmSha256>(rng),
    )).map_err(|_| ApiError::TlsHandshakeFailed)?;

    // 4. Send HTTP request over TLS
    let request_bytes = request.as_bytes();
    let mut sent = 0;
    while sent < request_bytes.len() {
        let chunk = &request_bytes[sent..];
        let n = tls.write(chunk).map_err(|_| ApiError::SendFailed)?;
        sent += n;
    }
    tls.flush().map_err(|_| ApiError::SendFailed)?;

    // 5. Receive + parse SSE response over TLS
    let mut response = String::new();
    let mut raw_buf = Vec::new();
    let mut recv_buf = [0u8; 4096];

    loop {
        match tls.read(&mut recv_buf) {
            Ok(0) => break, // EOF
            Ok(n) => {
                raw_buf.extend_from_slice(&recv_buf[..n]);

                while let Some(event_end) = find_sse_event_end(&raw_buf) {
                    let event_bytes = raw_buf[..event_end].to_vec();
                    raw_buf = raw_buf[event_end..].to_vec();

                    if let Some(text) = extract_content_delta(&event_bytes) {
                        on_token(&text);
                        response.push_str(&text);
                    }

                    if is_message_stop(&event_bytes) {
                        let _ = tls.close();
                        return Ok(response);
                    }
                }
            }
            Err(_) => break,
        }
    }

    let _ = tls.close();
    finish_response(response, raw_buf, &on_token)
}

/// Plain HTTP path — for proxy mode.
fn claude_request_plain<F>(
    net: &mut NetStack,
    config: &ClaudeConfig,
    request: &str,
    on_token: F,
) -> Result<String, ApiError>
where
    F: Fn(&str),
{
    let handle = net.tcp_connect(config.target_ip, config.target_port)
        .ok_or(ApiError::ConnectionFailed)?;

    let connected = net.poll_until(|n| n.tcp_can_send(handle), 10_000);
    if !connected {
        return Err(ApiError::ConnectionTimeout);
    }

    // Send request
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

    // Receive response — parse SSE stream
    let mut response = String::new();
    let mut raw_buf = Vec::new();
    let mut recv_buf = [0u8; 4096];

    loop {
        net.poll();

        if net.tcp_can_recv(handle) {
            let n = net.tcp_recv(handle, &mut recv_buf);
            if n > 0 {
                raw_buf.extend_from_slice(&recv_buf[..n]);

                while let Some(event_end) = find_sse_event_end(&raw_buf) {
                    let event_bytes = raw_buf[..event_end].to_vec();
                    raw_buf = raw_buf[event_end..].to_vec();

                    if let Some(text) = extract_content_delta(&event_bytes) {
                        on_token(&text);
                        response.push_str(&text);
                    }

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
    finish_response(response, raw_buf, &on_token)
}

/// Handle response completion — extract content from non-streaming or error responses.
fn finish_response<F: Fn(&str)>(
    response: String,
    raw_buf: Vec<u8>,
    on_token: &F,
) -> Result<String, ApiError> {
    if response.is_empty() {
        let raw = String::from_utf8_lossy(&raw_buf).into_owned();
        if raw.contains("error") {
            Err(ApiError::ApiError(raw))
        } else if raw.is_empty() {
            Err(ApiError::EmptyResponse)
        } else {
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

// ---- SSE parsing helpers ----

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
fn extract_content_delta(event: &[u8]) -> Option<String> {
    let s = core::str::from_utf8(event).ok()?;

    if !s.contains("content_block_delta") {
        return None;
    }

    let marker = r#""text":""#;
    let start = s.find(marker)? + marker.len();
    let rest = &s[start..];

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
    let marker = r#""text":""#;
    let start = raw.find(marker)? + marker.len();
    let rest = &raw[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

// ---- JSON helpers ----

fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let code = c as u32;
                out.push_str("\\u00");
                out.push(hex_digit((code >> 4) as u8));
                out.push(hex_digit((code & 0xF) as u8));
            }
            c => out.push(c),
        }
    }
    out
}

fn hex_digit(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        _ => (b'a' + n - 10) as char,
    }
}

fn unescape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('b') => out.push('\u{08}'),
                Some('f') => out.push('\u{0C}'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('/') => out.push('/'),
                Some('u') => {
                    let mut code = 0u32;
                    for _ in 0..4 {
                        let d = match chars.next() {
                            Some(h) => match h {
                                '0'..='9' => h as u32 - '0' as u32,
                                'a'..='f' => h as u32 - 'a' as u32 + 10,
                                'A'..='F' => h as u32 - 'A' as u32 + 10,
                                _ => 0,
                            },
                            None => 0,
                        };
                        code = (code << 4) | d;
                    }
                    if let Some(ch) = char::from_u32(code) {
                        out.push(ch);
                    }
                }
                Some(c) => { out.push('\\'); out.push(c); }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

// ---- Error types ----

/// API client errors.
#[derive(Debug)]
pub enum ApiError {
    ConnectionFailed,
    ConnectionTimeout,
    TlsHandshakeFailed,
    SendFailed,
    EmptyResponse,
    ApiError(String),
}

impl core::fmt::Display for ApiError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ApiError::ConnectionFailed => write!(f, "TCP connection failed"),
            ApiError::ConnectionTimeout => write!(f, "connection timeout"),
            ApiError::TlsHandshakeFailed => write!(f, "TLS handshake failed"),
            ApiError::SendFailed => write!(f, "failed to send request"),
            ApiError::EmptyResponse => write!(f, "empty response from API"),
            ApiError::ApiError(msg) => write!(f, "API error: {}", msg),
        }
    }
}

// ---- Static API key storage ----

use spin::Mutex;
static API_KEY: Mutex<Option<String>> = Mutex::new(None);

pub fn set_api_key(key: &str) {
    *API_KEY.lock() = Some(String::from(key));
}

pub fn get_api_key() -> Option<String> {
    API_KEY.lock().clone()
}
