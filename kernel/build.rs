/// HeavenOS kernel build script.
///
/// Compiles:
/// 1. SQLite 3.51.2 amalgamation + bare-metal stubs
/// 2. setjmp/longjmp assembly
/// 3. Lua 5.4.8 runtime + bare-metal stubs
fn main() {
    // Skip C compilation when building for the host target (unit tests).
    // The storage unit tests only exercise pure Rust bitmap/file-table logic
    // and don't need SQLite, the bare-metal stubs, or the kernel code model.
    let target = std::env::var("TARGET").unwrap_or_default();
    if !target.contains("heavenos") {
        // Host target (e.g., x86_64-unknown-linux-gnu) — skip bare-metal C code.
        return;
    }

    // Common flags for bare-metal x86_64 kernel code.
    // -fno-pic is critical: the cc crate may default to PIC, but
    // -mcmodel=kernel requires non-PIC code.
    let common_flags: &[&str] = &[
        "-ffreestanding",
        "-nostdlib",
        "-fno-stack-protector",
        "-fno-pic",
        "-fno-pie",
        "-mno-red-zone",
        "-mcmodel=kernel",
    ];

    // ---- SQLite amalgamation + stubs ----
    let mut cc_sqlite = cc::Build::new();
    cc_sqlite
        .file("vendor/sqlite/sqlite3.c")
        .file("vendor/sqlite/heaven_stubs.c")
        .include("vendor/sqlite")
        .flag("-include")
        .flag("vendor/sqlite/sqlite_config.h")
        .pic(false);
    for flag in common_flags {
        cc_sqlite.flag(flag);
    }
    cc_sqlite.warnings(false).flag("-w").compile("sqlite3");

    // ---- setjmp/longjmp assembly ----
    let mut asm = cc::Build::new();
    asm.file("vendor/sqlite/heaven_setjmp.S").pic(false);
    for flag in common_flags {
        asm.flag(flag);
    }
    asm.compile("heaven_setjmp");

    // ---- Lua 5.4.8 ----
    let lua_sources = [
        "lapi.c",
        "lauxlib.c",
        "lbaselib.c",
        "lcode.c",
        "lcorolib.c",
        "lctype.c",
        "ldebug.c",
        "ldo.c",
        "ldump.c",
        "lfunc.c",
        "lgc.c",
        "llex.c",
        "lmathlib.c",
        "lmem.c",
        "lobject.c",
        "lopcodes.c",
        "lparser.c",
        "lstate.c",
        "lstring.c",
        "lstrlib.c",
        "ltable.c",
        "ltablib.c",
        "ltm.c",
        "lundump.c",
        "lutf8lib.c",
        "lvm.c",
        "lzio.c",
        // Our replacements:
        "linit_heaven.c",
        "heaven_lua_stubs.c",
    ];

    let mut cc_lua = cc::Build::new();
    cc_lua
        .include("vendor/lua")
        .include("vendor/sqlite") // for access to shared stubs (strlen, etc.)
        // Inject our config header before luaconf.h
        .define("LUA_USER_H", "\"luaconf_heaven.h\"")
        .pic(false)
        // SSE2 enabled — Lua needs double precision floats
        .flag("-msse2")
        .flag("-mno-sse3")
        // Disable glibc fortification (_chk variants) — we are bare-metal
        .flag("-U_FORTIFY_SOURCE")
        .flag("-D_FORTIFY_SOURCE=0");
    for flag in common_flags {
        cc_lua.flag(flag);
    }
    for src in &lua_sources {
        cc_lua.file(format!("vendor/lua/{}", src));
    }
    cc_lua.warnings(false).flag("-w").compile("lua54");

    // Tell cargo to re-run if vendor files change
    println!("cargo:rerun-if-changed=vendor/sqlite/sqlite3.c");
    println!("cargo:rerun-if-changed=vendor/sqlite/sqlite3.h");
    println!("cargo:rerun-if-changed=vendor/sqlite/sqlite_config.h");
    println!("cargo:rerun-if-changed=vendor/sqlite/heaven_stubs.c");
    println!("cargo:rerun-if-changed=vendor/sqlite/heaven_setjmp.S");
    println!("cargo:rerun-if-changed=vendor/lua");
}
