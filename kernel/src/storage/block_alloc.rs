/// On-disk block allocator.
///
/// Layout on the NVMe namespace:
///   LBA 0:         Superblock (magic, version, geometry)
///   LBA 1..M:      Bitmap (1 bit per data block: 0=free, 1=used)
///   LBA M+1..M+K:  File table (fixed-size entries)
///   LBA M+K+1..:   Data blocks
///
/// The bitmap and file table are cached in RAM and flushed to disk on sync.
use alloc::vec;
use alloc::vec::Vec;

use crate::drivers::nvme::{NvmeDriver, NvmeError};
use crate::mem::DmaBuf;

/// Superblock magic: "HVNOS\x01\x00\x00" in little-endian.
const SUPERBLOCK_MAGIC: u64 = 0x0000_01_534F4E5648; // "HVNOS\x01"

/// Superblock version.
const SUPERBLOCK_VERSION: u32 = 1;

/// On-disk superblock at LBA 0.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Superblock {
    pub magic: u64,
    pub version: u32,
    pub block_size: u32,          // bytes per block (from NVMe, typically 4096)
    pub total_blocks: u64,        // total blocks on namespace
    pub bitmap_start_lba: u64,    // first LBA of bitmap region
    pub bitmap_block_count: u64,  // blocks occupied by bitmap
    pub file_table_start_lba: u64,
    pub file_table_block_count: u64,
    pub data_start_lba: u64,      // first usable data LBA
    pub data_block_count: u64,    // number of data blocks
    _padding: [u8; 4008],         // pad to 4096 bytes
}

static_assertions::const_assert!(core::mem::size_of::<Superblock>() <= 4096);

impl Superblock {
    /// Check if this superblock has valid magic.
    pub fn is_valid(&self) -> bool {
        self.magic == SUPERBLOCK_MAGIC && self.version == SUPERBLOCK_VERSION
    }
}

/// In-memory block allocator, backed by the on-disk bitmap.
pub struct BlockAllocator {
    bitmap: Vec<u64>,             // In-memory bitmap (1 bit per data block)
    data_block_count: u64,        // Total data blocks available
    data_start_lba: u64,          // LBA offset where data blocks begin
    bitmap_start_lba: u64,
    bitmap_on_disk_blocks: u64,
    block_size: u32,
    free_count: u64,
    dirty: bool,
}

impl BlockAllocator {
    /// Create an uninitialized allocator. Call `load` or `format` before use.
    pub fn new() -> Self {
        Self {
            bitmap: Vec::new(),
            data_block_count: 0,
            data_start_lba: 0,
            bitmap_start_lba: 0,
            bitmap_on_disk_blocks: 0,
            block_size: 4096,
            free_count: 0,
            dirty: false,
        }
    }

    /// Format a blank NVMe namespace — write superblock, zeroed bitmap,
    /// zeroed file table.
    pub fn format(
        nvme: &mut NvmeDriver,
        total_blocks: u64,
        block_size: u32,
    ) -> Result<Self, NvmeError> {
        // Calculate layout
        let data_bits_per_block = (block_size as u64) * 8;

        // Bitmap blocks needed = ceil(data_blocks / bits_per_block)
        // But data_blocks depends on bitmap size... iterate to fixed point.
        let overhead = 1u64; // superblock
        let file_table_blocks = 1u64; // one block for file table (fits ~50 entries)

        // First approximation: all blocks are data
        let approx_data = total_blocks - overhead - file_table_blocks;
        let bitmap_blocks = (approx_data + data_bits_per_block - 1) / data_bits_per_block;

        let data_start = overhead + bitmap_blocks + file_table_blocks;
        let data_blocks = total_blocks.saturating_sub(data_start);

        // Write superblock
        let sb = Superblock {
            magic: SUPERBLOCK_MAGIC,
            version: SUPERBLOCK_VERSION,
            block_size,
            total_blocks,
            bitmap_start_lba: 1,
            bitmap_block_count: bitmap_blocks,
            file_table_start_lba: 1 + bitmap_blocks,
            file_table_block_count: file_table_blocks,
            data_start_lba: data_start,
            data_block_count: data_blocks,
            _padding: [0u8; 4008],
        };

        let mut buf = DmaBuf::alloc(block_size as usize)
            .map_err(|_| NvmeError::OutOfMemory)?;
        let sb_bytes = unsafe {
            core::slice::from_raw_parts(
                &sb as *const Superblock as *const u8,
                core::mem::size_of::<Superblock>(),
            )
        };
        buf.copy_from_slice(sb_bytes);
        nvme.write_blocks(0, 1, &buf)?;

        // Write zeroed bitmap (all free)
        let zero_buf = DmaBuf::alloc(block_size as usize)
            .map_err(|_| NvmeError::OutOfMemory)?;
        for i in 0..bitmap_blocks {
            nvme.write_blocks(1 + i, 1, &zero_buf)?;
        }

        // Write zeroed file table
        nvme.write_blocks(1 + bitmap_blocks, 1, &zero_buf)?;

        // Flush to make everything durable
        nvme.flush()?;

        // Build in-memory state
        let bitmap_words = ((data_blocks + 63) / 64) as usize;
        let allocator = Self {
            bitmap: vec![0u64; bitmap_words], // all free
            data_block_count: data_blocks,
            data_start_lba: data_start,
            bitmap_start_lba: 1,
            bitmap_on_disk_blocks: bitmap_blocks,
            block_size,
            free_count: data_blocks,
            dirty: false,
        };

        Ok(allocator)
    }

    /// Load an existing allocator from a formatted NVMe namespace.
    pub fn load(nvme: &mut NvmeDriver) -> Result<Self, NvmeError> {
        // Read superblock
        let block_size = nvme.namespace_info()
            .ok_or(NvmeError::NotInitialized)?
            .block_size;

        let mut buf = DmaBuf::alloc(block_size as usize)
            .map_err(|_| NvmeError::OutOfMemory)?;
        nvme.read_blocks(0, 1, &mut buf)?;

        let sb = unsafe { &*(buf.as_ptr() as *const Superblock) };
        if !sb.is_valid() {
            return Err(NvmeError::MediaError); // Not formatted
        }

        // Read bitmap from disk into memory
        let bitmap_words = ((sb.data_block_count + 63) / 64) as usize;
        let mut bitmap = vec![0u64; bitmap_words];

        let mut bitmap_buf = DmaBuf::alloc(block_size as usize)
            .map_err(|_| NvmeError::OutOfMemory)?;

        let words_per_block = block_size as usize / 8;
        for blk in 0..sb.bitmap_block_count {
            nvme.read_blocks(sb.bitmap_start_lba + blk, 1, &mut bitmap_buf)?;

            let src = bitmap_buf.as_slice();
            let word_offset = blk as usize * words_per_block;
            for w in 0..words_per_block {
                if word_offset + w < bitmap_words {
                    let off = w * 8;
                    bitmap[word_offset + w] = u64::from_le_bytes(
                        src[off..off + 8].try_into().unwrap()
                    );
                }
            }
        }

        // Count free blocks
        let free_count = bitmap.iter()
            .enumerate()
            .map(|(i, &word)| {
                let valid_bits = if i == bitmap_words - 1 {
                    let rem = sb.data_block_count % 64;
                    if rem == 0 { 64 } else { rem }
                } else {
                    64
                };
                valid_bits - word.count_ones() as u64
            })
            .sum();

        Ok(Self {
            bitmap,
            data_block_count: sb.data_block_count,
            data_start_lba: sb.data_start_lba,
            bitmap_start_lba: sb.bitmap_start_lba,
            bitmap_on_disk_blocks: sb.bitmap_block_count,
            block_size,
            free_count,
            dirty: false,
        })
    }

    /// Allocate `count` contiguous data blocks. Returns the starting data-block index.
    /// The caller converts to LBA via `data_start_lba + index`.
    pub fn alloc(&mut self, count: u64) -> Result<u64, AllocError> {
        if count == 0 {
            return Err(AllocError::InvalidSize);
        }
        if self.free_count < count {
            return Err(AllocError::OutOfSpace);
        }

        // Linear scan for a contiguous run of free bits
        let total = self.data_block_count;
        let mut start = 0u64;

        while start + count <= total {
            let mut found = true;
            for i in 0..count {
                let idx = start + i;
                let word = (idx / 64) as usize;
                let bit = (idx % 64) as u32;
                if self.bitmap[word] & (1u64 << bit) != 0 {
                    start = idx + 1;
                    found = false;
                    break;
                }
            }

            if found {
                // Mark as allocated
                for i in 0..count {
                    let idx = start + i;
                    let word = (idx / 64) as usize;
                    let bit = (idx % 64) as u32;
                    self.bitmap[word] |= 1u64 << bit;
                }
                self.free_count -= count;
                self.dirty = true;
                return Ok(start);
            }
        }

        Err(AllocError::OutOfSpace)
    }

    /// Free `count` blocks starting at data-block index `start`.
    pub fn free(&mut self, start: u64, count: u64) {
        for i in 0..count {
            let idx = start + i;
            let word = (idx / 64) as usize;
            let bit = (idx % 64) as u32;
            debug_assert!(
                self.bitmap[word] & (1u64 << bit) != 0,
                "double free of block {}",
                idx
            );
            self.bitmap[word] &= !(1u64 << bit);
        }
        self.free_count += count;
        self.dirty = true;
    }

    /// Convert a data-block index to an absolute LBA.
    pub fn to_lba(&self, data_block: u64) -> u64 {
        self.data_start_lba + data_block
    }

    /// Flush the bitmap to disk if dirty.
    pub fn flush(&mut self, nvme: &mut NvmeDriver) -> Result<(), NvmeError> {
        if !self.dirty {
            return Ok(());
        }

        let mut buf = DmaBuf::alloc(self.block_size as usize)
            .map_err(|_| NvmeError::OutOfMemory)?;

        let words_per_block = self.block_size as usize / 8;
        let bitmap_words = self.bitmap.len();

        for blk in 0..self.bitmap_on_disk_blocks {
            let word_offset = blk as usize * words_per_block;
            let slice = buf.as_mut_slice();

            // Zero the buffer first
            slice.fill(0);

            // Copy bitmap words into the buffer
            for w in 0..words_per_block {
                if word_offset + w < bitmap_words {
                    let bytes = self.bitmap[word_offset + w].to_le_bytes();
                    let off = w * 8;
                    slice[off..off + 8].copy_from_slice(&bytes);
                }
            }

            nvme.write_blocks(self.bitmap_start_lba + blk, 1, &buf)?;
        }

        self.dirty = false;
        Ok(())
    }

    pub fn free_count(&self) -> u64 {
        self.free_count
    }

    pub fn block_size(&self) -> u32 {
        self.block_size
    }

    pub fn data_start_lba(&self) -> u64 {
        self.data_start_lba
    }
}

/// Block allocation errors.
#[derive(Debug)]
pub enum AllocError {
    OutOfSpace,
    InvalidSize,
    Fragmented,
}

impl core::fmt::Display for AllocError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            AllocError::OutOfSpace => write!(f, "no free blocks"),
            AllocError::InvalidSize => write!(f, "invalid allocation size"),
            AllocError::Fragmented => write!(f, "cannot find contiguous run"),
        }
    }
}
