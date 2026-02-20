/// Claude API client — calls Anthropic's Messages API from bare metal.
///
/// Supports two modes:
/// - **TLS mode** (`use_tls: true`): Direct HTTPS to api.anthropic.com:443
///   using `embedded-tls` for in-kernel TLS 1.3 (AES-128-GCM + P-256).
///   Certificate pinning via SPKI SHA-256 hash (RFC 7469).
///   DNS resolution via UDP to QEMU's forwarder (10.0.2.3).
///
/// - **Proxy mode** (`use_tls: false`): Plain HTTP to a local socat/nginx proxy
///   on the QEMU host that terminates TLS. Fallback for debugging.
pub mod http;
pub mod json;
pub mod tools;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use crate::net::NetStack;
use smoltcp::wire::Ipv4Address;

/// Whether to enforce SPKI pinning. Currently disabled because embedded-tls 0.18
/// marks CertificateRef.entries as pub(crate), preventing external certificate
/// inspection. See crypto/pin_verifier.rs for details and the pin management
/// infrastructure that's ready for when this limitation is resolved.
pub const ENFORCE_PINNING: bool = false;

// ---- Retry configuration ----

const MAX_RETRIES: u32 = 3;
const BASE_DELAY_MS: u64 = 1000;

// ---- Types ----

/// A single message in a conversation.
/// For simple text messages, `content` holds the text.
/// For tool_result messages, use `ContentBlock::ToolResult` via `content_blocks`.
pub struct Message {
    pub role: &'static str, // "user" | "assistant"
    pub content: String,
    /// Structured content blocks (used for tool_result and mixed responses).
    /// If non-empty, these are serialized instead of `content`.
    pub content_blocks: Vec<ContentBlock>,
}

impl Message {
    /// Create a simple text message.
    pub fn text(role: &'static str, content: String) -> Self {
        Self { role, content, content_blocks: Vec::new() }
    }

    /// Create a tool_result message.
    pub fn tool_result(tool_use_id: String, result: String, is_error: bool) -> Self {
        Self {
            role: "user",
            content: String::new(),
            content_blocks: vec![ContentBlock::ToolResult {
                tool_use_id,
                content: result,
                is_error,
            }],
        }
    }

    /// Create an assistant message with tool_use blocks (for conversation history).
    pub fn assistant_tool_use(text: String, tool_calls: Vec<ToolCall>) -> Self {
        let mut blocks = Vec::new();
        if !text.is_empty() {
            blocks.push(ContentBlock::Text(text));
        }
        for tc in tool_calls {
            blocks.push(ContentBlock::ToolUse {
                id: tc.id,
                name: tc.name,
                input_json: tc.input_json,
            });
        }
        Self {
            role: "assistant",
            content: String::new(),
            content_blocks: blocks,
        }
    }
}

/// A content block in a message — text, tool_use, or tool_result.
#[derive(Clone)]
pub enum ContentBlock {
    Text(String),
    ToolUse { id: String, name: String, input_json: String },
    ToolResult { tool_use_id: String, content: String, is_error: bool },
}

/// A tool call extracted from Claude's response.
#[derive(Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input_json: String,
}

/// Result of a Claude API request — may contain text and/or tool calls.
pub struct ClaudeResponse {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    /// "end_turn" or "tool_use" — indicates why the model stopped.
    pub stop_reason: String,
}

/// Full request parameters for the Claude API.
pub struct ClaudeRequest {
    pub config: ClaudeConfig,
    pub system: Option<String>,
    pub messages: Vec<Message>,
    /// Whether to include tool definitions in the request.
    pub use_tools: bool,
}

/// Claude API configuration.
pub struct ClaudeConfig {
    /// API key (sk-ant-...).
    pub api_key: String,
    /// Target IP address.
    /// TLS mode: IP of api.anthropic.com (resolved via DNS or manually).
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
            model: String::from("claude-sonnet-4-6-20250514"),
            use_tls: false,
        }
    }

    /// Config for direct HTTPS to api.anthropic.com via QEMU NAT.
    pub fn direct_tls(target_ip: Ipv4Address) -> Self {
        Self {
            api_key: String::from(""),
            target_ip,
            target_port: 443,
            model: String::from("claude-sonnet-4-6-20250514"),
            use_tls: true,
        }
    }
}

// ---- Request building ----

/// Build the HTTP request for a single-turn prompt (backward compat).
fn build_http_request(config: &ClaudeConfig, prompt: &str) -> Result<String, ApiError> {
    let messages = vec![Message::text("user", String::from(prompt))];
    build_http_request_multi(config, None, &messages, false)
}

/// Build the HTTP request for a multi-turn conversation with optional system prompt.
fn build_http_request_multi(
    config: &ClaudeConfig,
    system: Option<&str>,
    messages: &[Message],
    use_tools: bool,
) -> Result<String, ApiError> {
    // Validate inputs — reject CRLF to prevent header injection
    if config.model.contains('\r') || config.model.contains('\n') {
        return Err(ApiError::SendFailed);
    }
    if config.api_key.contains('\r') || config.api_key.contains('\n') {
        return Err(ApiError::SendFailed);
    }

    // Build messages JSON array
    let mut msgs_json = String::from("[");
    for (i, msg) in messages.iter().enumerate() {
        if i > 0 {
            msgs_json.push(',');
        }
        if msg.content_blocks.is_empty() {
            // Simple text message
            msgs_json.push_str(&format!(
                r#"{{"role":"{}","content":"{}"}}"#,
                escape_json(msg.role),
                escape_json(&msg.content),
            ));
        } else {
            // Structured content blocks
            msgs_json.push_str(&format!(r#"{{"role":"{}","content":["#, escape_json(msg.role)));
            for (j, block) in msg.content_blocks.iter().enumerate() {
                if j > 0 {
                    msgs_json.push(',');
                }
                match block {
                    ContentBlock::Text(text) => {
                        msgs_json.push_str(&format!(
                            r#"{{"type":"text","text":"{}"}}"#,
                            escape_json(text),
                        ));
                    }
                    ContentBlock::ToolUse { id, name, input_json } => {
                        msgs_json.push_str(&format!(
                            r#"{{"type":"tool_use","id":"{}","name":"{}","input":{}}}"#,
                            escape_json(id),
                            escape_json(name),
                            input_json,
                        ));
                    }
                    ContentBlock::ToolResult { tool_use_id, content, is_error } => {
                        if *is_error {
                            msgs_json.push_str(&format!(
                                r#"{{"type":"tool_result","tool_use_id":"{}","is_error":true,"content":"{}"}}"#,
                                escape_json(tool_use_id),
                                escape_json(content),
                            ));
                        } else {
                            msgs_json.push_str(&format!(
                                r#"{{"type":"tool_result","tool_use_id":"{}","content":"{}"}}"#,
                                escape_json(tool_use_id),
                                escape_json(content),
                            ));
                        }
                    }
                }
            }
            msgs_json.push_str("]}");
        }
    }
    msgs_json.push(']');

    // Build body
    let tools_part = if use_tools {
        format!(r#","tools":{}"#, tools::tools_json())
    } else {
        String::new()
    };

    let body = if let Some(sys) = system {
        format!(
            r#"{{"model":"{}","max_tokens":4096,"stream":true,"system":"{}","messages":{}{}}}"#,
            escape_json(&config.model),
            escape_json(sys),
            msgs_json,
            tools_part,
        )
    } else {
        format!(
            r#"{{"model":"{}","max_tokens":4096,"stream":true,"messages":{}{}}}"#,
            escape_json(&config.model),
            msgs_json,
            tools_part,
        )
    };

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

// ---- Public API ----

/// Send a single-turn message to Claude and stream the response.
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
    claude_send_with_retry(net, config, &request, on_token)
}

/// Send a multi-turn request to Claude (text-only response).
pub fn claude_request_multi<F>(
    net: &mut NetStack,
    request: &ClaudeRequest,
    on_token: F,
) -> Result<String, ApiError>
where
    F: Fn(&str),
{
    let http_req = build_http_request_multi(
        &request.config,
        request.system.as_deref(),
        &request.messages,
        request.use_tools,
    )?;
    claude_send_with_retry(net, &request.config, &http_req, on_token)
}

/// Send an agentic request to Claude — returns full response with tool calls.
pub fn claude_request_agentic<F>(
    net: &mut NetStack,
    request: &ClaudeRequest,
    on_token: F,
) -> Result<ClaudeResponse, ApiError>
where
    F: Fn(&str),
{
    let http_req = build_http_request_multi(
        &request.config,
        request.system.as_deref(),
        &request.messages,
        request.use_tools,
    )?;
    claude_send_agentic(net, &request.config, &http_req, on_token)
}

/// Send a request with retry logic.
fn claude_send_with_retry<F>(
    net: &mut NetStack,
    config: &ClaudeConfig,
    request: &str,
    on_token: F,
) -> Result<String, ApiError>
where
    F: Fn(&str),
{
    let mut last_err = ApiError::EmptyResponse;

    for attempt in 0..=MAX_RETRIES {
        if attempt > 0 {
            let delay_ms = BASE_DELAY_MS * (1u64 << (attempt - 1).min(4));
            crate::serial_println!("[API] Retry {}/{} after {}ms...", attempt, MAX_RETRIES, delay_ms);
            crate::arch::x86_64::timer::delay_us(delay_ms * 1000);
        }

        let result = if config.use_tls {
            claude_request_tls(net, config, request, &on_token)
        } else {
            claude_request_plain(net, config, request, &on_token)
        };

        match result {
            Ok(response) => return Ok(response),
            Err(ApiError::HttpStatus(status, ref msg, retry_after)) => {
                // Retry on server errors, not client errors
                if status == 429 || status == 500 || status == 529 {
                    // Honor Retry-After header if present (e.g. 429)
                    if let Some(secs) = retry_after {
                        let wait_ms = (secs * 1000).min(60_000);
                        crate::serial_println!("[API] Retry-After: {}s", secs);
                        crate::arch::x86_64::timer::delay_us(wait_ms * 1000);
                    }
                    last_err = ApiError::HttpStatus(status, msg.clone(), retry_after);
                    continue;
                }
                return Err(ApiError::HttpStatus(status, msg.clone(), retry_after));
            }
            Err(ApiError::ConnectionTimeout) | Err(ApiError::ConnectionFailed) => {
                last_err = ApiError::ConnectionTimeout;
                continue;
            }
            Err(e) => return Err(e),
        }
    }

    Err(last_err)
}

/// Agentic send — parses both text and tool_use blocks from SSE stream.
/// Currently TLS-only (agentic loop always uses direct HTTPS).
fn claude_send_agentic<F>(
    net: &mut NetStack,
    config: &ClaudeConfig,
    request: &str,
    on_token: F,
) -> Result<ClaudeResponse, ApiError>
where
    F: Fn(&str),
{
    let mut last_err = ApiError::EmptyResponse;

    for attempt in 0..=MAX_RETRIES {
        if attempt > 0 {
            let delay_ms = BASE_DELAY_MS * (1u64 << (attempt - 1).min(4));
            crate::serial_println!("[API] Retry {}/{} after {}ms...", attempt, MAX_RETRIES, delay_ms);
            crate::arch::x86_64::timer::delay_us(delay_ms * 1000);
        }

        let result = claude_request_tls_agentic(net, config, request, &on_token);

        match result {
            Ok(response) => return Ok(response),
            Err(ApiError::HttpStatus(status, ref msg, retry_after)) => {
                if status == 429 || status == 500 || status == 529 {
                    if let Some(secs) = retry_after {
                        let wait_ms = (secs * 1000).min(60_000);
                        crate::serial_println!("[API] Retry-After: {}s", secs);
                        crate::arch::x86_64::timer::delay_us(wait_ms * 1000);
                    }
                    last_err = ApiError::HttpStatus(status, msg.clone(), retry_after);
                    continue;
                }
                return Err(ApiError::HttpStatus(status, msg.clone(), retry_after));
            }
            Err(ApiError::ConnectionTimeout) | Err(ApiError::ConnectionFailed) => {
                last_err = ApiError::ConnectionTimeout;
                continue;
            }
            Err(e) => return Err(e),
        }
    }

    Err(last_err)
}

/// TLS agentic request — returns ClaudeResponse with text + tool calls.
fn claude_request_tls_agentic<F>(
    net: &mut NetStack,
    config: &ClaudeConfig,
    request: &str,
    on_token: &F,
) -> Result<ClaudeResponse, ApiError>
where
    F: Fn(&str),
{
    use crate::crypto::RdRandRng;
    use crate::net::tls::TcpStream;
    use embedded_tls::blocking::TlsConnection;
    use embedded_tls::{TlsConfig, TlsContext};

    let handle = net.tcp_connect(config.target_ip, config.target_port)
        .ok_or(ApiError::ConnectionFailed)?;

    let connected = net.poll_until(|n| n.tcp_can_send(handle), 10_000);
    if !connected {
        net.tcp_close(handle);
        return Err(ApiError::ConnectionTimeout);
    }

    let tcp = TcpStream::new(net, handle);

    let mut read_buf = vec![0u8; 16640];
    let mut write_buf = vec![0u8; 16640];

    let tls_config = TlsConfig::new()
        .with_server_name("api.anthropic.com")
        .enable_rsa_signatures();

    let mut tls = TlsConnection::new(tcp, &mut read_buf, &mut write_buf);
    let rng = RdRandRng::new();

    {
        use embedded_tls::{Aes128GcmSha256, UnsecureProvider};
        tls.open(TlsContext::new(
            &tls_config,
            UnsecureProvider::new::<Aes128GcmSha256>(rng),
        )).map_err(|e| {
            crate::serial_println!("[TLS] Handshake failed: {:?}", e);
            ApiError::TlsHandshakeFailed
        })?;
    }

    // Send request
    let request_bytes = request.as_bytes();
    let mut sent = 0;
    while sent < request_bytes.len() {
        let n = tls.write(&request_bytes[sent..]).map_err(|_| ApiError::SendFailed)?;
        sent += n;
    }
    tls.flush().map_err(|_| ApiError::SendFailed)?;

    // Parse SSE stream with tool_use support
    let mut text_response = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut stop_reason = String::from("end_turn");

    // State for accumulating tool_use blocks
    let mut current_tool_id = String::new();
    let mut current_tool_name = String::new();
    let mut current_tool_input = String::new();

    let mut raw_buf = Vec::new();
    let mut recv_buf = [0u8; 4096];
    let mut headers_parsed = false;

    loop {
        match tls.read(&mut recv_buf) {
            Ok(0) => break,
            Ok(n) => {
                raw_buf.extend_from_slice(&recv_buf[..n]);

                if !headers_parsed {
                    if let Ok(resp) = http::HttpResponse::parse(&raw_buf) {
                        headers_parsed = true;
                        if let Some(err_msg) = resp.error_message() {
                            let retry = resp.retry_after_secs();
                            let _ = tls.close();
                            return Err(ApiError::HttpStatus(resp.status, String::from(err_msg), retry));
                        }
                        raw_buf = raw_buf[resp.body_start..].to_vec();
                    }
                }

                if headers_parsed {
                    while let Some(event_end) = find_sse_event_end(&raw_buf) {
                        let event_bytes = raw_buf[..event_end].to_vec();
                        raw_buf = raw_buf[event_end..].to_vec();

                        let event_str = match core::str::from_utf8(&event_bytes) {
                            Ok(s) => s,
                            Err(_) => continue,
                        };

                        let data = match extract_sse_data(event_str) {
                            Some(d) => d,
                            None => continue,
                        };

                        // Parse the SSE data JSON
                        if let Ok(parsed) = json::parse(data) {
                            let event_type = parsed.get("type")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");

                            match event_type {
                                "content_block_start" => {
                                    // Check if this is a tool_use block
                                    if let Some(cb) = parsed.get("content_block") {
                                        let cb_type = cb.get("type")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("");
                                        if cb_type == "tool_use" {
                                            current_tool_id = cb.get("id")
                                                .and_then(|v| v.as_str())
                                                .map(String::from)
                                                .unwrap_or_default();
                                            current_tool_name = cb.get("name")
                                                .and_then(|v| v.as_str())
                                                .map(String::from)
                                                .unwrap_or_default();
                                            current_tool_input.clear();
                                        }
                                    }
                                }
                                "content_block_delta" => {
                                    if let Some(delta) = parsed.get("delta") {
                                        let delta_type = delta.get("type")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("");
                                        match delta_type {
                                            "text_delta" => {
                                                if let Some(text) = delta.get("text").and_then(|v| v.as_str()) {
                                                    on_token(text);
                                                    text_response.push_str(text);
                                                }
                                            }
                                            "input_json_delta" => {
                                                if let Some(pj) = delta.get("partial_json").and_then(|v| v.as_str()) {
                                                    current_tool_input.push_str(pj);
                                                }
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                                "content_block_stop" => {
                                    // If we were accumulating a tool_use, finalize it
                                    if !current_tool_id.is_empty() {
                                        tool_calls.push(ToolCall {
                                            id: core::mem::take(&mut current_tool_id),
                                            name: core::mem::take(&mut current_tool_name),
                                            input_json: core::mem::take(&mut current_tool_input),
                                        });
                                    }
                                }
                                "message_delta" => {
                                    // Extract stop_reason
                                    if let Some(delta) = parsed.get("delta") {
                                        if let Some(sr) = delta.get("stop_reason").and_then(|v| v.as_str()) {
                                            stop_reason = String::from(sr);
                                        }
                                    }
                                }
                                "message_stop" => {
                                    let _ = tls.close();
                                    return Ok(ClaudeResponse {
                                        text: text_response,
                                        tool_calls,
                                        stop_reason,
                                    });
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
            Err(_) => break,
        }
    }

    let _ = tls.close();

    if text_response.is_empty() && tool_calls.is_empty() {
        Err(ApiError::EmptyResponse)
    } else {
        Ok(ClaudeResponse {
            text: text_response,
            tool_calls,
            stop_reason,
        })
    }
}

/// TLS path — direct HTTPS using embedded-tls with SPKI pinning.
fn claude_request_tls<F>(
    net: &mut NetStack,
    config: &ClaudeConfig,
    request: &str,
    on_token: &F,
) -> Result<String, ApiError>
where
    F: Fn(&str),
{
    use crate::crypto::RdRandRng;
    use crate::net::tls::TcpStream;
    use embedded_tls::blocking::TlsConnection;
    use embedded_tls::{TlsConfig, TlsContext};

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

    // 3. TLS handshake — with SPKI pin verification if enabled
    let mut read_buf = vec![0u8; 16640];
    let mut write_buf = vec![0u8; 16640];

    let tls_config = TlsConfig::new()
        .with_server_name("api.anthropic.com")
        .enable_rsa_signatures();

    let mut tls = TlsConnection::new(tcp, &mut read_buf, &mut write_buf);

    let rng = RdRandRng::new();

    // NOTE: SPKI pin verification is not yet possible because embedded-tls 0.18
    // marks CertificateRef.entries as pub(crate), preventing external code from
    // inspecting the server certificate. See crypto/pin_verifier.rs for details.
    // When this limitation is resolved, ENFORCE_PINNING will enable the pin check.
    {
        use embedded_tls::{Aes128GcmSha256, UnsecureProvider};
        if !ENFORCE_PINNING {
            crate::serial_println!(
                "[SECURITY WARNING] TLS without certificate pinning — \
                 API key may be exposed to MITM attacks"
            );
        }
        tls.open(TlsContext::new(
            &tls_config,
            UnsecureProvider::new::<Aes128GcmSha256>(rng),
        )).map_err(|e| {
            crate::serial_println!("[TLS] Handshake failed: {:?}", e);
            ApiError::TlsHandshakeFailed
        })?;
    }

    // 4. Send HTTP request over TLS
    let request_bytes = request.as_bytes();
    let mut sent = 0;
    while sent < request_bytes.len() {
        let chunk = &request_bytes[sent..];
        let n = tls.write(chunk).map_err(|_| ApiError::SendFailed)?;
        sent += n;
    }
    tls.flush().map_err(|_| ApiError::SendFailed)?;

    // 5. Receive + parse response over TLS
    let mut response = String::new();
    let mut raw_buf = Vec::new();
    let mut recv_buf = [0u8; 4096];
    let mut headers_parsed = false;

    loop {
        match tls.read(&mut recv_buf) {
            Ok(0) => break, // EOF
            Ok(n) => {
                raw_buf.extend_from_slice(&recv_buf[..n]);

                // Parse HTTP headers once we have them
                if !headers_parsed {
                    if let Ok(resp) = http::HttpResponse::parse(&raw_buf) {
                        headers_parsed = true;
                        if let Some(err_msg) = resp.error_message() {
                            let retry = resp.retry_after_secs();
                            let _ = tls.close();
                            return Err(ApiError::HttpStatus(resp.status, String::from(err_msg), retry));
                        }
                        // Strip headers from buffer, keep body
                        raw_buf = raw_buf[resp.body_start..].to_vec();
                    }
                }

                // Parse SSE events from body
                if headers_parsed {
                    while let Some(event_end) = find_sse_event_end(&raw_buf) {
                        let event_bytes = raw_buf[..event_end].to_vec();
                        raw_buf = raw_buf[event_end..].to_vec();

                        if let Some(text) = extract_content_delta_json(&event_bytes) {
                            on_token(&text);
                            response.push_str(&text);
                        }

                        if is_message_stop(&event_bytes) {
                            let _ = tls.close();
                            return Ok(response);
                        }
                    }
                }
            }
            Err(_) => break,
        }
    }

    let _ = tls.close();
    finish_response(response, raw_buf, on_token)
}

/// Plain HTTP path — for proxy mode.
fn claude_request_plain<F>(
    net: &mut NetStack,
    config: &ClaudeConfig,
    request: &str,
    on_token: &F,
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
    let mut headers_parsed = false;

    loop {
        net.poll();

        if net.tcp_can_recv(handle) {
            let n = net.tcp_recv(handle, &mut recv_buf);
            if n > 0 {
                raw_buf.extend_from_slice(&recv_buf[..n]);

                // Parse HTTP headers
                if !headers_parsed {
                    if let Ok(resp) = http::HttpResponse::parse(&raw_buf) {
                        headers_parsed = true;
                        if let Some(err_msg) = resp.error_message() {
                            let retry = resp.retry_after_secs();
                            net.tcp_close(handle);
                            return Err(ApiError::HttpStatus(resp.status, String::from(err_msg), retry));
                        }
                        raw_buf = raw_buf[resp.body_start..].to_vec();
                    }
                }

                if headers_parsed {
                    while let Some(event_end) = find_sse_event_end(&raw_buf) {
                        let event_bytes = raw_buf[..event_end].to_vec();
                        raw_buf = raw_buf[event_end..].to_vec();

                        if let Some(text) = extract_content_delta_json(&event_bytes) {
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
        }

        if !net.tcp_is_active(handle) && !net.tcp_can_recv(handle) {
            break;
        }

        core::hint::spin_loop();
    }

    net.tcp_close(handle);
    finish_response(response, raw_buf, on_token)
}

/// Handle response completion — extract content from non-streaming or error responses.
fn finish_response<F: Fn(&str)>(
    response: String,
    raw_buf: Vec<u8>,
    on_token: &F,
) -> Result<String, ApiError> {
    if response.is_empty() {
        let raw = String::from_utf8_lossy(&raw_buf).into_owned();
        if raw.is_empty() {
            return Err(ApiError::EmptyResponse);
        }
        // Try to parse as JSON error response
        if let Ok(parsed) = json::parse(&raw) {
            if let Some(err_obj) = parsed.get("error") {
                let msg = err_obj.get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown error");
                return Err(ApiError::ApiError(String::from(msg)));
            }
            // Try to extract content from non-streaming response
            if let Some(content) = parsed.get("content") {
                if let Some(arr) = content.as_array() {
                    for block in arr {
                        if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                            on_token(text);
                            return Ok(String::from(text));
                        }
                    }
                }
            }
        }
        // Fallback: return raw
        Ok(raw)
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

/// Extract text content from an SSE content_block_delta event using JSON parsing.
fn extract_content_delta_json(event: &[u8]) -> Option<String> {
    let s = core::str::from_utf8(event).ok()?;

    // SSE format: "event: content_block_delta\ndata: {...}\n"
    // Extract the data line
    let data = extract_sse_data(s)?;

    if !data.contains("content_block_delta") {
        return None;
    }

    // Parse the JSON
    if let Ok(parsed) = json::parse(data) {
        if let Some(delta) = parsed.get("delta") {
            return delta.get("text").and_then(|v| v.as_str()).map(String::from);
        }
    }

    // Fallback to string scanning if JSON parse fails
    extract_content_delta_legacy(s)
}

/// Extract the `data:` payload from an SSE event.
fn extract_sse_data(event: &str) -> Option<&str> {
    for line in event.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            return Some(rest.trim_start());
        }
        // Also handle "data: " with space
        if let Some(rest) = line.strip_prefix("data: ") {
            return Some(rest);
        }
    }
    // If no explicit "data:" prefix, the whole thing might be raw JSON
    let trimmed = event.trim();
    if trimmed.starts_with('{') {
        return Some(trimmed);
    }
    None
}

/// Legacy string-scanning SSE extractor (fallback).
fn extract_content_delta_legacy(s: &str) -> Option<String> {
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

// ---- JSON helpers ----

pub fn escape_json(s: &str) -> String {
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
    DnsError(String),
    /// HTTP error with status code, human-readable message, and optional retry-after (secs).
    HttpStatus(u16, String, Option<u64>),
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
            ApiError::DnsError(msg) => write!(f, "DNS error: {}", msg),
            ApiError::HttpStatus(code, msg, _) => write!(f, "HTTP {}: {}", code, msg),
            ApiError::ApiError(msg) => write!(f, "API error: {}", msg),
        }
    }
}

// ---- Static API key storage ----

use spin::Mutex;
static API_KEY: Mutex<Option<String>> = Mutex::new(None);
static MODEL: Mutex<Option<String>> = Mutex::new(None);

pub fn set_api_key(key: &str) {
    *API_KEY.lock() = Some(String::from(key));
}

pub fn get_api_key() -> Option<String> {
    API_KEY.lock().clone()
}

pub fn set_model(model: &str) {
    *MODEL.lock() = Some(String::from(model));
}

pub fn get_model() -> String {
    MODEL.lock().clone().unwrap_or_else(|| String::from("claude-sonnet-4-6-20250514"))
}
