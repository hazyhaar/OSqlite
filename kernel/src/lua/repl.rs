//! Interactive Lua REPL over serial console.
//!
//! Creates a persistent Lua state and reads lines from the serial port.
//! ^D (Ctrl-D) or `exit()` returns to the HeavenOS shell.

use crate::{serial_print, serial_println};
use crate::shell::line::LineEditor;
use super::ffi::*;
use super::alloc::heaven_lua_alloc;
use super::builtins::register_builtins;
use core::ffi::c_int;

/// Run the interactive Lua REPL. Returns when the user types ^D.
pub fn run() {
    serial_println!("Lua 5.5.0  Copyright (C) 1994-2025 Lua.org, PUC-Rio");
    serial_println!("Type ^D to exit.");

    unsafe {
        // REPL gets a larger limit (4 MiB) for interactive use
        let mut alloc_state = super::alloc::LuaAllocState::new(4 * super::alloc::LUA_MEM_LIMIT);
        let ud = &mut alloc_state as *mut super::alloc::LuaAllocState as *mut core::ffi::c_void;
        let L = lua_newstate(heaven_lua_alloc, ud, 0);
        if L.is_null() {
            serial_println!("[lua] ERROR: failed to create Lua state (out of memory)");
            return;
        }

        luaL_openlibs(L);
        lua_gc(L, LUA_GCINC);
        lua_gc(L, LUA_GCPARAM, LUA_GCPPAUSE as core::ffi::c_int, 100 as core::ffi::c_int);
        lua_gc(L, LUA_GCPARAM, LUA_GCPSTEPMUL as core::ffi::c_int, 200 as core::ffi::c_int);
        lua_gc(L, LUA_GCPARAM, LUA_GCPSTEPSIZE as core::ffi::c_int, 10 as core::ffi::c_int);
        register_builtins(L);

        // Store agent name for audit
        lua_pushlstring(L, b"<repl>\0".as_ptr() as *const i8, 6);
        lua_setfield(L, LUA_REGISTRYINDEX, b"_AGENT_NAME\0".as_ptr() as *const i8);

        // Register exit() function
        lua_register(L, b"exit\0".as_ptr() as _, lua_exit);

        let mut editor = LineEditor::new();

        loop {
            serial_print!("> ");
            match editor.read_line() {
                Some(line) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }

                    // Try as expression first (prepend "return ")
                    let expr_code = ::alloc::format!("return {}", trimmed);
                    let rc = luaL_loadbufferx(
                        L,
                        expr_code.as_ptr() as *const i8,
                        expr_code.len(),
                        b"=stdin\0".as_ptr() as *const i8,
                        core::ptr::null(),
                    );

                    if rc == LUA_OK {
                        // Expression loaded — execute and print result
                        let rc = lua_pcall(L, 0, LUA_MULTRET, 0);
                        if rc == LUA_OK {
                            let nresults = lua_gettop(L);
                            if nresults > 0 {
                                print_stack_values(L, nresults);
                                lua_pop(L, nresults);
                            }
                        } else {
                            // Check for exit signal
                            if check_exit_signal(L) {
                                lua_close(L);
                                return;
                            }
                            print_error(L);
                        }
                    } else {
                        // Not an expression — try as statement
                        lua_pop(L, 1); // pop error from expression attempt

                        let rc = luaL_loadbufferx(
                            L,
                            trimmed.as_ptr() as *const i8,
                            trimmed.len(),
                            b"=stdin\0".as_ptr() as *const i8,
                            core::ptr::null(),
                        );

                        if rc == LUA_OK {
                            let rc = lua_pcall(L, 0, LUA_MULTRET, 0);
                            if rc != LUA_OK {
                                if check_exit_signal(L) {
                                    lua_close(L);
                                    return;
                                }
                                print_error(L);
                            }
                        } else {
                            print_error(L);
                        }
                    }
                }
                None => {
                    // ^C or ^D — exit REPL
                    serial_println!();
                    lua_close(L);
                    return;
                }
            }
        }
    }
}

/// Print values on the Lua stack (for REPL expression results).
unsafe fn print_stack_values(L: *mut LuaState, n: c_int) {
    for i in 1..=n {
        if i > 1 {
            serial_print!("\t");
        }
        match lua_to_str(L, i) {
            Some(bytes) => {
                if let Ok(s) = core::str::from_utf8(bytes) {
                    serial_print!("{}", s);
                }
            }
            None => {
                let t = lua_type(L, i);
                match t {
                    LUA_TNIL => serial_print!("nil"),
                    LUA_TBOOLEAN => {
                        let b = lua_toboolean(L, i);
                        serial_print!("{}", if b != 0 { "true" } else { "false" });
                    }
                    LUA_TTABLE => serial_print!("(table)"),
                    _ => serial_print!("(value)"),
                }
            }
        }
    }
    serial_println!();
}

/// Print a Lua error from the top of the stack.
unsafe fn print_error(L: *mut LuaState) {
    match lua_to_str(L, -1) {
        Some(bytes) => {
            if let Ok(s) = core::str::from_utf8(bytes) {
                serial_println!("{}", s);
            }
        }
        None => serial_println!("(unknown error)"),
    }
    lua_pop(L, 1);
}

/// Unique address used as lightuserdata sentinel for exit().
/// Using a lightuserdata avoids collisions with user error strings.
static EXIT_SENTINEL: u8 = 0;

/// exit() Lua function — signals the REPL to exit by raising an error.
unsafe extern "C" fn lua_exit(L: *mut LuaState) -> c_int {
    lua_pushlightuserdata(L, &EXIT_SENTINEL as *const u8 as *mut core::ffi::c_void);
    lua_error(L)
}

/// Check if the error on the stack is our exit sentinel (lightuserdata).
unsafe fn check_exit_signal(L: *mut LuaState) -> bool {
    if lua_type(L, -1) != LUA_TLIGHTUSERDATA {
        return false;
    }
    let p = lua_touserdata(L, -1);
    p == &EXIT_SENTINEL as *const u8 as *mut core::ffi::c_void
}
