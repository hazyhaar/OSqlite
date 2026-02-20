//! Raw FFI bindings to the Lua 5.4.8 C API.

use core::ffi::{c_char, c_int, c_void};

pub type LuaState = c_void;
pub type LuaCFunction = unsafe extern "C" fn(*mut LuaState) -> c_int;
pub type LuaAllocF = unsafe extern "C" fn(*mut c_void, *mut c_void, usize, usize) -> *mut c_void;

extern "C" {
    // === Lifecycle ===
    pub fn lua_newstate(f: LuaAllocF, ud: *mut c_void) -> *mut LuaState;
    pub fn lua_close(L: *mut LuaState);
    pub fn luaL_openlibs(L: *mut LuaState);

    // === Stack ===
    pub fn lua_gettop(L: *mut LuaState) -> c_int;
    pub fn lua_settop(L: *mut LuaState, idx: c_int);
    pub fn lua_pushnil(L: *mut LuaState);
    pub fn lua_pushinteger(L: *mut LuaState, n: i64);
    pub fn lua_pushnumber(L: *mut LuaState, n: f64);
    pub fn lua_pushstring(L: *mut LuaState, s: *const c_char) -> *const c_char;
    pub fn lua_pushlstring(L: *mut LuaState, s: *const c_char, len: usize) -> *const c_char;
    pub fn lua_pushcclosure(L: *mut LuaState, f: LuaCFunction, n: c_int);
    pub fn lua_pushboolean(L: *mut LuaState, b: c_int);

    // === Getters ===
    pub fn lua_tointegerx(L: *mut LuaState, idx: c_int, isnum: *mut c_int) -> i64;
    pub fn lua_tonumberx(L: *mut LuaState, idx: c_int, isnum: *mut c_int) -> f64;
    pub fn lua_tolstring(L: *mut LuaState, idx: c_int, len: *mut usize) -> *const c_char;
    pub fn lua_toboolean(L: *mut LuaState, idx: c_int) -> c_int;
    pub fn lua_type(L: *mut LuaState, idx: c_int) -> c_int;

    // === Tables ===
    pub fn lua_createtable(L: *mut LuaState, narr: c_int, nrec: c_int);
    pub fn lua_setfield(L: *mut LuaState, idx: c_int, k: *const c_char);
    pub fn lua_getfield(L: *mut LuaState, idx: c_int, k: *const c_char) -> c_int;
    pub fn lua_rawseti(L: *mut LuaState, idx: c_int, n: i64);
    pub fn lua_next(L: *mut LuaState, idx: c_int) -> c_int;

    // === Globals ===
    pub fn lua_setglobal(L: *mut LuaState, name: *const c_char);
    pub fn lua_getglobal(L: *mut LuaState, name: *const c_char) -> c_int;

    // === Execution ===
    pub fn luaL_loadbufferx(
        L: *mut LuaState,
        buff: *const c_char,
        sz: usize,
        name: *const c_char,
        mode: *const c_char,
    ) -> c_int;
    pub fn lua_pcallk(
        L: *mut LuaState,
        nargs: c_int,
        nresults: c_int,
        errfunc: c_int,
        ctx: isize,
        k: Option<unsafe extern "C" fn(*mut LuaState, c_int, isize) -> c_int>,
    ) -> c_int;

    // === GC ===
    pub fn lua_gc(L: *mut LuaState, what: c_int, ...) -> c_int;

    // === Errors ===
    pub fn lua_error(L: *mut LuaState) -> c_int;

    // === Debug hooks (for execution timeout) ===
    pub fn lua_sethook(
        L: *mut LuaState,
        f: Option<unsafe extern "C" fn(*mut LuaState, *mut c_void)>,
        mask: c_int,
        count: c_int,
    ) -> c_int;

    // === Auxiliary ===
    pub fn luaL_error(L: *mut LuaState, fmt: *const c_char, ...) -> c_int;
}

// === Constants ===
pub const LUA_OK: c_int = 0;
pub const LUA_ERRSYNTAX: c_int = 3;
pub const LUA_ERRMEM: c_int = 4;

pub const LUA_TNIL: c_int = 0;
pub const LUA_TBOOLEAN: c_int = 1;
pub const LUA_TNUMBER: c_int = 3;
pub const LUA_TSTRING: c_int = 4;
pub const LUA_TTABLE: c_int = 5;

pub const LUA_MULTRET: c_int = -1;
pub const LUA_REGISTRYINDEX: c_int = -1001000;

pub const LUA_GCINC: c_int = 9;

// Debug hook masks
pub const LUA_MASKCOUNT: c_int = 1 << 3;

// === Inline helpers ===

#[inline]
pub unsafe fn lua_pop(L: *mut LuaState, n: c_int) {
    lua_settop(L, -(n) - 1);
}

#[inline]
pub unsafe fn lua_register(L: *mut LuaState, name: *const c_char, f: LuaCFunction) {
    lua_pushcclosure(L, f, 0);
    lua_setglobal(L, name);
}

#[inline]
pub unsafe fn lua_pcall(L: *mut LuaState, n: c_int, r: c_int, f: c_int) -> c_int {
    lua_pcallk(L, n, r, f, 0, None)
}

#[inline]
pub unsafe fn lua_isstring(L: *mut LuaState, idx: c_int) -> bool {
    let t = lua_type(L, idx);
    t == LUA_TSTRING || t == LUA_TNUMBER
}

#[inline]
pub unsafe fn lua_isnil(L: *mut LuaState, idx: c_int) -> bool {
    lua_type(L, idx) == LUA_TNIL
}

/// Get a string from the Lua stack as a byte slice.
/// Returns None if the value is not a string.
pub unsafe fn lua_to_str<'a>(L: *mut LuaState, idx: c_int) -> Option<&'a [u8]> {
    let mut len: usize = 0;
    let ptr = lua_tolstring(L, idx, &mut len);
    if ptr.is_null() {
        None
    } else {
        Some(core::slice::from_raw_parts(ptr as *const u8, len))
    }
}
