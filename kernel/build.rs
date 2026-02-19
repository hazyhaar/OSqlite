/// HeavenOS kernel build script.
///
/// Compiles the SQLite 3.51.2 amalgamation + bare-metal stubs + setjmp asm
/// as a static C library linked into the kernel.
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
    let mut cc = cc::Build::new();
    cc.file("vendor/sqlite/sqlite3.c")
        .file("vendor/sqlite/heaven_stubs.c")
        .include("vendor/sqlite")
        .flag("-include")
        .flag("vendor/sqlite/sqlite_config.h")
        .pic(false);
    for flag in common_flags {
        cc.flag(flag);
    }
    cc.warnings(false)
        .flag("-w")
        .compile("sqlite3");

    // ---- setjmp/longjmp assembly ----
    let mut asm = cc::Build::new();
    asm.file("vendor/sqlite/heaven_setjmp.S")
        .pic(false);
    for flag in common_flags {
        asm.flag(flag);
    }
    asm.compile("heaven_setjmp");

    // Tell cargo to re-run if vendor files change
    println!("cargo:rerun-if-changed=vendor/sqlite/sqlite3.c");
    println!("cargo:rerun-if-changed=vendor/sqlite/sqlite3.h");
    println!("cargo:rerun-if-changed=vendor/sqlite/sqlite_config.h");
    println!("cargo:rerun-if-changed=vendor/sqlite/heaven_stubs.c");
    println!("cargo:rerun-if-changed=vendor/sqlite/heaven_setjmp.S");
}
