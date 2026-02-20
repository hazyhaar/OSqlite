//! Lua allocator bridge — delegates to the kernel slab allocator.
//!
//! Lua calls `l_alloc(ud, ptr, osize, nsize)` for all memory operations.
//! This function is passed to `lua_newstate()`.
//!
//! A per-state memory limit is enforced via a `LuaAllocState` userdata
//! pointer. When the limit is exceeded, the allocator returns NULL and
//! Lua raises a memory error.

use core::ffi::c_void;

/// Default memory limit per Lua state: 1 MiB.
pub const LUA_MEM_LIMIT: usize = 1024 * 1024;

/// Per-Lua-state allocation tracking.
#[repr(C)]
pub struct LuaAllocState {
    pub used: usize,
    pub limit: usize,
}

impl LuaAllocState {
    pub fn new(limit: usize) -> Self {
        Self { used: 0, limit }
    }
}

/// Lua allocator with per-state memory limit.
///
/// Contract (identical to ANSI realloc):
/// - nsize == 0: free ptr, return NULL
/// - ptr == NULL: allocate nsize bytes (malloc)
/// - otherwise: reallocate ptr from osize to nsize (realloc)
pub unsafe extern "C" fn heaven_lua_alloc(
    ud: *mut c_void,
    ptr: *mut c_void,
    osize: usize,
    nsize: usize,
) -> *mut c_void {
    extern "C" {
        fn heavenos_malloc(size: usize) -> *mut u8;
        fn heavenos_free(ptr: *mut u8);
        fn heavenos_realloc(ptr: *mut u8, new_size: usize) -> *mut u8;
    }

    let state = &mut *(ud as *mut LuaAllocState);

    if nsize == 0 {
        // Free
        if !ptr.is_null() {
            state.used = state.used.saturating_sub(osize);
            heavenos_free(ptr as *mut u8);
        }
        core::ptr::null_mut()
    } else if ptr.is_null() {
        // Malloc
        if state.used + nsize > state.limit {
            return core::ptr::null_mut(); // OOM — Lua will raise memory error
        }
        let p = heavenos_malloc(nsize);
        if !p.is_null() {
            state.used += nsize;
        }
        p as *mut c_void
    } else {
        // Realloc
        if nsize > osize {
            let delta = nsize - osize;
            if state.used + delta > state.limit {
                return core::ptr::null_mut(); // OOM
            }
        }
        let p = heavenos_realloc(ptr as *mut u8, nsize);
        if !p.is_null() {
            // Update accounting: remove old size, add new size
            state.used = state.used.saturating_sub(osize) + nsize;
        }
        p as *mut c_void
    }
}
