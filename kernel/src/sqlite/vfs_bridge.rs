#![allow(non_snake_case, static_mut_refs)]
/// VFS bridge — registers a SQLite VFS that delegates to HeavenVfs.
///
/// SQLite expects a C struct (sqlite3_vfs) with function pointers for
/// file I/O. We build that struct and register it so sqlite3_open_v2()
/// with zVfs="heaven" routes all I/O through our NVMe-backed VFS.
///
/// The VFS global is stored in a static Mutex and set up during boot
/// (after NVMe + block allocator + file table are ready).
use core::ffi::{c_char, c_int, c_void};
use core::ptr;
use alloc::string::String;

use crate::vfs::HeavenVfs;

// ---- SQLite VFS structures (must match sqlite3.h exactly) ----

/// sqlite3_vfs — the VFS descriptor that SQLite uses to find our I/O methods.
#[repr(C)]
struct Sqlite3Vfs {
    iVersion: c_int,                    // Structure version (3)
    szOsFile: c_int,                    // Size of HeavenSqliteFile
    mxPathname: c_int,                  // Max pathname length
    pNext: *mut Sqlite3Vfs,             // Linked list (managed by SQLite)
    zName: *const c_char,               // VFS name: "heaven"
    pAppData: *mut c_void,              // Unused
    xOpen: Option<unsafe extern "C" fn(*mut Sqlite3Vfs, *const c_char, *mut Sqlite3File, c_int, *mut c_int) -> c_int>,
    xDelete: Option<unsafe extern "C" fn(*mut Sqlite3Vfs, *const c_char, c_int) -> c_int>,
    xAccess: Option<unsafe extern "C" fn(*mut Sqlite3Vfs, *const c_char, c_int, *mut c_int) -> c_int>,
    xFullPathname: Option<unsafe extern "C" fn(*mut Sqlite3Vfs, *const c_char, c_int, *mut c_char) -> c_int>,
    xDlOpen: Option<unsafe extern "C" fn(*mut Sqlite3Vfs, *const c_char) -> *mut c_void>,
    xDlError: Option<unsafe extern "C" fn(*mut Sqlite3Vfs, c_int, *mut c_char)>,
    xDlSym: Option<unsafe extern "C" fn(*mut Sqlite3Vfs, *mut c_void, *const c_char) -> Option<unsafe extern "C" fn()>>,
    xDlClose: Option<unsafe extern "C" fn(*mut Sqlite3Vfs, *mut c_void)>,
    xRandomness: Option<unsafe extern "C" fn(*mut Sqlite3Vfs, c_int, *mut c_char) -> c_int>,
    xSleep: Option<unsafe extern "C" fn(*mut Sqlite3Vfs, c_int) -> c_int>,
    xCurrentTime: Option<unsafe extern "C" fn(*mut Sqlite3Vfs, *mut f64) -> c_int>,
    xGetLastError: Option<unsafe extern "C" fn(*mut Sqlite3Vfs, c_int, *mut c_char) -> c_int>,
    // v2
    xCurrentTimeInt64: Option<unsafe extern "C" fn(*mut Sqlite3Vfs, *mut i64) -> c_int>,
    // v3
    xSetSystemCall: Option<unsafe extern "C" fn(*mut Sqlite3Vfs, *const c_char, *mut c_void) -> c_int>,
    xGetSystemCall: Option<unsafe extern "C" fn(*mut Sqlite3Vfs, *const c_char) -> *mut c_void>,
    xNextSystemCall: Option<unsafe extern "C" fn(*mut Sqlite3Vfs, *const c_char) -> *const c_char>,
}

/// sqlite3_io_methods — the per-file I/O method table.
#[repr(C)]
struct Sqlite3IoMethods {
    iVersion: c_int,
    xClose: Option<unsafe extern "C" fn(*mut Sqlite3File) -> c_int>,
    xRead: Option<unsafe extern "C" fn(*mut Sqlite3File, *mut c_void, c_int, i64) -> c_int>,
    xWrite: Option<unsafe extern "C" fn(*mut Sqlite3File, *const c_void, c_int, i64) -> c_int>,
    xTruncate: Option<unsafe extern "C" fn(*mut Sqlite3File, i64) -> c_int>,
    xSync: Option<unsafe extern "C" fn(*mut Sqlite3File, c_int) -> c_int>,
    xFileSize: Option<unsafe extern "C" fn(*mut Sqlite3File, *mut i64) -> c_int>,
    xLock: Option<unsafe extern "C" fn(*mut Sqlite3File, c_int) -> c_int>,
    xUnlock: Option<unsafe extern "C" fn(*mut Sqlite3File, c_int) -> c_int>,
    xCheckReservedLock: Option<unsafe extern "C" fn(*mut Sqlite3File, *mut c_int) -> c_int>,
    xFileControl: Option<unsafe extern "C" fn(*mut Sqlite3File, c_int, *mut c_void) -> c_int>,
    xSectorSize: Option<unsafe extern "C" fn(*mut Sqlite3File) -> c_int>,
    xDeviceCharacteristics: Option<unsafe extern "C" fn(*mut Sqlite3File) -> c_int>,
}

/// sqlite3_file header — the first field of every open file handle.
/// SQLite allocates szOsFile bytes and expects us to fill in pMethods.
#[repr(C)]
struct Sqlite3File {
    pMethods: *const Sqlite3IoMethods,
}

/// Our extended file handle — starts with Sqlite3File header, then our data.
#[repr(C)]
struct HeavenSqliteFile {
    base: Sqlite3File,
    file_table_index: usize,
    start_lba: u64,
    block_count: u64,
    byte_length: u64,
    block_size: u32,
}

// ---- SQLite constants ----

const SQLITE_OK: c_int = 0;
const SQLITE_IOERR: c_int = 10;
const SQLITE_NOTFOUND: c_int = 12;
const SQLITE_CANTOPEN: c_int = 14;
const SQLITE_OPEN_CREATE: c_int = 0x00000004;

// ---- Static VFS and I/O methods ----

/// VFS name (null-terminated).
static VFS_NAME: &[u8] = b"heaven\0";

/// The I/O methods table — shared by all open files.
static IO_METHODS: Sqlite3IoMethods = Sqlite3IoMethods {
    iVersion: 1,
    xClose: Some(heaven_close),
    xRead: Some(heaven_read),
    xWrite: Some(heaven_write),
    xTruncate: Some(heaven_truncate),
    xSync: Some(heaven_sync),
    xFileSize: Some(heaven_file_size),
    xLock: Some(heaven_lock),
    xUnlock: Some(heaven_unlock),
    xCheckReservedLock: Some(heaven_check_reserved_lock),
    xFileControl: Some(heaven_file_control),
    xSectorSize: Some(heaven_sector_size),
    xDeviceCharacteristics: Some(heaven_device_characteristics),
};

/// The VFS instance. Must be in static mutable memory because SQLite
/// modifies the pNext field.
static mut HEAVEN_VFS: Sqlite3Vfs = Sqlite3Vfs {
    iVersion: 3,
    szOsFile: core::mem::size_of::<HeavenSqliteFile>() as c_int,
    mxPathname: 256,
    pNext: ptr::null_mut(),
    zName: ptr::null(), // Set at registration time
    pAppData: ptr::null_mut(),
    xOpen: Some(heaven_open),
    xDelete: Some(heaven_delete),
    xAccess: Some(heaven_access),
    xFullPathname: Some(heaven_full_pathname),
    xDlOpen: None,
    xDlError: None,
    xDlSym: None,
    xDlClose: None,
    xRandomness: Some(heaven_randomness),
    xSleep: Some(heaven_sleep),
    xCurrentTime: Some(heaven_current_time),
    xGetLastError: Some(heaven_get_last_error),
    xCurrentTimeInt64: Some(heaven_current_time_int64),
    xSetSystemCall: None,
    xGetSystemCall: None,
    xNextSystemCall: None,
};

// ---- Registration ----

extern "C" {
    fn sqlite3_vfs_register(vfs: *mut Sqlite3Vfs, makeDflt: c_int) -> c_int;
}

/// Register the "heaven" VFS with SQLite.
pub fn register_vfs() -> Result<(), String> {
    unsafe {
        HEAVEN_VFS.zName = VFS_NAME.as_ptr() as *const c_char;
        let rc = sqlite3_vfs_register(&mut HEAVEN_VFS as *mut Sqlite3Vfs, 1);
        if rc != SQLITE_OK {
            return Err(alloc::format!("sqlite3_vfs_register failed: {}", rc));
        }
    }
    Ok(())
}

// ---- Helper: get HeavenVfs from the global VFS singleton ----

/// Access the global VFS. Returns None if not initialized.
fn with_vfs<F, R>(f: F) -> R
where
    F: FnOnce(&crate::vfs::HeavenVfs) -> R,
{
    // The HeavenVfs is stored in a global. We access it through the vfs module.
    // For now we use a static that's set up before SQLite init.
    unsafe {
        let vfs = VFS_INSTANCE.as_ref().expect("HeavenVfs not initialized");
        f(vfs)
    }
}

/// Global HeavenVfs pointer — set before register_vfs() is called.
static mut VFS_INSTANCE: Option<&'static HeavenVfs> = None;

/// Set the global VFS instance. Called from init code before sqlite::init().
///
/// # Safety
/// Must be called exactly once, with a reference that lives for 'static.
pub unsafe fn set_vfs_instance(vfs: &'static HeavenVfs) {
    unsafe { VFS_INSTANCE = Some(vfs); }
}

// ---- Helper: C string → byte slice ----

unsafe fn cstr_to_bytes(s: *const c_char) -> &'static [u8] {
    if s.is_null() {
        return b"";
    }
    let mut len = 0;
    unsafe {
        while *s.add(len) != 0 {
            len += 1;
        }
        core::slice::from_raw_parts(s as *const u8, len)
    }
}

// ---- VFS method implementations ----

unsafe extern "C" fn heaven_open(
    _vfs: *mut Sqlite3Vfs,
    zName: *const c_char,
    pFile: *mut Sqlite3File,
    flags: c_int,
    _pOutFlags: *mut c_int,
) -> c_int {
    let name = unsafe { cstr_to_bytes(zName) };
    if name.is_empty() {
        return SQLITE_CANTOPEN;
    }

    let result = with_vfs(|vfs| vfs.open(name, flags));
    match result {
        Ok(hfile) => {
            let file = pFile as *mut HeavenSqliteFile;
            unsafe {
                (*file).base.pMethods = &IO_METHODS;
                (*file).file_table_index = hfile.file_table_index;
                (*file).start_lba = hfile.start_lba;
                (*file).block_count = hfile.block_count;
                (*file).byte_length = hfile.byte_length;
                (*file).block_size = hfile.block_size;
            }
            SQLITE_OK
        }
        Err(e) => e,
    }
}

unsafe extern "C" fn heaven_close(pFile: *mut Sqlite3File) -> c_int {
    let file = pFile as *mut HeavenSqliteFile;
    let hfile = unsafe { heaven_file_to_vfs_file(&*file) };
    with_vfs(|vfs| vfs.close(&hfile))
}

unsafe extern "C" fn heaven_read(
    pFile: *mut Sqlite3File,
    buf: *mut c_void,
    iAmt: c_int,
    iOfst: i64,
) -> c_int {
    let file = pFile as *mut HeavenSqliteFile;
    let hfile = unsafe { heaven_file_to_vfs_file(&*file) };
    let slice = unsafe { core::slice::from_raw_parts_mut(buf as *mut u8, iAmt as usize) };
    with_vfs(|vfs| vfs.read(&hfile, slice, iOfst as u64))
}

unsafe extern "C" fn heaven_write(
    pFile: *mut Sqlite3File,
    buf: *const c_void,
    iAmt: c_int,
    iOfst: i64,
) -> c_int {
    let file = pFile as *mut HeavenSqliteFile;
    let data = unsafe { core::slice::from_raw_parts(buf as *const u8, iAmt as usize) };
    let mut hfile = unsafe { heaven_file_to_vfs_file(&*file) };
    let rc = with_vfs(|vfs| vfs.write(&mut hfile, data, iOfst as u64));
    // Write back updated metadata
    unsafe {
        (*file).byte_length = hfile.byte_length;
        (*file).block_count = hfile.block_count;
        (*file).start_lba = hfile.start_lba;
    }
    rc
}

unsafe extern "C" fn heaven_truncate(pFile: *mut Sqlite3File, size: i64) -> c_int {
    let file = pFile as *mut HeavenSqliteFile;
    let mut hfile = unsafe { heaven_file_to_vfs_file(&*file) };
    let rc = with_vfs(|vfs| vfs.truncate(&mut hfile, size as u64));
    unsafe { (*file).byte_length = hfile.byte_length; }
    rc
}

unsafe extern "C" fn heaven_sync(pFile: *mut Sqlite3File, _flags: c_int) -> c_int {
    let file = pFile as *const HeavenSqliteFile;
    let hfile = unsafe { heaven_file_to_vfs_file(&*file) };
    with_vfs(|vfs| vfs.sync(&hfile))
}

unsafe extern "C" fn heaven_file_size(pFile: *mut Sqlite3File, pSize: *mut i64) -> c_int {
    let file = pFile as *const HeavenSqliteFile;
    let hfile = unsafe { heaven_file_to_vfs_file(&*file) };
    match with_vfs(|vfs| vfs.file_size(&hfile)) {
        Ok(size) => {
            unsafe { *pSize = size as i64; }
            SQLITE_OK
        }
        Err(e) => e,
    }
}

unsafe extern "C" fn heaven_lock(_pFile: *mut Sqlite3File, _level: c_int) -> c_int {
    SQLITE_OK // Single process — locking is a no-op
}

unsafe extern "C" fn heaven_unlock(_pFile: *mut Sqlite3File, _level: c_int) -> c_int {
    SQLITE_OK
}

unsafe extern "C" fn heaven_check_reserved_lock(
    _pFile: *mut Sqlite3File,
    pResOut: *mut c_int,
) -> c_int {
    unsafe { *pResOut = 0; }
    SQLITE_OK
}

unsafe extern "C" fn heaven_file_control(
    _pFile: *mut Sqlite3File,
    _op: c_int,
    _pArg: *mut c_void,
) -> c_int {
    SQLITE_NOTFOUND // We don't handle any FCNTL
}

unsafe extern "C" fn heaven_sector_size(_pFile: *mut Sqlite3File) -> c_int {
    4096 // NVMe block size
}

unsafe extern "C" fn heaven_device_characteristics(_pFile: *mut Sqlite3File) -> c_int {
    0 // No special characteristics
}

unsafe extern "C" fn heaven_delete(
    _vfs: *mut Sqlite3Vfs,
    zName: *const c_char,
    _syncDir: c_int,
) -> c_int {
    let name = unsafe { cstr_to_bytes(zName) };
    with_vfs(|vfs| vfs.delete(name))
}

unsafe extern "C" fn heaven_access(
    _vfs: *mut Sqlite3Vfs,
    zName: *const c_char,
    _flags: c_int,
    pResOut: *mut c_int,
) -> c_int {
    let name = unsafe { cstr_to_bytes(zName) };
    let exists = with_vfs(|vfs| vfs.access(name));
    unsafe { *pResOut = if exists { 1 } else { 0 }; }
    SQLITE_OK
}

unsafe extern "C" fn heaven_full_pathname(
    _vfs: *mut Sqlite3Vfs,
    zName: *const c_char,
    nOut: c_int,
    zOut: *mut c_char,
) -> c_int {
    // Our "filesystem" has flat names — just copy the name.
    let name = unsafe { cstr_to_bytes(zName) };
    let copy_len = name.len().min((nOut - 1) as usize);
    unsafe {
        ptr::copy_nonoverlapping(name.as_ptr(), zOut as *mut u8, copy_len);
        *zOut.add(copy_len) = 0;
    }
    SQLITE_OK
}

unsafe extern "C" fn heaven_randomness(
    _vfs: *mut Sqlite3Vfs,
    nByte: c_int,
    zOut: *mut c_char,
) -> c_int {
    let buf = unsafe { core::slice::from_raw_parts_mut(zOut as *mut u8, nByte as usize) };
    with_vfs(|vfs| vfs.randomness(buf));
    nByte
}

unsafe extern "C" fn heaven_sleep(_vfs: *mut Sqlite3Vfs, microseconds: c_int) -> c_int {
    with_vfs(|vfs| vfs.sleep(microseconds as u64));
    microseconds
}

unsafe extern "C" fn heaven_current_time(
    _vfs: *mut Sqlite3Vfs,
    pTime: *mut f64,
) -> c_int {
    // Julian day as a floating-point number
    let ms = with_vfs(|vfs| vfs.current_time_ms());
    unsafe { *pTime = ms as f64 / 86_400_000.0; }
    SQLITE_OK
}

unsafe extern "C" fn heaven_current_time_int64(
    _vfs: *mut Sqlite3Vfs,
    pTime: *mut i64,
) -> c_int {
    let ms = with_vfs(|vfs| vfs.current_time_ms());
    unsafe { *pTime = ms; }
    SQLITE_OK
}

unsafe extern "C" fn heaven_get_last_error(
    _vfs: *mut Sqlite3Vfs,
    _nBuf: c_int,
    _zBuf: *mut c_char,
) -> c_int {
    SQLITE_OK
}

// ---- Helper: convert HeavenSqliteFile fields → HeavenFile ----

use crate::vfs::sqlite_vfs::HeavenFile;

fn heaven_file_to_vfs_file(file: &HeavenSqliteFile) -> HeavenFile {
    HeavenFile {
        file_table_index: file.file_table_index,
        start_lba: file.start_lba,
        block_count: file.block_count,
        byte_length: file.byte_length,
        block_size: file.block_size,
    }
}
