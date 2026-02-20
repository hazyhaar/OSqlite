//! OSqlite builtin functions exposed to Lua scripts.
//!
//! sql(query, ...)    — execute SQL, return table of results
//! read(path)         — read from namespace → string or nil
//! write(path, data)  — write to namespace → boolean
//! ls(path)           — list namespace entries → table of strings
//! log(msg)           — write to serial console
//! sleep(ms)          — busy-wait using TSC
//! now()              — monotonic timestamp in ms
//! audit(level, action, detail) — write to audit table
//! ask(prompt) or ask(table)   — call Claude API → string

use alloc::vec;
use alloc::vec::Vec;
use core::ffi::{c_char, c_int};
use super::ffi::*;
use crate::sqlite::SqlValue;

/// Register all OSqlite builtins in a Lua state.
pub unsafe fn register_builtins(L: *mut LuaState) {
    lua_register(L, b"sql\0".as_ptr() as _, lua_sql);
    lua_register(L, b"read\0".as_ptr() as _, lua_read);
    lua_register(L, b"write\0".as_ptr() as _, lua_write);
    lua_register(L, b"ls\0".as_ptr() as _, lua_ls);
    lua_register(L, b"log\0".as_ptr() as _, lua_log);
    lua_register(L, b"sleep\0".as_ptr() as _, lua_sleep);
    lua_register(L, b"now\0".as_ptr() as _, lua_now);
    lua_register(L, b"audit\0".as_ptr() as _, lua_audit);
    lua_register(L, b"ask\0".as_ptr() as _, lua_ask);
}

// ============================================================
// sql(query, ...) → table of result rows
// ============================================================

unsafe extern "C" fn lua_sql(L: *mut LuaState) -> c_int {
    let query = match lua_to_str(L, 1) {
        Some(b) => match core::str::from_utf8(b) {
            Ok(s) => s,
            Err(_) => {
                lua_pushnil(L);
                lua_pushstring(L, b"invalid UTF-8 in query\0".as_ptr() as _);
                return 2;
            }
        },
        None => {
            lua_pushnil(L);
            lua_pushstring(L, b"sql() requires a string argument\0".as_ptr() as _);
            return 2;
        }
    };

    // Block dangerous SQL from agents (not REPL).
    // Check registry flag _SQL_READONLY; if set, only allow SELECT/EXPLAIN/PRAGMA.
    let restricted = is_sql_restricted(L);
    if restricted {
        let trimmed = query.trim_start().as_bytes();
        let allowed = starts_with_ignore_case(trimmed, b"SELECT")
            || starts_with_ignore_case(trimmed, b"EXPLAIN")
            || starts_with_ignore_case(trimmed, b"PRAGMA");
        if !allowed {
            lua_pushnil(L);
            lua_pushstring(L, b"sql() is read-only for agents\0".as_ptr() as _);
            return 2;
        }
    }

    // Use the SQLite database — structured query API
    let guard = crate::sqlite::DB.lock();
    let db = match guard.as_ref() {
        Some(db) => db,
        None => {
            lua_pushnil(L);
            lua_pushstring(L, b"database not open\0".as_ptr() as _);
            return 2;
        }
    };

    match db.query(query) {
        Ok(result) => {
            if result.columns.is_empty() {
                // DDL/DML — return true
                drop(guard);
                audit_log(L, "SQL_EXEC", query);
                lua_pushboolean(L, 1);
                return 1;
            }

            // Build Lua result table from typed rows
            lua_createtable(L, result.rows.len() as c_int, 0);

            for (row_idx, row) in result.rows.iter().enumerate() {
                // Create row table {col_name = value, ...}
                lua_createtable(L, 0, result.columns.len() as c_int);

                for (col_idx, val) in row.iter().enumerate() {
                    // Push typed value
                    push_sql_value(L, val);

                    // Set field: row[column_name] = value
                    if let Some(col_name) = result.columns.get(col_idx) {
                        let mut hdr_buf = alloc::vec::Vec::with_capacity(col_name.len() + 1);
                        hdr_buf.extend_from_slice(col_name.as_bytes());
                        hdr_buf.push(0);
                        lua_setfield(L, -2, hdr_buf.as_ptr() as *const c_char);
                    }
                }

                // result[row_idx+1] = row
                lua_rawseti(L, -2, (row_idx + 1) as i64);
            }

            drop(guard);
            audit_log(L, "SQL_EXEC", query);
            1 // return the result table
        }
        Err(e) => {
            drop(guard);
            lua_pushnil(L);
            push_rust_string(L, &e);
            2
        }
    }
}

/// Push a SqlValue onto the Lua stack with correct typing.
unsafe fn push_sql_value(L: *mut LuaState, val: &SqlValue) {
    match val {
        SqlValue::Null => lua_pushnil(L),
        SqlValue::Integer(n) => lua_pushinteger(L, *n),
        SqlValue::Real(n) => lua_pushnumber(L, *n),
        SqlValue::Text(s) => {
            lua_pushlstring(L, s.as_ptr() as *const c_char, s.len());
        }
    }
}

/// Push a Rust &str as a null-terminated Lua string.
unsafe fn push_rust_string(L: *mut LuaState, s: &str) {
    let mut buf = alloc::vec::Vec::with_capacity(s.len() + 1);
    buf.extend_from_slice(s.as_bytes());
    buf.push(0);
    lua_pushstring(L, buf.as_ptr() as *const c_char);
}

// ============================================================
// read(path) → string or nil
// ============================================================

unsafe extern "C" fn lua_read(L: *mut LuaState) -> c_int {
    let path = match lua_to_str(L, 1) {
        Some(b) => match core::str::from_utf8(b) {
            Ok(s) => s,
            Err(_) => { lua_pushnil(L); return 1; }
        },
        None => { lua_pushnil(L); return 1; }
    };

    let guard = crate::sqlite::DB.lock();
    let db = match guard.as_ref() {
        Some(db) => db,
        None => { lua_pushnil(L); return 1; }
    };

    let query = alloc::format!(
        "SELECT content FROM namespace WHERE path='{}'",
        path.replace('\'', "''")
    );

    match db.query_value(&query) {
        Ok(Some(content)) => {
            lua_pushlstring(L, content.as_ptr() as *const c_char, content.len());
            drop(guard);
            audit_log(L, "FILE_READ", path);
            1
        }
        _ => {
            lua_pushnil(L);
            1
        }
    }
}

// ============================================================
// write(path, data) → boolean
// ============================================================

unsafe extern "C" fn lua_write(L: *mut LuaState) -> c_int {
    let path = match lua_to_str(L, 1) {
        Some(b) => match core::str::from_utf8(b) {
            Ok(s) => alloc::string::String::from(s),
            Err(_) => { lua_pushboolean(L, 0); return 1; }
        },
        None => { lua_pushboolean(L, 0); return 1; }
    };

    let data = match lua_to_str(L, 2) {
        Some(b) => match core::str::from_utf8(b) {
            Ok(s) => alloc::string::String::from(s),
            Err(_) => { lua_pushboolean(L, 0); return 1; }
        },
        None => { lua_pushboolean(L, 0); return 1; }
    };

    let guard = crate::sqlite::DB.lock();
    let db = match guard.as_ref() {
        Some(db) => db,
        None => { lua_pushboolean(L, 0); return 1; }
    };

    // mtime = strftime('%s','now') via SQL expression
    let query = alloc::format!(
        "INSERT OR REPLACE INTO namespace (path, type, content, mtime) \
         VALUES ('{}', 'data', '{}', strftime('%s','now'))",
        path.replace('\'', "''"),
        data.replace('\'', "''")
    );

    let ok = db.exec(&query).is_ok();
    drop(guard);
    audit_log(L, "FILE_WRITE", &path);
    lua_pushboolean(L, ok as c_int);
    1
}

// ============================================================
// ls(path) → table of strings
// ============================================================

unsafe extern "C" fn lua_ls(L: *mut LuaState) -> c_int {
    let path = match lua_to_str(L, 1) {
        Some(b) => match core::str::from_utf8(b) {
            Ok(s) => s,
            Err(_) => "/",
        },
        None => "/",
    };

    let guard = crate::sqlite::DB.lock();
    let db = match guard.as_ref() {
        Some(db) => db,
        None => {
            lua_createtable(L, 0, 0);
            return 1;
        }
    };

    // List entries whose path starts with the given prefix.
    // Use substr() instead of LIKE to avoid wildcard injection (%, _).
    let prefix = if path.ends_with('/') {
        alloc::string::String::from(path)
    } else {
        alloc::format!("{}/", path)
    };

    let query = alloc::format!(
        "SELECT path FROM namespace WHERE substr(path, 1, {}) = '{}' ORDER BY path",
        prefix.len(),
        prefix.replace('\'', "''")
    );

    match db.query_column(&query) {
        Ok(paths) => {
            lua_createtable(L, paths.len() as c_int, 0);
            for (i, p) in paths.iter().enumerate() {
                lua_pushlstring(L, p.as_ptr() as *const c_char, p.len());
                lua_rawseti(L, -2, (i + 1) as i64);
            }
            1
        }
        Err(_) => {
            lua_createtable(L, 0, 0);
            1
        }
    }
}

// ============================================================
// log(msg) — write to serial console
// ============================================================

unsafe extern "C" fn lua_log(L: *mut LuaState) -> c_int {
    let nargs = lua_gettop(L);
    for i in 1..=nargs {
        if i > 1 {
            crate::serial_print!("\t");
        }
        match lua_to_str(L, i) {
            Some(bytes) => {
                if let Ok(s) = core::str::from_utf8(bytes) {
                    crate::serial_print!("{}", s);
                }
            }
            None => {
                let t = lua_type(L, i);
                match t {
                    LUA_TNIL => crate::serial_print!("nil"),
                    LUA_TBOOLEAN => {
                        let b = lua_toboolean(L, i);
                        crate::serial_print!("{}", if b != 0 { "true" } else { "false" });
                    }
                    _ => crate::serial_print!("({} value)", type_name(t)),
                }
            }
        }
    }
    crate::serial_println!();
    0
}

fn type_name(t: c_int) -> &'static str {
    match t {
        LUA_TNIL => "nil",
        LUA_TBOOLEAN => "boolean",
        LUA_TNUMBER => "number",
        LUA_TSTRING => "string",
        LUA_TTABLE => "table",
        _ => "other",
    }
}

// ============================================================
// sleep(ms) — busy-wait using TSC
// ============================================================

const MAX_SLEEP_MS: i64 = 60_000; // 60 seconds max

unsafe extern "C" fn lua_sleep(L: *mut LuaState) -> c_int {
    let ms = lua_tointegerx(L, 1, core::ptr::null_mut());
    if ms > 0 {
        let clamped = if ms > MAX_SLEEP_MS { MAX_SLEEP_MS } else { ms };
        crate::arch::x86_64::timer::delay_us(clamped as u64 * 1000);
    }
    0
}

// ============================================================
// now() → monotonic ms since boot
// ============================================================

unsafe extern "C" fn lua_now(L: *mut LuaState) -> c_int {
    let ms = crate::arch::x86_64::timer::monotonic_ms();
    lua_pushinteger(L, ms as i64);
    1
}

// ============================================================
// audit(level, action, detail)
// ============================================================

unsafe extern "C" fn lua_audit(L: *mut LuaState) -> c_int {
    let level = match lua_to_str(L, 1) {
        Some(b) => core::str::from_utf8(b).unwrap_or("INFO"),
        None => "INFO",
    };
    let action = match lua_to_str(L, 2) {
        Some(b) => core::str::from_utf8(b).unwrap_or(""),
        None => "",
    };
    let detail = match lua_to_str(L, 3) {
        Some(b) => core::str::from_utf8(b).unwrap_or(""),
        None => "",
    };

    // Get agent name from registry
    let agent = get_agent_name(L);

    let guard = crate::sqlite::DB.lock();
    if let Some(db) = guard.as_ref() {
        let query = alloc::format!(
            "INSERT INTO audit (level, agent, action, detail) VALUES ('{}', '{}', '{}', '{}')",
            level.replace('\'', "''"),
            agent.replace('\'', "''"),
            action.replace('\'', "''"),
            detail.replace('\'', "''"),
        );
        let _ = db.exec(&query);
    }

    0
}

// ============================================================
// ask(prompt) or ask({system=..., messages={...}}) → string
// ============================================================

/// Rate limit: minimum interval between ask() calls (ms).
const ASK_MIN_INTERVAL_MS: u64 = 10_000;
static LAST_ASK_MS: spin::Mutex<u64> = spin::Mutex::new(0);

unsafe extern "C" fn lua_ask(L: *mut LuaState) -> c_int {
    use alloc::string::String;
    use alloc::vec::Vec;

    // Rate limiting
    let now_ms = crate::arch::x86_64::timer::monotonic_ms();
    {
        let mut last = LAST_ASK_MS.lock();
        if now_ms - *last < ASK_MIN_INTERVAL_MS {
            lua_pushnil(L);
            push_rust_string(L, "ask() rate limited (10s between calls)");
            return 2;
        }
        *last = now_ms;
    }

    // Check API key
    let api_key = match crate::api::get_api_key() {
        Some(k) => k,
        None => {
            lua_pushnil(L);
            push_rust_string(L, "API key not set");
            return 2;
        }
    };

    // Parse arguments: either a string or a table
    let arg_type = lua_type(L, 1);

    let (system, messages) = if arg_type == LUA_TSTRING {
        // Simple mode: ask("prompt")
        let prompt = match lua_to_str(L, 1) {
            Some(b) => match core::str::from_utf8(b) {
                Ok(s) => String::from(s),
                Err(_) => {
                    lua_pushnil(L);
                    push_rust_string(L, "invalid UTF-8 in prompt");
                    return 2;
                }
            },
            None => {
                lua_pushnil(L);
                push_rust_string(L, "ask() requires a string or table argument");
                return 2;
            }
        };
        (None, vec![crate::api::Message::text("user", prompt)])
    } else if arg_type == LUA_TTABLE {
        // Table mode: ask({system="...", messages={...}})
        let mut system = None;
        let mut messages = Vec::new();

        // Get system field
        lua_getfield(L, 1, b"system\0".as_ptr() as *const c_char);
        if !lua_isnil(L, -1) {
            if let Some(b) = lua_to_str(L, -1) {
                if let Ok(s) = core::str::from_utf8(b) {
                    system = Some(String::from(s));
                }
            }
        }
        lua_pop(L, 1);

        // Get messages array
        lua_getfield(L, 1, b"messages\0".as_ptr() as *const c_char);
        if lua_type(L, -1) == LUA_TTABLE {
            let msg_table_idx = lua_gettop(L);
            messages = parse_messages_table(L, msg_table_idx);
        }
        lua_pop(L, 1); // pop messages

        if messages.is_empty() {
            lua_pushnil(L);
            push_rust_string(L, "ask() table must contain 'messages' array");
            return 2;
        }

        (system, messages)
    } else {
        lua_pushnil(L);
        push_rust_string(L, "ask() requires a string or table argument");
        return 2;
    };

    // Acquire network stack
    let mut net_guard = crate::net::NET_STACK.lock();
    let net = match net_guard.as_mut() {
        Some(n) => n,
        None => {
            lua_pushnil(L);
            push_rust_string(L, "network stack not initialized");
            return 2;
        }
    };

    // Resolve API target IP
    let target_ip = match crate::net::dns::resolve_a(net, "api.anthropic.com") {
        Ok(ip) => ip,
        Err(e) => {
            lua_pushnil(L);
            let msg = alloc::format!("DNS resolution failed: {}", e);
            push_rust_string(L, &msg);
            return 2;
        }
    };

    // Build request
    let request = crate::api::ClaudeRequest {
        config: crate::api::ClaudeConfig {
            api_key,
            model: crate::api::get_model(),
            ..crate::api::ClaudeConfig::direct_tls(target_ip)
        },
        system,
        messages,
        use_tools: false,
    };

    // Send request (no streaming to console for Lua — collect full response)
    let result = crate::api::claude_request_multi(net, &request, |_| {});
    drop(net_guard);

    match result {
        Ok(text) => {
            audit_log(L, "API_CALL", "ask()");
            lua_pushlstring(L, text.as_ptr() as *const c_char, text.len());
            1
        }
        Err(e) => {
            let msg = alloc::format!("{}", e);
            lua_pushnil(L);
            push_rust_string(L, &msg);
            2
        }
    }
}

/// Parse a Lua messages table into a Vec<Message>.
/// Expects: { {role="user", content="..."}, {role="assistant", content="..."}, ... }
/// Uses lua_next to iterate the array.
unsafe fn parse_messages_table(L: *mut LuaState, table_idx: c_int) -> Vec<crate::api::Message> {
    use alloc::string::String;

    let mut messages = Vec::new();

    lua_pushnil(L); // first key
    while lua_next(L, table_idx) != 0 {
        // key at -2, value at -1
        if lua_type(L, -1) == LUA_TTABLE {
            let msg_idx = lua_gettop(L);

            let mut role = String::from("user");
            let mut content = String::new();

            // Get role
            lua_getfield(L, msg_idx, b"role\0".as_ptr() as *const c_char);
            if let Some(b) = lua_to_str(L, -1) {
                if let Ok(s) = core::str::from_utf8(b) {
                    role = String::from(s);
                }
            }
            lua_pop(L, 1);

            // Get content
            lua_getfield(L, msg_idx, b"content\0".as_ptr() as *const c_char);
            if let Some(b) = lua_to_str(L, -1) {
                if let Ok(s) = core::str::from_utf8(b) {
                    content = String::from(s);
                }
            }
            lua_pop(L, 1);

            // Map role string to static
            let static_role: &'static str = match role.as_str() {
                "assistant" => "assistant",
                _ => "user",
            };

            messages.push(crate::api::Message::text(static_role, content));
        }
        lua_pop(L, 1); // pop value, keep key for next iteration
    }

    messages
}

// ============================================================
// Internal helpers
// ============================================================

/// Case-insensitive prefix check on byte slices.
fn starts_with_ignore_case(haystack: &[u8], needle: &[u8]) -> bool {
    if haystack.len() < needle.len() {
        return false;
    }
    for (h, n) in haystack[..needle.len()].iter().zip(needle.iter()) {
        if h.to_ascii_uppercase() != n.to_ascii_uppercase() {
            return false;
        }
    }
    true
}

/// Check if SQL is restricted to read-only for this Lua state.
unsafe fn is_sql_restricted(L: *mut LuaState) -> bool {
    lua_getfield(L, LUA_REGISTRYINDEX, b"_SQL_READONLY\0".as_ptr() as *const c_char);
    let restricted = lua_toboolean(L, -1) != 0;
    lua_pop(L, 1);
    restricted
}

/// Mark this Lua state as SQL-restricted (read-only).
pub unsafe fn set_sql_readonly(L: *mut LuaState, readonly: bool) {
    lua_pushboolean(L, readonly as core::ffi::c_int);
    lua_setfield(L, LUA_REGISTRYINDEX, b"_SQL_READONLY\0".as_ptr() as *const c_char);
}

/// Get the agent name from the Lua registry.
unsafe fn get_agent_name(L: *mut LuaState) -> alloc::string::String {
    lua_getfield(L, LUA_REGISTRYINDEX, b"_AGENT_NAME\0".as_ptr() as *const c_char);
    let name = match lua_to_str(L, -1) {
        Some(b) => alloc::string::String::from_utf8_lossy(b).into_owned(),
        None => alloc::string::String::from("unknown"),
    };
    lua_pop(L, 1);
    name
}

/// Log an action to the audit table.
unsafe fn audit_log(L: *mut LuaState, action: &str, target: &str) {
    let agent = get_agent_name(L);
    let guard = crate::sqlite::DB.lock();
    if let Some(db) = guard.as_ref() {
        let query = alloc::format!(
            "INSERT INTO audit (agent, action, target) VALUES ('{}', '{}', '{}')",
            agent.replace('\'', "''"),
            action.replace('\'', "''"),
            target.replace('\'', "''"),
        );
        let _ = db.exec(&query);
    }
}
