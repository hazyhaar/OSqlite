/// SQLite integration for HeavenOS.
///
/// This module provides:
/// - Raw FFI bindings to the C SQLite library
/// - A safe Rust wrapper for executing SQL
/// - VFS registration that connects SQLite to NVMe via our VFS
///
/// The VFS is registered at init time. After that, sqlite3_open_v2()
/// with zVfs="heaven" opens the system database backed by NVMe blocks.
mod ffi;
mod vfs_bridge;

use alloc::string::String;
use spin::Mutex;

use crate::vfs::HeavenVfs;

pub use ffi::SqliteDb;

/// Global SQLite database instance (opened once at boot).
pub static DB: Mutex<Option<SqliteDb>> = Mutex::new(None);

extern "C" {
    fn heaven_configure_malloc() -> core::ffi::c_int;
}

/// Initialize SQLite and open the system database.
///
/// `vfs` must be a reference with `'static` lifetime (typically a leaked
/// Box or a `static` global). It is stored and used for all subsequent
/// SQLite I/O.
///
/// Must be called after the VFS (block allocator + file table) is ready.
pub fn init(vfs: &'static HeavenVfs) -> Result<(), String> {
    // 1. Configure our memory allocator (must happen BEFORE sqlite3_initialize)
    let rc = unsafe { heaven_configure_malloc() };
    if rc != 0 {
        return Err(alloc::format!("heaven_configure_malloc failed: {}", rc));
    }

    // 2. Install the VFS instance (must happen before register_vfs / open)
    unsafe { vfs_bridge::set_vfs_instance(vfs); }

    // 3. Initialize SQLite library
    let rc = unsafe { ffi::sqlite3_initialize() };
    if rc != 0 {
        return Err(alloc::format!("sqlite3_initialize failed: {}", rc));
    }

    // 4. Register our VFS with SQLite
    vfs_bridge::register_vfs()?;

    // 5. Open the system database
    let db = SqliteDb::open("heaven.db")?;

    // 6. Create the namespace table if it doesn't exist
    db.exec(
        "CREATE TABLE IF NOT EXISTS namespace (\
            path    TEXT PRIMARY KEY, \
            type    TEXT NOT NULL, \
            content BLOB, \
            mode    INTEGER DEFAULT 420, \
            mtime   INTEGER DEFAULT 0\
        )",
    )?;

    *DB.lock() = Some(db);
    Ok(())
}

/// Execute a SQL statement and return results as formatted text.
pub fn exec_and_format(sql: &str) -> Result<String, String> {
    let guard = DB.lock();
    let db = guard.as_ref().ok_or_else(|| String::from("database not open"))?;
    db.exec_with_results(sql)
}
