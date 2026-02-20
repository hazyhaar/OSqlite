/// Agentic loop — multi-turn tool-use conversation with Claude.
///
/// Sends a prompt with tool definitions, executes tool calls locally,
/// feeds results back, and repeats until Claude produces a final text
/// response or the turn limit is reached.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::api::{self, ClaudeConfig, ClaudeRequest, ContentBlock, Message};
use crate::net::NetStack;
use crate::{serial_print, serial_println};

/// Maximum number of agentic turns before stopping.
const MAX_TURNS: usize = 20;

/// System prompt for the agentic loop.
const AGENT_SYSTEM: &str = "\
You are an AI assistant running inside OSqlite, a bare-metal OS with an embedded SQLite database. \
You have tools to read/write files in the namespace, execute SQL queries, and list directories. \
Use tools to inspect and modify the system as needed. Be concise in your responses.";

/// Run the agentic loop for a user prompt.
/// Returns the final text response.
pub fn run_agent_loop(prompt: &str, use_tls: bool) -> Result<String, String> {
    // Check API key
    let api_key = api::get_api_key()
        .ok_or_else(|| String::from("API key not set. Run: apikey sk-ant-..."))?;

    // Acquire network stack
    let mut net_guard = crate::net::NET_STACK.lock();
    let net = net_guard.as_mut()
        .ok_or_else(|| String::from("network stack not initialized"))?;

    // Resolve target IP
    let (_target_ip, config_base) = if use_tls {
        let ip = resolve_api_ip(net)?;
        serial_println!("[TLS to {}:443...]", ip);
        (ip, ClaudeConfig::direct_tls(ip))
    } else {
        serial_println!("[proxy mode: 10.0.2.2:8080...]");
        let cfg = ClaudeConfig::default_proxy();
        (cfg.target_ip, cfg)
    };

    let config = ClaudeConfig {
        api_key,
        model: api::get_model(),
        ..config_base
    };

    // Initialize conversation
    let mut messages: Vec<Message> = Vec::new();
    messages.push(Message::text("user", String::from(prompt)));

    let mut final_text = String::new();

    for _turn in 0..MAX_TURNS {
        serial_println!();

        let request = ClaudeRequest {
            config: ClaudeConfig {
                api_key: config.api_key.clone(),
                model: config.model.clone(),
                target_ip: config.target_ip,
                target_port: config.target_port,
                use_tls: config.use_tls,
            },
            system: Some(String::from(AGENT_SYSTEM)),
            messages: clone_messages(&messages),
            use_tools: true,
        };

        let response = api::claude_request_agentic(net, &request, |token| {
            serial_print!("{}", token);
        }).map_err(|e| format!("API error: {}", e))?;

        if response.tool_calls.is_empty() {
            // Final text response — done
            final_text = response.text;
            serial_println!();
            return Ok(final_text);
        }

        // We have tool calls — execute them
        // First, record the assistant's response in conversation history
        messages.push(Message::assistant_tool_use(
            response.text.clone(),
            response.tool_calls.clone(),
        ));

        // Execute each tool call and build tool_result messages
        let mut result_blocks: Vec<ContentBlock> = Vec::new();
        for tc in &response.tool_calls {
            serial_println!();
            serial_println!("[tool] {} ...", tc.name);

            let (result, is_error) = dispatch_tool(&tc.name, &tc.input_json);

            // Truncate display for long results
            let display = if result.len() > 200 {
                format!("{}... ({} bytes)", &result[..200], result.len())
            } else {
                result.clone()
            };
            if is_error {
                serial_println!("[tool] ERROR: {}", display);
            } else {
                serial_println!("[tool] -> {}", display);
            }

            result_blocks.push(ContentBlock::ToolResult {
                tool_use_id: tc.id.clone(),
                content: result,
                is_error,
            });
        }

        // Add all tool results as a single user message
        messages.push(Message {
            role: "user",
            content: String::new(),
            content_blocks: result_blocks,
        });
    }

    serial_println!();
    serial_println!("[agent] Turn limit ({}) reached", MAX_TURNS);
    Ok(final_text)
}

/// Dispatch a tool call to the appropriate handler.
/// Returns (result_string, is_error).
fn dispatch_tool(name: &str, input_json: &str) -> (String, bool) {
    // Parse the input JSON
    let input = match api::json::parse(input_json) {
        Ok(v) => v,
        Err(e) => return (format!("Invalid tool input JSON: {}", e), true),
    };

    match name {
        "read_file" => tool_read_file(&input),
        "write_file" => tool_write_file(&input),
        "sql_query" => tool_sql_query(&input),
        "list_dir" => tool_list_dir(&input),
        "str_replace" => tool_str_replace(&input),
        _ => (format!("Unknown tool: {}", name), true),
    }
}

fn tool_read_file(input: &api::json::JsonValue) -> (String, bool) {
    let path = match input.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return (String::from("missing 'path' parameter"), true),
    };

    let guard = crate::sqlite::DB.lock();
    let db = match guard.as_ref() {
        Some(db) => db,
        None => return (String::from("database not open"), true),
    };

    let query = format!(
        "SELECT content FROM namespace WHERE path='{}'",
        path.replace('\'', "''")
    );

    match db.query_value(&query) {
        Ok(Some(content)) => (content, false),
        Ok(None) => (format!("file not found: {}", path), true),
        Err(e) => (format!("read error: {}", e), true),
    }
}

fn tool_write_file(input: &api::json::JsonValue) -> (String, bool) {
    let path = match input.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return (String::from("missing 'path' parameter"), true),
    };
    let content = match input.get("content").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return (String::from("missing 'content' parameter"), true),
    };

    let guard = crate::sqlite::DB.lock();
    let db = match guard.as_ref() {
        Some(db) => db,
        None => return (String::from("database not open"), true),
    };

    let query = format!(
        "INSERT OR REPLACE INTO namespace (path, type, content, mtime) \
         VALUES ('{}', 'data', '{}', strftime('%s','now'))",
        path.replace('\'', "''"),
        content.replace('\'', "''")
    );

    match db.exec(&query) {
        Ok(()) => (format!("wrote {} bytes to {}", content.len(), path), false),
        Err(e) => (format!("write error: {}", e), true),
    }
}

fn tool_sql_query(input: &api::json::JsonValue) -> (String, bool) {
    let query = match input.get("query").and_then(|v| v.as_str()) {
        Some(q) => q,
        None => return (String::from("missing 'query' parameter"), true),
    };

    // Read-only: only SELECT, EXPLAIN, PRAGMA
    let trimmed = query.trim_start().as_bytes();
    let allowed = starts_with_ic(trimmed, b"SELECT")
        || starts_with_ic(trimmed, b"EXPLAIN")
        || starts_with_ic(trimmed, b"PRAGMA");
    if !allowed {
        return (String::from("only SELECT/EXPLAIN/PRAGMA allowed"), true);
    }

    match crate::sqlite::exec_and_format(query) {
        Ok(output) => (output, false),
        Err(e) => (format!("SQL error: {}", e), true),
    }
}

fn tool_list_dir(input: &api::json::JsonValue) -> (String, bool) {
    let path = match input.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return (String::from("missing 'path' parameter"), true),
    };

    let prefix = if path.ends_with('/') {
        String::from(path)
    } else {
        format!("{}/", path)
    };

    let guard = crate::sqlite::DB.lock();
    let db = match guard.as_ref() {
        Some(db) => db,
        None => return (String::from("database not open"), true),
    };

    let query = format!(
        "SELECT path FROM namespace WHERE substr(path, 1, {}) = '{}' ORDER BY path",
        prefix.len(),
        prefix.replace('\'', "''")
    );

    match db.query_column(&query) {
        Ok(paths) => {
            if paths.is_empty() {
                (format!("no entries under {}", path), false)
            } else {
                (paths.join("\n"), false)
            }
        }
        Err(e) => (format!("list error: {}", e), true),
    }
}

fn tool_str_replace(input: &api::json::JsonValue) -> (String, bool) {
    let path = match input.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return (String::from("missing 'path' parameter"), true),
    };
    let old_str = match input.get("old_str").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return (String::from("missing 'old_str' parameter"), true),
    };
    let new_str = match input.get("new_str").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return (String::from("missing 'new_str' parameter"), true),
    };

    // Read current content
    let guard = crate::sqlite::DB.lock();
    let db = match guard.as_ref() {
        Some(db) => db,
        None => return (String::from("database not open"), true),
    };

    let read_query = format!(
        "SELECT content FROM namespace WHERE path='{}'",
        path.replace('\'', "''")
    );

    let content = match db.query_value(&read_query) {
        Ok(Some(c)) => c,
        Ok(None) => return (format!("file not found: {}", path), true),
        Err(e) => return (format!("read error: {}", e), true),
    };

    // Find and replace
    if !content.contains(old_str) {
        return (format!("old_str not found in {}", path), true);
    }

    let new_content = content.replacen(old_str, new_str, 1);

    let write_query = format!(
        "UPDATE namespace SET content='{}', mtime=strftime('%s','now') WHERE path='{}'",
        new_content.replace('\'', "''"),
        path.replace('\'', "''")
    );

    match db.exec(&write_query) {
        Ok(()) => (format!("replaced in {} ({} bytes -> {} bytes)", path, content.len(), new_content.len()), false),
        Err(e) => (format!("write error: {}", e), true),
    }
}

/// Case-insensitive prefix check.
fn starts_with_ic(haystack: &[u8], needle: &[u8]) -> bool {
    if haystack.len() < needle.len() {
        return false;
    }
    haystack[..needle.len()].iter()
        .zip(needle.iter())
        .all(|(h, n)| h.to_ascii_uppercase() == n.to_ascii_uppercase())
}

/// Clone messages for re-sending (needed because ClaudeRequest takes ownership).
fn clone_messages(messages: &[Message]) -> Vec<Message> {
    messages.iter().map(|m| Message {
        role: m.role,
        content: m.content.clone(),
        content_blocks: m.content_blocks.clone(),
    }).collect()
}

/// Resolve api.anthropic.com IP with DNS, checking manual override first.
fn resolve_api_ip(net: &mut NetStack) -> Result<smoltcp::wire::Ipv4Address, String> {
    use smoltcp::wire::Ipv4Address;

    // Check manual override
    let manual = *super::commands::API_TARGET_IP_ACCESSOR.lock();
    if manual != Ipv4Address::new(0, 0, 0, 0) {
        serial_println!("[resolve: {} (manual)]", manual);
        return Ok(manual);
    }

    serial_println!("[DNS resolve: api.anthropic.com...]");
    match crate::net::dns::resolve_a(net, "api.anthropic.com") {
        Ok(ip) => {
            serial_println!("[resolved: {}]", ip);
            Ok(ip)
        }
        Err(e) => Err(format!("DNS resolution failed: {}", e)),
    }
}
