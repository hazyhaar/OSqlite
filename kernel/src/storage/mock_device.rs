/// RAM-backed mock block device for testing.
///
/// Simulates a block device entirely in memory. Used with the `test-mock-nvme`
/// feature flag for unit testing BlockAllocator and FileTable without hardware.
use alloc::vec;
use alloc::vec::Vec;

use crate::drivers::nvme::NvmeError;
use crate::mem::DmaBuf;
use super::block_device::BlockDevice;

/// RAM-backed block device.
pub struct RamDisk {
    data: Vec<u8>,
    block_size: u32,
    total_blocks: u64,
    flush_count: u64,
}

impl RamDisk {
    /// Create a RAM disk with the given geometry.
    pub fn new(total_blocks: u64, block_size: u32) -> Self {
        let total_bytes = total_blocks as usize * block_size as usize;
        Self {
            data: vec![0u8; total_bytes],
            block_size,
            total_blocks,
            flush_count: 0,
        }
    }

    /// How many times flush() was called (for testing).
    pub fn flush_count(&self) -> u64 {
        self.flush_count
    }

    /// Read raw bytes at an offset (for test verification).
    pub fn read_raw(&self, offset: usize, len: usize) -> &[u8] {
        &self.data[offset..offset + len]
    }
}

impl BlockDevice for RamDisk {
    fn read_blocks(&mut self, lba: u64, block_count: u16, buf: &mut DmaBuf) -> Result<(), NvmeError> {
        let bs = self.block_size as usize;
        let start = lba as usize * bs;
        let len = block_count as usize * bs;

        if start + len > self.data.len() {
            return Err(NvmeError::MediaError);
        }

        let dst = buf.as_mut_slice();
        let copy_len = len.min(dst.len());
        dst[..copy_len].copy_from_slice(&self.data[start..start + copy_len]);

        Ok(())
    }

    fn write_blocks(&mut self, lba: u64, block_count: u16, buf: &DmaBuf) -> Result<(), NvmeError> {
        let bs = self.block_size as usize;
        let start = lba as usize * bs;
        let len = block_count as usize * bs;

        if start + len > self.data.len() {
            return Err(NvmeError::MediaError);
        }

        let src = buf.as_slice();
        let copy_len = len.min(src.len());
        self.data[start..start + copy_len].copy_from_slice(&src[..copy_len]);

        Ok(())
    }

    fn flush(&mut self) -> Result<(), NvmeError> {
        self.flush_count += 1;
        Ok(())
    }

    fn block_size(&self) -> u32 {
        self.block_size
    }

    fn total_blocks(&self) -> u64 {
        self.total_blocks
    }
}
