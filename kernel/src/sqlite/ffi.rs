/// Raw FFI bindings to the SQLite C library.
///
/// Only the functions we actually use are declared here.
/// SQLite is compiled as a static library via build.rs and linked in.
use core::ffi::{c_char, c_int, c_void, CStr};
use alloc::string::String;
use alloc::vec::Vec;

// ---- SQLite return codes ----

pub const SQLITE_OK: c_int = 0;
pub const SQLITE_ROW: c_int = 100;
pub const SQLITE_DONE: c_int = 101;

// ---- Opaque types ----

#[repr(C)]
pub struct sqlite3 {
    _opaque: [u8; 0],
}

#[repr(C)]
pub struct sqlite3_stmt {
    _opaque: [u8; 0],
}

#[repr(C)]
pub struct sqlite3_vfs {
    _opaque: [u8; 0],
}

// ---- SQLite C API ----

extern "C" {
    pub fn sqlite3_initialize() -> c_int;

    pub fn sqlite3_open_v2(
        filename: *const c_char,
        ppDb: *mut *mut sqlite3,
        flags: c_int,
        zVfs: *const c_char,
    ) -> c_int;

    pub fn sqlite3_close(db: *mut sqlite3) -> c_int;

    pub fn sqlite3_exec(
        db: *mut sqlite3,
        sql: *const c_char,
        callback: Option<
            unsafe extern "C" fn(
                data: *mut c_void,
                ncols: c_int,
                values: *mut *mut c_char,
                names: *mut *mut c_char,
            ) -> c_int,
        >,
        data: *mut c_void,
        errmsg: *mut *mut c_char,
    ) -> c_int;

    pub fn sqlite3_errmsg(db: *mut sqlite3) -> *const c_char;

    pub fn sqlite3_free(ptr: *mut c_void);

    pub fn sqlite3_prepare_v2(
        db: *mut sqlite3,
        sql: *const c_char,
        nByte: c_int,
        ppStmt: *mut *mut sqlite3_stmt,
        pzTail: *mut *const c_char,
    ) -> c_int;

    pub fn sqlite3_step(stmt: *mut sqlite3_stmt) -> c_int;

    pub fn sqlite3_column_count(stmt: *mut sqlite3_stmt) -> c_int;

    pub fn sqlite3_column_text(stmt: *mut sqlite3_stmt, iCol: c_int) -> *const c_char;

    pub fn sqlite3_column_name(stmt: *mut sqlite3_stmt, iCol: c_int) -> *const c_char;

    pub fn sqlite3_column_type(stmt: *mut sqlite3_stmt, iCol: c_int) -> c_int;

    pub fn sqlite3_finalize(stmt: *mut sqlite3_stmt) -> c_int;
}

// Open flags
const SQLITE_OPEN_READWRITE: c_int = 0x00000002;
const SQLITE_OPEN_CREATE: c_int = 0x00000004;

// Column types
const SQLITE_NULL: c_int = 5;

/// Safe wrapper around a sqlite3 database connection.
pub struct SqliteDb {
    db: *mut sqlite3,
}

unsafe impl Send for SqliteDb {}

impl SqliteDb {
    /// Open a database file using our "heaven" VFS.
    pub fn open(name: &str) -> Result<Self, String> {
        let mut db: *mut sqlite3 = core::ptr::null_mut();

        // Null-terminated filename
        let mut name_buf = Vec::with_capacity(name.len() + 1);
        name_buf.extend_from_slice(name.as_bytes());
        name_buf.push(0);

        // Null-terminated VFS name
        let vfs_name = b"heaven\0";

        let rc = unsafe {
            sqlite3_open_v2(
                name_buf.as_ptr() as *const c_char,
                &mut db,
                SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE,
                vfs_name.as_ptr() as *const c_char,
            )
        };

        if rc != SQLITE_OK {
            let msg = if !db.is_null() {
                unsafe { errmsg_string(db) }
            } else {
                alloc::format!("sqlite3_open_v2 failed: {}", rc)
            };
            if !db.is_null() {
                unsafe { sqlite3_close(db); }
            }
            return Err(msg);
        }

        Ok(Self { db })
    }

    /// Execute a SQL statement (no results expected).
    pub fn exec(&self, sql: &str) -> Result<(), String> {
        let mut sql_buf = Vec::with_capacity(sql.len() + 1);
        sql_buf.extend_from_slice(sql.as_bytes());
        sql_buf.push(0);

        let mut errmsg: *mut c_char = core::ptr::null_mut();

        let rc = unsafe {
            sqlite3_exec(
                self.db,
                sql_buf.as_ptr() as *const c_char,
                None,
                core::ptr::null_mut(),
                &mut errmsg,
            )
        };

        if rc != SQLITE_OK {
            let msg = if !errmsg.is_null() {
                let s = unsafe { cstr_to_string(errmsg) };
                unsafe { sqlite3_free(errmsg as *mut c_void); }
                s
            } else {
                unsafe { errmsg_string(self.db) }
            };
            return Err(msg);
        }

        Ok(())
    }

    /// Execute a SQL statement and return formatted results.
    pub fn exec_with_results(&self, sql: &str) -> Result<String, String> {
        let mut sql_buf = Vec::with_capacity(sql.len() + 1);
        sql_buf.extend_from_slice(sql.as_bytes());
        sql_buf.push(0);

        let mut stmt: *mut sqlite3_stmt = core::ptr::null_mut();

        let rc = unsafe {
            sqlite3_prepare_v2(
                self.db,
                sql_buf.as_ptr() as *const c_char,
                sql_buf.len() as c_int,
                &mut stmt,
                core::ptr::null_mut(),
            )
        };

        if rc != SQLITE_OK {
            return Err(unsafe { errmsg_string(self.db) });
        }

        let ncols = unsafe { sqlite3_column_count(stmt) };
        let mut output = String::new();

        // Print column headers
        if ncols > 0 {
            for i in 0..ncols {
                if i > 0 {
                    output.push('|');
                }
                let name = unsafe { sqlite3_column_name(stmt, i) };
                if !name.is_null() {
                    output.push_str(&unsafe { cstr_to_string(name) });
                }
            }
            output.push('\n');
        }

        // Print rows
        loop {
            let step_rc = unsafe { sqlite3_step(stmt) };
            if step_rc == SQLITE_DONE {
                break;
            }
            if step_rc != SQLITE_ROW {
                let msg = unsafe { errmsg_string(self.db) };
                unsafe { sqlite3_finalize(stmt); }
                return Err(msg);
            }

            for i in 0..ncols {
                if i > 0 {
                    output.push('|');
                }
                let col_type = unsafe { sqlite3_column_type(stmt, i) };
                if col_type == SQLITE_NULL {
                    output.push_str("NULL");
                } else {
                    let text = unsafe { sqlite3_column_text(stmt, i) };
                    if !text.is_null() {
                        output.push_str(&unsafe { cstr_to_string(text) });
                    }
                }
            }
            output.push('\n');
        }

        unsafe { sqlite3_finalize(stmt); }

        // If no columns (DDL/DML), just show OK
        if ncols == 0 {
            output.push_str("OK\n");
        }

        Ok(output)
    }
}

impl Drop for SqliteDb {
    fn drop(&mut self) {
        if !self.db.is_null() {
            unsafe { sqlite3_close(self.db); }
        }
    }
}

/// Convert a C string pointer to a Rust String.
unsafe fn cstr_to_string(ptr: *const c_char) -> String {
    let cstr = unsafe { CStr::from_ptr(ptr) };
    String::from_utf8_lossy(cstr.to_bytes()).into_owned()
}

/// Get the error message from a sqlite3 handle.
unsafe fn errmsg_string(db: *mut sqlite3) -> String {
    let msg = unsafe { sqlite3_errmsg(db) };
    if msg.is_null() {
        String::from("unknown error")
    } else {
        unsafe { cstr_to_string(msg) }
    }
}
