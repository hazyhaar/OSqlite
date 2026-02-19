/// BlockDevice trait — abstracts block I/O for testing.
///
/// Both the real NVMe driver and a RAM-backed mock implement this trait.
/// BlockAllocator and FileTable use this instead of NvmeDriver directly.
use crate::drivers::nvme::NvmeError;
use crate::mem::DmaBuf;

/// Abstract block device for storage operations.
pub trait BlockDevice {
    /// Read `block_count` blocks starting at `lba` into `buf`.
    fn read_blocks(&mut self, lba: u64, block_count: u16, buf: &mut DmaBuf) -> Result<(), NvmeError>;

    /// Write `block_count` blocks starting at `lba` from `buf`.
    fn write_blocks(&mut self, lba: u64, block_count: u16, buf: &DmaBuf) -> Result<(), NvmeError>;

    /// Flush all writes to stable storage.
    fn flush(&mut self) -> Result<(), NvmeError>;

    /// Block size in bytes.
    fn block_size(&self) -> u32;

    /// Total number of blocks on device.
    fn total_blocks(&self) -> u64;
}
