//! Lua 5.5.0 integration for HeavenOS.
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

use core::ffi::{c_int, c_void};

use ffi::*;

/// Default execution timeout for Lua agents (30 seconds).
const EXEC_TIMEOUT_MS: u64 = 30_000;

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
        // 1. Create Lua state with our allocator (memory-limited)
        let mut alloc_state = alloc::LuaAllocState::new(alloc::LUA_MEM_LIMIT);
        let ud = &mut alloc_state as *mut alloc::LuaAllocState as *mut core::ffi::c_void;
        let L = lua_newstate(alloc::heaven_lua_alloc, ud, 0);
        if L.is_null() {
            return Err(String::from("failed to create Lua state (out of memory)"));
        }

        // 2. Open filtered standard libraries
        luaL_openlibs(L);

        // 3. Configure GC for incremental mode with small steps
        lua_gc(L, LUA_GCINC);
        lua_gc(L, LUA_GCPARAM, LUA_GCPPAUSE as c_int, 100 as c_int);
        lua_gc(L, LUA_GCPARAM, LUA_GCPSTEPMUL as c_int, 200 as c_int);
        lua_gc(L, LUA_GCPARAM, LUA_GCPSTEPSIZE as c_int, 10 as c_int);

        // 4. Register OSqlite builtins
        builtins::register_builtins(L);

        // 5. Store agent name in registry for audit logging
        store_agent_name(L, name);

        // 6. Restrict SQL to read-only for agents (REPL has full access)
        builtins::set_sql_readonly(L, true);

        // 7. Install execution timeout hook (30 second limit for agents)
        install_timeout_hook(L, EXEC_TIMEOUT_MS);

        // 7. Load and execute the script
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

    match db.query_value(&query) {
        Ok(Some(content)) => Ok(content),
        Ok(None) => Err(::alloc::format!("agent not found: {}", path)),
        Err(e) => Err(e),
    }
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

/// Install a Lua debug hook that aborts execution after a timeout.
///
/// The hook fires every 10000 instructions and checks elapsed time via TSC.
/// The deadline (in TSC ticks) is stored in the Lua registry as a light userdata.
unsafe fn install_timeout_hook(L: *mut LuaState, timeout_ms: u64) {
    let per_ms = crate::arch::x86_64::timer::tsc_per_ms();
    let start = crate::arch::x86_64::cpu::rdtsc();
    let deadline = if per_ms > 0 {
        start.saturating_add(timeout_ms.saturating_mul(per_ms))
    } else {
        u64::MAX // no TSC calibration — no timeout
    };

    // Store deadline in registry as light userdata (pointer-sized integer)
    lua_pushinteger(L, deadline as i64);
    lua_setfield(L, LUA_REGISTRYINDEX, b"_DEADLINE\0".as_ptr() as *const i8);

    // Install count hook: fires every 10000 VM instructions
    lua_sethook(L, Some(timeout_hook), LUA_MASKCOUNT, 10000);
}

/// Lua debug hook callback — checks if execution has exceeded deadline.
unsafe extern "C" fn timeout_hook(L: *mut LuaState, _ar: *mut c_void) {
    lua_getfield(L, LUA_REGISTRYINDEX, b"_DEADLINE\0".as_ptr() as *const i8);
    let deadline = lua_tointegerx(L, -1, core::ptr::null_mut()) as u64;
    lua_pop(L, 1);

    let now = crate::arch::x86_64::cpu::rdtsc();
    if now >= deadline {
        luaL_error(L, b"execution timeout exceeded\0".as_ptr() as *const i8);
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
