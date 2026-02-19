/// NVMe command construction and PRP list building.
use core::fmt;
use crate::mem::{PhysAddr, DmaBuf};

/// NVMe admin command opcodes.
#[repr(u8)]
#[derive(Debug, Clone, Copy)]
pub enum AdminOpcode {
    DeleteIoSq = 0x00,
    CreateIoSq = 0x01,
    DeleteIoCq = 0x04,
    CreateIoCq = 0x05,
    Identify = 0x06,
}

/// NVMe NVM I/O command opcodes.
#[repr(u8)]
#[derive(Debug, Clone, Copy)]
pub enum NvmOpcode {
    Flush = 0x00,
    Write = 0x01,
    Read = 0x02,
}

/// NVMe error types.
#[derive(Debug)]
pub enum NvmeError {
    /// Controller reported fatal status (CSTS.CFS).
    ControllerFatal,
    /// Command timed out waiting for completion.
    Timeout,
    /// NVMe command completed with non-zero status.
    CommandFailed(u16),
    /// No NVMe device found during PCI enumeration.
    DeviceNotFound,
    /// Driver not initialized yet.
    NotInitialized,
    /// DMA memory allocation failed.
    OutOfMemory,
    /// Media error — unrecoverable read/write failure.
    MediaError,
}

impl fmt::Display for NvmeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NvmeError::ControllerFatal => write!(f, "NVMe controller fatal error"),
            NvmeError::Timeout => write!(f, "NVMe command timeout"),
            NvmeError::CommandFailed(s) => write!(f, "NVMe command failed: status {:#x}", s),
            NvmeError::DeviceNotFound => write!(f, "NVMe device not found"),
            NvmeError::NotInitialized => write!(f, "NVMe driver not initialized"),
            NvmeError::OutOfMemory => write!(f, "NVMe DMA allocation failed"),
            NvmeError::MediaError => write!(f, "NVMe media error"),
        }
    }
}

/// Classify an NVMe completion status into an error category.
pub fn classify_status(status: u16) -> Result<(), NvmeError> {
    if status == 0 {
        return Ok(());
    }

    let sct = (status >> 9) & 0x7; // Status Code Type
    let sc = (status >> 1) & 0xFF;  // Status Code

    match sct {
        0 => {
            // Generic Command Status
            match sc {
                0x00 => Ok(()),                          // Successful Completion
                0x01 => Err(NvmeError::CommandFailed(status)), // Invalid Command Opcode
                0x02 => Err(NvmeError::CommandFailed(status)), // Invalid Field
                0x80 => Err(NvmeError::MediaError),      // LBA Out of Range
                _ => Err(NvmeError::CommandFailed(status)),
            }
        }
        2 => {
            // Media and Data Integrity Errors
            Err(NvmeError::MediaError)
        }
        _ => Err(NvmeError::CommandFailed(status)),
    }
}

/// SQLite error codes returned from VFS operations.
/// These mirror the sqlite3 error codes we need.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SqliteIoError {
    Ok = 0,
    Error = 1,
    IoErrRead = 266,       // SQLITE_IOERR_READ
    IoErrWrite = 778,      // SQLITE_IOERR_WRITE
    IoErrFsync = 1034,     // SQLITE_IOERR_FSYNC
    IoErrTruncate = 1546,  // SQLITE_IOERR_TRUNCATE
    IoErrDelete = 2570,    // SQLITE_IOERR_DELETE
    Full = 13,             // SQLITE_FULL
    Busy = 5,              // SQLITE_BUSY
    CantOpen = 14,         // SQLITE_CANTOPEN
    CorruptFs = 1290,      // SQLITE_IOERR_CORRUPTFS
}

/// Map NVMe error to SQLite I/O error code.
pub fn nvme_to_sqlite_error(err: &NvmeError) -> SqliteIoError {
    match err {
        NvmeError::ControllerFatal => SqliteIoError::Error,
        NvmeError::Timeout => SqliteIoError::Busy,
        NvmeError::CommandFailed(_) => SqliteIoError::IoErrRead,
        NvmeError::DeviceNotFound => SqliteIoError::CantOpen,
        NvmeError::NotInitialized => SqliteIoError::CantOpen,
        NvmeError::OutOfMemory => SqliteIoError::Full,
        NvmeError::MediaError => SqliteIoError::CorruptFs,
    }
}

/// Build PRP entries for a DMA buffer.
///
/// NVMe PRP rules:
/// - If transfer fits in one page (≤ 4096 bytes): prp1 = phys_addr, prp2 = 0
/// - If transfer fits in two pages (≤ 8192 bytes): prp1 = first page, prp2 = second page
/// - If transfer spans more than two pages: prp1 = first page, prp2 = pointer to PRP list
///
/// For simplicity, since our DMA buffers are physically contiguous, prp2 is
/// just phys_addr + 4096 for the two-page case. For > 2 pages with contiguous
/// memory, we could use a PRP list, but contiguous buffers allow direct offsets.
pub fn build_prp(buf: &DmaBuf, transfer_size: usize) -> (PhysAddr, PhysAddr) {
    let base = buf.phys_addr();
    let page_size: u64 = 4096;

    if transfer_size <= page_size as usize {
        // Single page: prp2 unused
        (base, PhysAddr::new(0))
    } else if transfer_size <= (page_size * 2) as usize {
        // Two pages: prp2 = second page
        (base, PhysAddr::new(base.as_u64() + page_size))
    } else {
        // More than two pages with contiguous memory.
        // We MUST build a PRP list in a separate page.
        // The PRP list contains physical addresses of pages 1..N
        // (page 0 is in PRP1).
        //
        // For now, since our buffers are contiguous, we build the list
        // with sequential addresses. A more general implementation would
        // handle scattered pages.
        //
        // IMPORTANT: This creates a PRP list as a static-lifetime leak.
        // In production, the PRP list buffer should be managed alongside
        // the command lifecycle. For initial implementation, we accept this.
        build_prp_contiguous(base, transfer_size)
    }
}

/// Build PRP list for a contiguous buffer spanning > 2 pages.
fn build_prp_contiguous(base: PhysAddr, transfer_size: usize) -> (PhysAddr, PhysAddr) {
    let page_size: u64 = 4096;
    let num_pages = (transfer_size as u64 + page_size - 1) / page_size;

    // Allocate a page for the PRP list itself.
    // Each entry is a u64 (8 bytes), so one page holds 512 entries.
    // That's enough for 512 * 4096 = 2 MB transfers.
    let prp_list = DmaBuf::alloc(page_size as usize)
        .expect("failed to allocate PRP list page");

    let list_ptr = prp_list.as_mut_ptr() as *mut u64;

    // Fill PRP list with addresses of pages 1..N-1
    for i in 1..num_pages {
        unsafe {
            core::ptr::write_volatile(
                list_ptr.add((i - 1) as usize),
                base.as_u64() + i * page_size,
            );
        }
    }

    prp_list.flush_cache();

    let prp2 = prp_list.phys_addr();

    // Leak the PRP list — it must survive until the command completes.
    // TODO: proper lifecycle management via a PRP list pool.
    core::mem::forget(prp_list);

    (base, prp2)
}

/// NVMe command wrapper for the public API.
/// This is a higher-level representation; the driver converts it
/// to SubmissionEntry internally.
pub struct NvmeCommand {
    pub opcode: NvmOpcode,
    pub nsid: u32,
    pub lba: u64,
    pub block_count: u16,
    pub prp1: PhysAddr,
    pub prp2: PhysAddr,
}
