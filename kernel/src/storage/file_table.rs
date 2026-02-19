/// On-disk file table — maps names to LBA ranges.
///
/// The file table lives in a single block immediately after the bitmap.
/// Each entry is 96 bytes (fixed). With a 4096-byte block, we get 42 entries.
///
/// This is NOT a general-purpose filesystem. It maps a small number of
/// well-known names (main.db, main.db-wal, main.db-shm, main.db-journal,
/// temp files) to contiguous block allocations on disk.
use crate::drivers::nvme::NvmeError;
use crate::mem::DmaBuf;
use super::block_device::BlockDevice;

/// Maximum file name length (including null terminator).
const MAX_NAME_LEN: usize = 64;

/// Maximum entries in the file table.
const MAX_ENTRIES: usize = 42;

/// A single file table entry — 96 bytes.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FileEntry {
    /// File name, null-terminated.
    pub name: [u8; MAX_NAME_LEN],
    /// Starting data-block index (NOT raw LBA — add data_start_lba).
    pub start_block: u64,
    /// Number of allocated blocks.
    pub block_count: u64,
    /// Actual byte length of the file (may be less than block_count * block_size).
    pub byte_length: u64,
    /// Flags: bit 0 = in_use, bit 1 = read_only
    pub flags: u32,
    /// Reserved for future use.
    _reserved: u32,
}

static_assertions::const_assert_eq!(core::mem::size_of::<FileEntry>(), 96);

impl FileEntry {
    pub const fn empty() -> Self {
        Self {
            name: [0u8; MAX_NAME_LEN],
            start_block: 0,
            block_count: 0,
            byte_length: 0,
            flags: 0,
            _reserved: 0,
        }
    }

    pub fn is_in_use(&self) -> bool {
        self.flags & 1 != 0
    }

    pub fn set_in_use(&mut self, used: bool) {
        if used {
            self.flags |= 1;
        } else {
            self.flags &= !1;
        }
    }

    /// Get the file name as a byte slice (up to the first null).
    pub fn name_bytes(&self) -> &[u8] {
        let len = self.name.iter().position(|&b| b == 0).unwrap_or(MAX_NAME_LEN);
        &self.name[..len]
    }

    /// Set the file name from a byte slice.
    pub fn set_name(&mut self, name: &[u8]) {
        let copy_len = name.len().min(MAX_NAME_LEN - 1);
        self.name[..copy_len].copy_from_slice(&name[..copy_len]);
        self.name[copy_len] = 0;
    }
}

/// In-memory file table, cached from disk.
pub struct FileTable {
    entries: [FileEntry; MAX_ENTRIES],
    file_table_lba: u64,
    block_size: u32,
    dirty: bool,
}

impl FileTable {
    /// Create an empty file table.
    pub fn new(file_table_lba: u64, block_size: u32) -> Self {
        Self {
            entries: [FileEntry::empty(); MAX_ENTRIES],
            file_table_lba,
            block_size,
            dirty: false,
        }
    }

    /// Load the file table from disk.
    pub fn load(
        dev: &mut dyn BlockDevice,
        file_table_lba: u64,
        block_size: u32,
    ) -> Result<Self, NvmeError> {
        let mut buf = DmaBuf::alloc(block_size as usize)
            .map_err(|_| NvmeError::OutOfMemory)?;
        dev.read_blocks(file_table_lba, 1, &mut buf)?;

        let mut table = Self::new(file_table_lba, block_size);
        let data = buf.as_slice();
        let entry_size = core::mem::size_of::<FileEntry>();

        for i in 0..MAX_ENTRIES {
            let offset = i * entry_size;
            if offset + entry_size <= data.len() {
                unsafe {
                    let src = data.as_ptr().add(offset) as *const FileEntry;
                    table.entries[i] = core::ptr::read(src);
                }
            }
        }

        Ok(table)
    }

    /// Flush the file table to disk if dirty.
    pub fn flush(&mut self, dev: &mut dyn BlockDevice) -> Result<(), NvmeError> {
        if !self.dirty {
            return Ok(());
        }

        let mut buf = DmaBuf::alloc(self.block_size as usize)
            .map_err(|_| NvmeError::OutOfMemory)?;

        let data = buf.as_mut_slice();
        data.fill(0);

        let entry_size = core::mem::size_of::<FileEntry>();
        for i in 0..MAX_ENTRIES {
            let offset = i * entry_size;
            if offset + entry_size <= data.len() {
                unsafe {
                    let src = &self.entries[i] as *const FileEntry as *const u8;
                    core::ptr::copy_nonoverlapping(src, data.as_mut_ptr().add(offset), entry_size);
                }
            }
        }

        dev.write_blocks(self.file_table_lba, 1, &buf)?;
        self.dirty = false;
        Ok(())
    }

    /// Look up a file by name. Returns the entry index and a reference.
    pub fn lookup(&self, name: &[u8]) -> Option<(usize, &FileEntry)> {
        for (i, entry) in self.entries.iter().enumerate() {
            if entry.is_in_use() && entry.name_bytes() == name {
                return Some((i, entry));
            }
        }
        None
    }

    /// Look up a file by name, returning a mutable reference.
    pub fn lookup_mut(&mut self, name: &[u8]) -> Option<(usize, &mut FileEntry)> {
        for (i, entry) in self.entries.iter_mut().enumerate() {
            if entry.is_in_use() && entry.name_bytes() == name {
                return Some((i, entry));
            }
        }
        None
    }

    /// Create a new file entry. Returns the index.
    pub fn create(
        &mut self,
        name: &[u8],
        start_block: u64,
        block_count: u64,
    ) -> Option<usize> {
        // Find a free slot
        let slot = self.entries.iter().position(|e| !e.is_in_use())?;

        let entry = &mut self.entries[slot];
        entry.set_name(name);
        entry.start_block = start_block;
        entry.block_count = block_count;
        entry.byte_length = 0;
        entry.set_in_use(true);
        self.dirty = true;

        Some(slot)
    }

    /// Delete a file entry by index.
    pub fn delete(&mut self, index: usize) -> Option<FileEntry> {
        if index >= MAX_ENTRIES || !self.entries[index].is_in_use() {
            return None;
        }

        let entry = self.entries[index];
        self.entries[index] = FileEntry::empty();
        self.dirty = true;
        Some(entry)
    }

    /// Get a reference to an entry by index.
    pub fn get(&self, index: usize) -> Option<&FileEntry> {
        if index < MAX_ENTRIES && self.entries[index].is_in_use() {
            Some(&self.entries[index])
        } else {
            None
        }
    }

    /// Get a mutable reference to an entry by index.
    pub fn get_mut(&mut self, index: usize) -> Option<&mut FileEntry> {
        if index < MAX_ENTRIES && self.entries[index].is_in_use() {
            self.dirty = true;
            Some(&mut self.entries[index])
        } else {
            None
        }
    }

    /// Mark the table as dirty (e.g., after modifying an entry's byte_length).
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }
}
