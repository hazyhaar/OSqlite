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

use core::ffi::{c_char, c_int};
use super::ffi::*;

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
        let upper = query.trim_start();
        let allowed = upper.starts_with("SELECT")
            || upper.starts_with("select")
            || upper.starts_with("EXPLAIN")
            || upper.starts_with("explain")
            || upper.starts_with("PRAGMA")
            || upper.starts_with("pragma");
        if !allowed {
            lua_pushnil(L);
            lua_pushstring(L, b"sql() is read-only for agents\0".as_ptr() as _);
            return 2;
        }
    }

    // Use the SQLite database
    let guard = crate::sqlite::DB.lock();
    let db = match guard.as_ref() {
        Some(db) => db,
        None => {
            lua_pushnil(L);
            lua_pushstring(L, b"database not open\0".as_ptr() as _);
            return 2;
        }
    };

    // Execute via the prepared statement API for results
    match db.exec_with_results(query) {
        Ok(output) => {
            // Parse the pipe-delimited output into Lua tables
            // Format: "col1|col2\nval1|val2\nval3|val4\n"
            let lines: alloc::vec::Vec<&str> = output.lines().collect();

            if lines.is_empty() || output.trim() == "OK" {
                // DDL/DML — return true
                lua_pushboolean(L, 1);
                return 1;
            }

            // First line is headers
            let headers: alloc::vec::Vec<&str> = lines[0].split('|').collect();

            // Create result table
            lua_createtable(L, (lines.len() - 1) as c_int, 0);

            for (row_idx, line) in lines[1..].iter().enumerate() {
                let values: alloc::vec::Vec<&str> = line.split('|').collect();

                // Create row table
                lua_createtable(L, 0, headers.len() as c_int);

                for (col_idx, header) in headers.iter().enumerate() {
                    let val = values.get(col_idx).unwrap_or(&"");

                    // Push value (try integer, then number, then string)
                    if *val == "NULL" {
                        lua_pushnil(L);
                    } else if let Ok(n) = val.parse::<i64>() {
                        lua_pushinteger(L, n);
                    } else if let Ok(n) = val.parse::<f64>() {
                        lua_pushnumber(L, n);
                    } else {
                        lua_pushlstring(L, val.as_ptr() as *const c_char, val.len());
                    }

                    // Null-terminate header name for setfield
                    let mut hdr_buf = alloc::vec::Vec::with_capacity(header.len() + 1);
                    hdr_buf.extend_from_slice(header.as_bytes());
                    hdr_buf.push(0);
                    lua_setfield(L, -2, hdr_buf.as_ptr() as *const c_char);
                }

                // result[row_idx+1] = row
                lua_rawseti(L, -2, (row_idx + 1) as i64);
            }

            // Log to audit
            drop(guard);
            audit_log(L, "SQL_EXEC", query);

            1 // return the result table
        }
        Err(e) => {
            drop(guard);
            lua_pushnil(L);
            let mut msg_buf = alloc::vec::Vec::with_capacity(e.len() + 1);
            msg_buf.extend_from_slice(e.as_bytes());
            msg_buf.push(0);
            lua_pushstring(L, msg_buf.as_ptr() as *const c_char);
            2
        }
    }
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

    match db.exec_with_results(&query) {
        Ok(output) => {
            let lines: alloc::vec::Vec<&str> = output.lines().collect();
            if lines.len() >= 2 {
                // Content may contain embedded newlines — join all lines after header
                let content = lines[1..].join("\n");
                lua_pushlstring(L, content.as_ptr() as *const c_char, content.len());
            } else {
                lua_pushnil(L);
            }
            drop(guard);
            audit_log(L, "FILE_READ", path);
            1
        }
        Err(_) => {
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

    let query = alloc::format!(
        "INSERT OR REPLACE INTO namespace (path, type, content) VALUES ('{}', 'data', '{}')",
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

    // List entries whose path starts with the given prefix
    let prefix = if path.ends_with('/') {
        alloc::string::String::from(path)
    } else {
        alloc::format!("{}/", path)
    };

    let query = alloc::format!(
        "SELECT path FROM namespace WHERE path LIKE '{}%' ORDER BY path",
        prefix.replace('\'', "''")
    );

    match db.exec_with_results(&query) {
        Ok(output) => {
            let lines: alloc::vec::Vec<&str> = output.lines().collect();
            lua_createtable(L, (lines.len().saturating_sub(1)) as c_int, 0);

            for (i, line) in lines[1..].iter().enumerate() {
                lua_pushlstring(L, line.as_ptr() as *const c_char, line.len());
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
// Internal helpers
// ============================================================

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
