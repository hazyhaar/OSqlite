# Audit Report — Phase 3: Lua 5.4.8 Embedding

**Date**: 2026-02-20
**Scope**: All files created/modified in Phase 3 (Lua embedding commit `2b74e85`)

---

## Summary

| Severity | Count | Fixed |
|----------|-------|-------|
| P0       | 3     | 3     |
| P1       | 4     | 4     |
| P2       | 5     | 2     |

---

## P0 — Critical

### 1. Multiline script truncation in `load_script_from_db()`

**File**: `kernel/src/lua/mod.rs:91`
**Issue**: `exec_with_results()` returns pipe-delimited columns with `\n` row separators. `load_script_from_db()` splits on newlines and takes only `lines[1]`, truncating multiline Lua scripts to their first line. This makes the entire agent system non-functional for any real script.
**Fix**: Join all lines after the header with `\n` to reconstruct multiline content.

### 2. `read()` builtin truncates multiline content

**File**: `kernel/src/lua/builtins.rs:155`
**Issue**: Same pattern — `lua_read()` takes only `lines[1]` from exec_with_results output. Content with embedded newlines is truncated.
**Fix**: Join `lines[1..]` with newlines.

### 3. `cat` shell command truncates multiline content

**File**: `kernel/src/shell/commands.rs:269`
**Issue**: `cmd_cat()` displays only `lines[1]`, truncating multiline namespace entries.
**Fix**: Join and display all content lines.

---

## P1 — Important

### 4. No Lua memory limit

**File**: `kernel/src/lua/alloc.rs`
**Issue**: The allocator has no cap on how much heap a Lua state can consume. A malicious or buggy script (`while true do t = {t} end`) can exhaust the kernel heap, crashing the system.
**Fix**: Add a per-state byte counter with a configurable limit (default 1 MB). Return NULL when exceeded.

### 5. No execution timeout for Lua scripts

**File**: `kernel/src/lua/mod.rs`
**Issue**: A `while true do end` loop hangs the kernel forever. There is no Lua debug hook or instruction counter to bound execution time.
**Fix**: Install a `lua_sethook` with `LUA_MASKCOUNT` that checks elapsed TSC ticks and calls `luaL_error()` after a timeout (default 30 seconds).

### 6. `sql()` gives unrestricted database access

**File**: `kernel/src/lua/builtins.rs:60`
**Issue**: Any Lua script can execute arbitrary SQL including `DROP TABLE`, `DELETE FROM audit`, `ALTER TABLE`. A malicious script can destroy the namespace, wipe its audit trail, and corrupt system state. No access control or sandboxing.
**Fix**: Add a read-only mode flag per Lua state. For untrusted scripts, restrict `sql()` to SELECT-only queries by checking the trimmed query prefix.

### 7. `sleep()` has no upper bound

**File**: `kernel/src/lua/builtins.rs:312`
**Issue**: `sleep(2^53)` busy-waits the kernel effectively forever.
**Fix**: Clamp sleep duration to a maximum (e.g., 60 seconds).

---

## P2 — Minor

### 8. `abs(INT_MIN)` is undefined behavior

**File**: `kernel/vendor/lua/heaven_lua_stubs.c:144`
**Issue**: `abs(-2147483648)` causes signed integer overflow (UB in C). Lua's `lcode.c` calls `abs()` on line offsets which won't be INT_MIN in practice.
**Fix**: Guard against INT_MIN.

### 9. toupper/tolower tables return 0 for chars 128-255

**File**: `kernel/vendor/lua/heaven_lua_stubs.c:277,327`
**Issue**: High-byte entries in `_heaven_toupper_table` and `_heaven_tolower_table` are initialized to 0 instead of identity values. `toupper(200)` returns 0 instead of 200.
**Fix**: Initialize high-byte range as identity values. (Deferred — Lua uses its own lctype for non-locale operations.)

### 10. `fmin`/`fmax` NaN handling incorrect

**File**: `kernel/vendor/lua/heaven_lua_stubs.c:427-428`
**Issue**: `fmin(x, NaN)` returns NaN instead of x. C99/IEEE 754 requires the non-NaN argument to be returned.
**Fix**: Add NaN checks.

### 11. `store` command limited to single-line scripts

**File**: `kernel/src/shell/commands.rs:96`
**Issue**: The `store <path> <code>` shell command joins remaining arguments with spaces, making it impossible to enter multiline scripts. Users must use `sql INSERT` directly.
**Note**: Acceptable limitation for Phase 3. Multiline `store` can be added later.

### 12. `sql()` result parsing breaks on pipe characters

**File**: `kernel/src/lua/builtins.rs:79`
**Issue**: Column values containing `|` are split incorrectly by the pipe-delimited parser. Inherent limitation of the text-based `exec_with_results()` format.
**Note**: Deferred — requires switching to prepared statement API for structured results.

---

## Files Audited

- `kernel/vendor/lua/heaven_lua_stubs.c` — C libc stubs
- `kernel/vendor/lua/luaconf_heaven.h` — Lua configuration
- `kernel/vendor/lua/linit_heaven.c` — Filtered library init
- `kernel/src/lua/ffi.rs` — Lua C API FFI bindings
- `kernel/src/lua/alloc.rs` — Allocator bridge
- `kernel/src/lua/mod.rs` — Lua runner
- `kernel/src/lua/builtins.rs` — Lua builtin functions
- `kernel/src/lua/repl.rs` — Interactive REPL
- `kernel/src/shell/commands.rs` — Shell command wiring
- `kernel/src/shell/line.rs` — Line editor (Ctrl-D)
- `kernel/src/shell/mod.rs` — Module visibility
- `kernel/src/sqlite/mod.rs` — Audit table init
- `kernel/build.rs` — Lua compilation
