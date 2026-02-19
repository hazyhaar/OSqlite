/// HeavenOS SQLite VFS — the bridge between SQLite and bare-metal NVMe.
///
/// This module implements the ~20 methods that SQLite requires from a VFS.
/// SQLite sees "files" (main.db, .wal, .shm, .journal); the VFS translates
/// every file operation into NVMe block reads/writes via the block allocator.
///
/// Key design decisions:
/// - xRead: always reads full blocks, copies the requested byte range
/// - xWrite: Read-Modify-Write for partial-block writes, fast path for aligned
/// - xSync: bitmap flush + file table flush + NVMe Flush command = ACID
/// - xShm*: RAM-backed (trivial in a single-address-space kernel)
use core::ffi::c_int;
use core::sync::atomic::Ordering;

use alloc::vec::Vec;
use spin::Mutex;

use crate::drivers::nvme::NVME;
use crate::mem::DmaBuf;
use crate::storage::{BlockAllocator, FileTable};

// ---- SQLite constants (from sqlite3.h) ----

const SQLITE_OK: c_int = 0;
const SQLITE_ERROR: c_int = 1;
const SQLITE_BUSY: c_int = 5;
const SQLITE_IOERR: c_int = 10;
const SQLITE_FULL: c_int = 13;
const SQLITE_CANTOPEN: c_int = 14;

const SQLITE_IOERR_READ: c_int = 266;
const SQLITE_IOERR_SHORT_READ: c_int = 522;
const SQLITE_IOERR_WRITE: c_int = 778;
const SQLITE_IOERR_FSYNC: c_int = 1034;
const SQLITE_IOERR_TRUNCATE: c_int = 1546;
const SQLITE_IOERR_DELETE: c_int = 2570;
const SQLITE_IOERR_NOMEM: c_int = 3082;

const SQLITE_OPEN_MAIN_DB: c_int = 0x00000100;
const SQLITE_OPEN_MAIN_JOURNAL: c_int = 0x00000800;
const SQLITE_OPEN_TEMP_DB: c_int = 0x00000200;
const SQLITE_OPEN_TEMP_JOURNAL: c_int = 0x00001000;
const SQLITE_OPEN_SUBJOURNAL: c_int = 0x00002000;
const SQLITE_OPEN_WAL: c_int = 0x00080000;
const SQLITE_OPEN_CREATE: c_int = 0x00000004;

const SQLITE_FCNTL_SIZE_HINT: c_int = 5;
const SQLITE_FCNTL_CHUNK_SIZE: c_int = 6;
const SQLITE_FCNTL_PRAGMA: c_int = 14;

const SQLITE_SHM_NLOCK: usize = 8;
const SQLITE_SHM_LOCK: c_int = 2;
const SQLITE_SHM_UNLOCK: c_int = 1;
const SQLITE_SHM_SHARED: c_int = 4;
const SQLITE_SHM_EXCLUSIVE: c_int = 8;

// ---- Internal file handle ----

/// Default initial allocation for a new file (in blocks).
const INITIAL_ALLOC_BLOCKS: u64 = 16; // 64 KiB at 4096 block size

/// Per-open-file state. Stored alongside the sqlite3_file header.
pub struct HeavenFile {
    /// Index into the file table.
    pub file_table_index: usize,
    /// Cached start LBA (absolute, not data-block index).
    pub start_lba: u64,
    /// Cached block count.
    pub block_count: u64,
    /// Cached byte length.
    pub byte_length: u64,
    /// Block size (from NVMe).
    pub block_size: u32,
}

// ---- Shared Memory for WAL ----

/// WAL shared memory state.
struct ShmState {
    regions: Vec<(*mut u8, usize)>,
    locks: [ShmLockState; SQLITE_SHM_NLOCK],
}

unsafe impl Send for ShmState {}

#[derive(Clone, Copy, Default)]
struct ShmLockState {
    shared_count: u32,
    exclusive: bool,
}

impl ShmLockState {
    fn try_shared(&mut self) -> bool {
        if self.exclusive { return false; }
        self.shared_count += 1;
        true
    }

    fn try_exclusive(&mut self) -> bool {
        if self.exclusive || self.shared_count > 0 { return false; }
        self.exclusive = true;
        true
    }

    fn release_shared(&mut self) {
        debug_assert!(self.shared_count > 0);
        self.shared_count -= 1;
    }

    fn release_exclusive(&mut self) {
        debug_assert!(self.exclusive);
        self.exclusive = false;
    }
}

static SHM: Mutex<Option<ShmState>> = Mutex::new(None);

// ---- Main VFS Implementation ----

/// The HeavenOS VFS — holds references to block allocator and file table.
pub struct HeavenVfs {
    allocator: Mutex<BlockAllocator>,
    file_table: Mutex<FileTable>,
}

impl HeavenVfs {
    /// Create a new VFS backed by a block allocator and file table.
    pub fn new(allocator: BlockAllocator, file_table: FileTable) -> Self {
        Self {
            allocator: Mutex::new(allocator),
            file_table: Mutex::new(file_table),
        }
    }

    // ---- xOpen ----

    /// Open a file. Creates it if SQLITE_OPEN_CREATE is set and it doesn't exist.
    /// Lock order: allocator → file_table (NVME not needed here).
    pub fn open(&self, name: &[u8], flags: c_int) -> Result<HeavenFile, c_int> {
        let mut alloc = self.allocator.lock();
        let mut ft = self.file_table.lock();

        let block_size = alloc.block_size();

        // Look up existing file
        if let Some((idx, entry)) = ft.lookup(name) {
            let start_lba = alloc.data_start_lba() + entry.start_block;
            return Ok(HeavenFile {
                file_table_index: idx,
                start_lba,
                block_count: entry.block_count,
                byte_length: entry.byte_length,
                block_size,
            });
        }

        // File doesn't exist — create if allowed
        if flags & SQLITE_OPEN_CREATE == 0 {
            return Err(SQLITE_CANTOPEN);
        }

        // Allocate initial blocks
        let start_block = alloc.alloc(INITIAL_ALLOC_BLOCKS)
            .map_err(|_| SQLITE_FULL)?;

        let idx = ft.create(name, start_block, INITIAL_ALLOC_BLOCKS)
            .ok_or(SQLITE_FULL)?;

        let start_lba = alloc.data_start_lba() + start_block;

        Ok(HeavenFile {
            file_table_index: idx,
            start_lba,
            block_count: INITIAL_ALLOC_BLOCKS,
            byte_length: 0,
            block_size,
        })
    }

    // ---- xClose ----

    pub fn close(&self, file: &HeavenFile) -> c_int {
        // Sync the file table entry with the cached byte_length.
        let mut ft = self.file_table.lock();
        if let Some(entry) = ft.get_mut(file.file_table_index) {
            entry.byte_length = file.byte_length;
        }
        SQLITE_OK
    }

    // ---- xRead ----

    /// Read `amount` bytes at `offset` from the file into `buf`.
    ///
    /// Strategy: read full blocks from NVMe, copy the requested byte range.
    pub fn read(
        &self,
        file: &HeavenFile,
        buf: &mut [u8],
        offset: u64,
    ) -> c_int {
        let amount = buf.len();
        let bs = file.block_size as u64;

        // Short read: if reading past end-of-file, zero-fill
        if offset >= file.byte_length {
            buf.fill(0);
            return SQLITE_IOERR_SHORT_READ;
        }

        let available = (file.byte_length - offset) as usize;
        let to_read = amount.min(available);

        let start_block = offset / bs;
        let end_block = (offset + to_read as u64 - 1) / bs;
        let block_count = end_block - start_block + 1;

        // Bounds check
        if start_block + block_count > file.block_count {
            buf.fill(0);
            return SQLITE_IOERR_SHORT_READ;
        }

        let start_lba = file.start_lba + start_block;

        let mut nvme_guard = NVME.lock();
        let nvme = match nvme_guard.as_mut() {
            Some(n) => n,
            None => return SQLITE_IOERR,
        };

        let dma_size = (block_count as usize) * file.block_size as usize;
        let mut dma = match DmaBuf::alloc(dma_size) {
            Ok(d) => d,
            Err(_) => return SQLITE_IOERR_NOMEM,
        };

        // NVMe read
        if let Err(_) = nvme.read_blocks(start_lba, block_count as u16, &mut dma) {
            return SQLITE_IOERR_READ;
        }

        // Copy the requested byte range
        let byte_offset_in_first_block = (offset % bs) as usize;
        dma.copy_to_slice(&mut buf[..to_read], byte_offset_in_first_block, to_read);

        // Zero-fill remainder if short read
        if to_read < amount {
            buf[to_read..].fill(0);
            return SQLITE_IOERR_SHORT_READ;
        }

        SQLITE_OK
    }

    // ---- xWrite ----

    /// Write `data` at `offset` to the file.
    ///
    /// Strategy:
    /// - Aligned writes: DMA directly
    /// - Partial-block writes: Read-Modify-Write
    pub fn write(
        &self,
        file: &mut HeavenFile,
        data: &[u8],
        offset: u64,
    ) -> c_int {
        let amount = data.len();
        let bs = file.block_size as u64;

        let start_block = offset / bs;
        let end_block = (offset + amount as u64 - 1) / bs;
        let block_count = end_block - start_block + 1;

        // Grow file if needed.
        // Lock order: NVME → allocator → file_table.
        if start_block + block_count > file.block_count {
            let needed = start_block + block_count;

            // Step 1: Take NVME lock first (consistent lock ordering).
            let mut nvme_guard = NVME.lock();
            let nvme = match nvme_guard.as_mut() {
                Some(n) => n,
                None => return SQLITE_IOERR,
            };

            // Step 2: Take allocator lock.
            let mut alloc = self.allocator.lock();

            // Try to allocate a new contiguous region and relocate.
            // Crash-safe ordering:
            //   1. Alloc new region
            //   2. Copy old data → new region
            //   3. NVMe Flush (new data durable)
            //   4. Update file table to point to new region
            //   5. Free old blocks (safe: file table already points to new region)
            match alloc.alloc(needed) {
                Ok(new_start_block) => {
                    let old_data_start = file.start_lba;
                    let old_start_block = file.start_lba - alloc.data_start_lba();
                    let old_block_count = file.block_count;
                    let new_data_start = alloc.data_start_lba() + new_start_block;

                    // Copy existing blocks to new region
                    let copy_bs = file.block_size as usize;
                    if let Ok(mut tmp) = DmaBuf::alloc(copy_bs) {
                        for blk in 0..old_block_count {
                            if nvme.read_blocks(old_data_start + blk, 1, &mut tmp).is_err() {
                                alloc.free(new_start_block, needed);
                                return SQLITE_IOERR_READ;
                            }
                            if nvme.write_blocks(new_data_start + blk, 1, &tmp).is_err() {
                                alloc.free(new_start_block, needed);
                                return SQLITE_IOERR_WRITE;
                            }
                        }
                    } else {
                        alloc.free(new_start_block, needed);
                        return SQLITE_IOERR_NOMEM;
                    }

                    // NVMe Flush to ensure new copies are durable
                    if nvme.flush().is_err() {
                        alloc.free(new_start_block, needed);
                        return SQLITE_IOERR_FSYNC;
                    }

                    // Update metadata BEFORE freeing old blocks
                    file.start_lba = new_data_start;
                    file.block_count = needed;

                    let mut ft = self.file_table.lock();
                    if let Some(entry) = ft.get_mut(file.file_table_index) {
                        entry.start_block = new_start_block;
                        entry.block_count = needed;
                    }
                    drop(ft);

                    // Free old blocks (now safe)
                    alloc.free(old_start_block, old_block_count);
                }
                Err(_) => {
                    return SQLITE_FULL;
                }
            }
            drop(alloc);
            drop(nvme_guard);
        }

        let start_lba = file.start_lba + start_block;
        let byte_offset_in_first_block = (offset % bs) as usize;
        let is_aligned = byte_offset_in_first_block == 0 && amount % (bs as usize) == 0;

        let mut nvme_guard = NVME.lock();
        let nvme = match nvme_guard.as_mut() {
            Some(n) => n,
            None => return SQLITE_IOERR,
        };

        let dma_size = (block_count as usize) * file.block_size as usize;

        if is_aligned {
            // Fast path: direct write
            let mut dma = match DmaBuf::alloc(dma_size) {
                Ok(d) => d,
                Err(_) => return SQLITE_IOERR_NOMEM,
            };
            dma.copy_from_slice(data);

            if let Err(_) = nvme.write_blocks(start_lba, block_count as u16, &dma) {
                return SQLITE_IOERR_WRITE;
            }
        } else {
            // Slow path: Read-Modify-Write
            let mut dma = match DmaBuf::alloc(dma_size) {
                Ok(d) => d,
                Err(_) => return SQLITE_IOERR_NOMEM,
            };

            // 1. READ existing blocks
            if let Err(_) = nvme.read_blocks(start_lba, block_count as u16, &mut dma) {
                return SQLITE_IOERR_READ;
            }

            // 2. MODIFY: overlay the new data
            let dst = dma.as_mut_slice();
            dst[byte_offset_in_first_block..byte_offset_in_first_block + amount]
                .copy_from_slice(data);

            // 3. WRITE back
            if let Err(_) = nvme.write_blocks(start_lba, block_count as u16, &dma) {
                return SQLITE_IOERR_WRITE;
            }
        }

        // Update file byte length
        let new_end = offset + amount as u64;
        if new_end > file.byte_length {
            file.byte_length = new_end;
        }

        SQLITE_OK
    }

    // ---- xSync — THE ACID GUARANTEE ----

    /// Flush all dirty metadata and issue NVMe Flush.
    ///
    /// This is the function that makes SQLite's WAL commit durable.
    /// Without the NVMe Flush command, the device's volatile write cache
    /// may reorder or lose writes on power loss.
    pub fn sync(&self, file: &HeavenFile) -> c_int {
        // Hold all three locks for the entire sync to ensure atomicity.
        // Lock order: NVME → allocator → file_table (consistent to prevent deadlock).
        let mut nvme_guard = NVME.lock();
        let nvme = match nvme_guard.as_mut() {
            Some(n) => n,
            None => return SQLITE_IOERR_FSYNC,
        };

        let mut alloc = self.allocator.lock();
        let mut ft = self.file_table.lock();

        // 1. Update file table entry
        if let Some(entry) = ft.get_mut(file.file_table_index) {
            entry.byte_length = file.byte_length;
        }

        // 2. Flush block allocator bitmap to disk
        if alloc.flush(nvme).is_err() {
            return SQLITE_IOERR_FSYNC;
        }

        // 3. Flush file table to disk
        if ft.flush(nvme).is_err() {
            return SQLITE_IOERR_FSYNC;
        }

        // 4. NVMe Flush — the critical barrier
        if nvme.flush().is_err() {
            return SQLITE_IOERR_FSYNC;
        }

        SQLITE_OK
    }

    // ---- xFileSize ----

    pub fn file_size(&self, file: &HeavenFile) -> Result<u64, c_int> {
        Ok(file.byte_length)
    }

    // ---- xTruncate ----

    pub fn truncate(&self, file: &mut HeavenFile, size: u64) -> c_int {
        if size > file.byte_length {
            return SQLITE_OK; // truncate to larger = no-op (SQLite behavior)
        }
        file.byte_length = size;

        // Release unused blocks back to the allocator.
        let bs = file.block_size as u64;
        let needed_blocks = if size == 0 {
            // Keep at least 1 block so the file retains a valid start LBA.
            1
        } else {
            (size + bs - 1) / bs
        };

        if needed_blocks < file.block_count {
            let mut alloc = self.allocator.lock();
            let old_start_block = file.start_lba - alloc.data_start_lba();
            let excess_start = old_start_block + needed_blocks;
            let excess_count = file.block_count - needed_blocks;
            alloc.free(excess_start, excess_count);
            file.block_count = needed_blocks;

            // Update file table entry
            let mut ft = self.file_table.lock();
            if let Some(entry) = ft.get_mut(file.file_table_index) {
                entry.block_count = needed_blocks;
                entry.byte_length = size;
            }
        }

        SQLITE_OK
    }

    // ---- xDelete ----

    /// Lock order: allocator → file_table (NVME not needed for metadata-only ops).
    pub fn delete(&self, name: &[u8]) -> c_int {
        let mut alloc = self.allocator.lock();
        let mut ft = self.file_table.lock();

        if let Some((idx, entry)) = ft.lookup(name) {
            let start_block = entry.start_block;
            let block_count = entry.block_count;

            ft.delete(idx);
            alloc.free(start_block, block_count);

            SQLITE_OK
        } else {
            // File doesn't exist — SQLite expects OK for deleting non-existent files
            SQLITE_OK
        }
    }

    // ---- xAccess ----

    pub fn access(&self, name: &[u8]) -> bool {
        let ft = self.file_table.lock();
        ft.lookup(name).is_some()
    }

    // ---- xShmMap ----

    pub fn shm_map(&self, region: usize, region_size: usize) -> Result<*mut u8, c_int> {
        let mut shm = SHM.lock();
        let state = shm.get_or_insert_with(|| ShmState {
            regions: Vec::new(),
            locks: [ShmLockState::default(); SQLITE_SHM_NLOCK],
        });

        // Extend regions if needed
        while state.regions.len() <= region {
            let layout = core::alloc::Layout::from_size_align(region_size, 4096)
                .map_err(|_| SQLITE_IOERR_NOMEM)?;
            let ptr = unsafe { alloc::alloc::alloc_zeroed(layout) };
            if ptr.is_null() {
                return Err(SQLITE_IOERR_NOMEM);
            }
            state.regions.push((ptr, region_size));
        }

        Ok(state.regions[region].0)
    }

    // ---- xShmLock ----

    pub fn shm_lock(&self, offset: usize, n: usize, flags: c_int) -> c_int {
        let mut shm = SHM.lock();
        let state = match shm.as_mut() {
            Some(s) => s,
            None => return SQLITE_ERROR,
        };

        let is_lock = flags & SQLITE_SHM_LOCK != 0;
        let is_exclusive = flags & SQLITE_SHM_EXCLUSIVE != 0;

        for i in offset..offset + n {
            if i >= SQLITE_SHM_NLOCK {
                return SQLITE_ERROR;
            }
            let lock = &mut state.locks[i];

            if is_lock {
                if is_exclusive {
                    if !lock.try_exclusive() {
                        // Rollback any locks we acquired in this call
                        for j in offset..i {
                            state.locks[j].release_exclusive();
                        }
                        return SQLITE_BUSY;
                    }
                } else {
                    if !lock.try_shared() {
                        for j in offset..i {
                            state.locks[j].release_shared();
                        }
                        return SQLITE_BUSY;
                    }
                }
            } else {
                // Unlock
                if is_exclusive {
                    lock.release_exclusive();
                } else {
                    lock.release_shared();
                }
            }
        }

        SQLITE_OK
    }

    // ---- xShmBarrier ----

    pub fn shm_barrier(&self) {
        core::sync::atomic::fence(Ordering::SeqCst);
    }

    // ---- xShmUnmap ----

    pub fn shm_unmap(&self, delete: bool) -> c_int {
        if delete {
            let mut shm = SHM.lock();
            if let Some(state) = shm.take() {
                for (ptr, size) in state.regions {
                    let layout = core::alloc::Layout::from_size_align(size, 4096).unwrap();
                    unsafe { alloc::alloc::dealloc(ptr, layout) };
                }
            }
        }
        SQLITE_OK
    }

    // ---- xSleep ----

    /// Sleep for `microseconds` using calibrated TSC busy-wait.
    pub fn sleep(&self, microseconds: u64) -> u64 {
        crate::arch::x86_64::timer::delay_us(microseconds);
        microseconds
    }

    // ---- xCurrentTimeInt64 ----

    /// Returns current time as Julian day in milliseconds.
    /// Reads year/month/day/hour/minute/second from CMOS RTC (ports 0x70/0x71).
    pub fn current_time_ms(&self) -> i64 {
        let (year, month, day, hour, minute, second) = read_cmos_rtc();

        // Convert to Julian Day Number using the standard formula.
        // JDN = (1461 * (Y + 4800 + (M - 14)/12)) / 4
        //     + (367 * (M - 2 - 12 * ((M - 14)/12))) / 12
        //     - (3 * ((Y + 4900 + (M - 14)/12) / 100)) / 4
        //     + D - 32075
        let y = year as i64;
        let m = month as i64;
        let d = day as i64;
        let a = (14 - m) / 12;
        let adj_y = y + 4800 - a;
        let adj_m = m + 12 * a - 3;
        let jdn = d + (153 * adj_m + 2) / 5 + 365 * adj_y + adj_y / 4 - adj_y / 100 + adj_y / 400 - 32045;

        // Convert to milliseconds since Julian epoch (noon UT).
        // Julian day starts at noon, so subtract 0.5 day and add time-of-day.
        let day_ms: i64 = 86_400_000;
        let time_ms = (hour as i64) * 3_600_000
            + (minute as i64) * 60_000
            + (second as i64) * 1_000;

        // SQLite xCurrentTimeInt64 expects ms since Julian day 0, noon.
        // jdn is the Julian day number at noon of the given date.
        // We subtract half a day (to get to midnight) then add time_ms.
        (jdn - 1) * day_ms + day_ms / 2 + time_ms
    }

    // ---- xRandomness ----

    /// Fill buffer with random bytes using RDRAND.
    pub fn randomness(&self, buf: &mut [u8]) {
        for byte in buf.iter_mut() {
            *byte = rdrand_u8();
        }
    }
}

// ---- CPU instruction helpers ----

fn rdrand_u64() -> u64 {
    let mut val: u64;
    unsafe {
        core::arch::asm!(
            "2:",
            "rdrand {val}",
            "jnc 2b",
            val = out(reg) val,
            options(nostack),
        );
    }
    val
}

fn rdrand_u8() -> u8 {
    (rdrand_u64() & 0xFF) as u8
}

// ---- CMOS RTC reader ----

use crate::arch::x86_64::{outb, inb};

/// Read a CMOS RTC register (0x00=sec, 0x02=min, 0x04=hour, 0x07=day, 0x08=month, 0x09=year).
fn cmos_read(reg: u8) -> u8 {
    outb(0x70, reg);
    inb(0x71)
}

/// Convert BCD-encoded byte to binary.
fn bcd_to_bin(val: u8) -> u8 {
    (val & 0x0F) + (val >> 4) * 10
}

/// Read date/time from CMOS RTC. Waits for a stable read (no update in progress).
/// Returns (year, month, day, hour, minute, second) in UTC.
fn read_cmos_rtc() -> (u32, u32, u32, u32, u32, u32) {
    // Wait until RTC is not updating (bit 7 of register 0x0A).
    while cmos_read(0x0A) & 0x80 != 0 {
        core::hint::spin_loop();
    }

    let sec = cmos_read(0x00);
    let min = cmos_read(0x02);
    let hour = cmos_read(0x04);
    let day = cmos_read(0x07);
    let month = cmos_read(0x08);
    let year = cmos_read(0x09);
    let century = cmos_read(0x32); // ACPI century register (may be 0)

    // Check status register B for BCD vs binary mode.
    let status_b = cmos_read(0x0B);
    let is_bcd = status_b & 0x04 == 0;

    let (sec, min, hour, day, month, year, century) = if is_bcd {
        (
            bcd_to_bin(sec),
            bcd_to_bin(min),
            bcd_to_bin(hour & 0x7F), // mask AM/PM bit
            bcd_to_bin(day),
            bcd_to_bin(month),
            bcd_to_bin(year),
            bcd_to_bin(century),
        )
    } else {
        (sec, min, hour & 0x7F, day, month, year, century)
    };

    // Handle 12-hour mode
    let hour = if status_b & 0x02 == 0 && hour & 0x80 != 0 {
        // 12-hour mode, PM flag set
        ((hour & 0x7F) % 12) + 12
    } else {
        hour
    };

    // Full 4-digit year
    let full_year = if century > 0 {
        century as u32 * 100 + year as u32
    } else {
        2000 + year as u32
    };

    (full_year, month as u32, day as u32, hour as u32, min as u32, sec as u32)
}
