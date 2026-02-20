//! Lua allocator bridge — delegates to the kernel slab allocator.
//!
//! Lua calls `l_alloc(ud, ptr, osize, nsize)` for all memory operations.
//! This function is passed to `lua_newstate()`.

use core::ffi::c_void;

/// Lua allocator that delegates to the kernel heap (heavenos_malloc/free/realloc).
///
/// Contract (identical to ANSI realloc):
/// - nsize == 0: free ptr, return NULL
/// - ptr == NULL: allocate nsize bytes (malloc)
/// - otherwise: reallocate ptr from osize to nsize (realloc)
pub unsafe extern "C" fn heaven_lua_alloc(
    _ud: *mut c_void,
    ptr: *mut c_void,
    _osize: usize,
    nsize: usize,
) -> *mut c_void {
    extern "C" {
        fn heavenos_malloc(size: usize) -> *mut u8;
        fn heavenos_free(ptr: *mut u8);
        fn heavenos_realloc(ptr: *mut u8, new_size: usize) -> *mut u8;
    }

    if nsize == 0 {
        if !ptr.is_null() {
            heavenos_free(ptr as *mut u8);
        }
        core::ptr::null_mut()
    } else if ptr.is_null() {
        heavenos_malloc(nsize) as *mut c_void
    } else {
        heavenos_realloc(ptr as *mut u8, nsize) as *mut c_void
    }
}
