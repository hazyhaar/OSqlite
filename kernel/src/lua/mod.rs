//! Lua 5.4.8 integration for HeavenOS.
//!
//! Provides:
//! - `run_agent(path)`: load a Lua script from the namespace table and execute it
//! - `run_string(code, name)`: execute a Lua string directly
//! - `repl()`: interactive Lua REPL over serial
//!
//! Each `run_agent` call creates a fresh Lua state, registers the
//! OSqlite builtins (sql, read, write, ls, log, sleep, now, audit),
//! executes the script, and tears down the state.

pub mod ffi;
pub mod alloc;
pub mod builtins;
pub mod repl;

use ::alloc::string::String;
use ::alloc::vec::Vec;

use ffi::*;

/// Run a Lua agent stored in the namespace table.
///
/// 1. SELECT content FROM namespace WHERE path=? AND type='lua'
/// 2. Create Lua state, load libs, register builtins
/// 3. Execute the script
/// 4. Close state
///
/// Returns Ok(()) on success, Err(message) on failure.
pub fn run_agent(path: &str) -> Result<(), String> {
    // 1. Load script from SQLite namespace table
    let content = load_script_from_db(path)?;

    // 2. Run it
    run_string(&content, path)
}

/// Execute a Lua source string.
pub fn run_string(code: &str, name: &str) -> Result<(), String> {
    unsafe {
        // 1. Create Lua state with our allocator
        let L = lua_newstate(alloc::heaven_lua_alloc, core::ptr::null_mut());
        if L.is_null() {
            return Err(String::from("failed to create Lua state (out of memory)"));
        }

        // 2. Open filtered standard libraries
        luaL_openlibs(L);

        // 3. Configure GC for incremental mode with small steps
        lua_gc(L, LUA_GCINC, 100, 200, 10);

        // 4. Register OSqlite builtins
        builtins::register_builtins(L);

        // 5. Store agent name in registry for audit logging
        store_agent_name(L, name);

        // 6. Load and execute the script
        let result = load_and_exec(L, code, name);

        // 7. Close state (frees all Lua memory)
        lua_close(L);

        result
    }
}

/// Load script content from the namespace table via SQLite.
fn load_script_from_db(path: &str) -> Result<String, String> {
    let guard = crate::sqlite::DB.lock();
    let db = guard
        .as_ref()
        .ok_or_else(|| String::from("database not open"))?;

    // Build the query with the path escaped
    let query = ::alloc::format!(
        "SELECT content FROM namespace WHERE path='{}' AND type='lua'",
        path.replace('\'', "''")
    );

    let result = db.exec_with_results(&query)?;

    // exec_with_results returns "header\nrow1\n..." — skip the header line
    let lines: Vec<&str> = result.lines().collect();
    if lines.len() < 2 {
        return Err(::alloc::format!("agent not found: {}", path));
    }

    // The content is everything after the header line
    Ok(String::from(lines[1]))
}

/// Store the agent name in the Lua registry so builtins can read it for audit.
unsafe fn store_agent_name(L: *mut LuaState, name: &str) {
    let mut buf = Vec::with_capacity(name.len() + 1);
    buf.extend_from_slice(name.as_bytes());
    buf.push(0);
    lua_pushlstring(L, buf.as_ptr() as *const i8, name.len());
    lua_setfield(L, LUA_REGISTRYINDEX, b"_AGENT_NAME\0".as_ptr() as *const i8);
}

/// Load a Lua chunk from a string and execute it with pcall.
unsafe fn load_and_exec(L: *mut LuaState, code: &str, name: &str) -> Result<(), String> {
    // Null-terminate the chunk name
    let mut name_buf = Vec::with_capacity(name.len() + 1);
    name_buf.extend_from_slice(name.as_bytes());
    name_buf.push(0);

    // Load the chunk
    let rc = luaL_loadbufferx(
        L,
        code.as_ptr() as *const i8,
        code.len(),
        name_buf.as_ptr() as *const i8,
        core::ptr::null(), // auto-detect text/binary
    );

    if rc != LUA_OK {
        let err = get_lua_error(L);
        return Err(err);
    }

    // Execute with pcall (protected call — errors don't panic the kernel)
    let rc = lua_pcall(L, 0, LUA_MULTRET, 0);
    if rc != LUA_OK {
        let err = get_lua_error(L);
        return Err(err);
    }

    Ok(())
}

/// Pop the error message from the Lua stack.
unsafe fn get_lua_error(L: *mut LuaState) -> String {
    match lua_to_str(L, -1) {
        Some(bytes) => {
            let msg = String::from_utf8_lossy(bytes).into_owned();
            lua_pop(L, 1);
            msg
        }
        None => {
            lua_pop(L, 1);
            String::from("unknown Lua error")
        }
    }
}

// === C FFI exports called from heaven_lua_stubs.c ===

/// Write bytes to the serial console (called from C).
#[no_mangle]
pub extern "C" fn serial_write_bytes(s: *const u8, len: i32) {
    if s.is_null() || len <= 0 {
        return;
    }
    let bytes = unsafe { core::slice::from_raw_parts(s, len as usize) };
    if let Ok(text) = core::str::from_utf8(bytes) {
        crate::serial_print!("{}", text);
    }
}

/// Kernel halt — called from C exit()/abort() stubs.
#[no_mangle]
pub extern "C" fn rust_panic_halt() -> ! {
    panic!("Lua called exit/abort");
}
