#[allow(dead_code)]
mod queue;
#[allow(dead_code)]
mod command;
pub mod pci;

pub use command::{NvmeCommand, AdminOpcode, NvmOpcode, NvmeError};
pub use queue::{SubmissionEntry, CompletionEntry};

use core::sync::atomic::{compiler_fence, Ordering};
use spin::Mutex;
use crate::mem::DmaBuf;
use queue::{QueuePair, AdminQueue};

/// NVMe controller BAR0 register offsets.
mod regs {
    pub const CAP: usize = 0x00;       // Controller Capabilities
    pub const VS: usize = 0x08;        // Version
    pub const CC: usize = 0x14;        // Controller Configuration
    pub const CSTS: usize = 0x1C;      // Controller Status
    pub const AQA: usize = 0x24;       // Admin Queue Attributes
    pub const ASQ: usize = 0x28;       // Admin Submission Queue Base Address
    pub const ACQ: usize = 0x30;       // Admin Completion Queue Base Address
    pub const SQ0TDBL: usize = 0x1000; // Submission Queue 0 Tail Doorbell
}

/// Namespace identification data (from Identify Namespace command).
#[derive(Debug, Clone)]
pub struct NamespaceInfo {
    pub nsid: u32,
    pub block_count: u64,
    pub block_size: u32,          // Typically 512 or 4096
    pub metadata_size: u32,
}

/// Main NVMe driver state.
pub struct NvmeDriver {
    bar0: *mut u8,                 // MMIO base address
    doorbell_stride: usize,        // From CAP.DSTRD
    admin_queue: AdminQueue,
    io_queue: Option<QueuePair>,
    ns_info: Option<NamespaceInfo>,
}

unsafe impl Send for NvmeDriver {}

impl NvmeDriver {
    /// Create a new NVMe driver from a discovered BAR0 address.
    ///
    /// # Safety
    /// `bar0` must point to the NVMe controller's BAR0 MMIO region,
    /// mapped as uncacheable (UC) in the page tables.
    pub unsafe fn new(bar0: *mut u8) -> Result<Self, NvmeError> {
        let mut driver = Self {
            bar0,
            doorbell_stride: 4, // default, updated from CAP
            admin_queue: AdminQueue::uninit(),
            io_queue: None,
            ns_info: None,
        };

        driver.init_controller()?;
        Ok(driver)
    }

    /// Full controller initialization sequence per NVMe spec 1.4.
    unsafe fn init_controller(&mut self) -> Result<(), NvmeError> {
        // 1. Read capabilities
        let cap = self.read_reg64(regs::CAP);
        self.doorbell_stride = 4 << ((cap >> 32) & 0xF) as usize; // CAP.DSTRD
        let max_queue_entries = ((cap & 0xFFFF) + 1) as u16;
        let timeout_500ms = ((cap >> 24) & 0xFF) as u32; // in 500ms units

        // 2. Disable controller (CC.EN = 0)
        self.write_reg32(regs::CC, 0);
        self.wait_for_ready(false, timeout_500ms)?;

        // 3. Configure Admin Queue
        let aq_size: u16 = 32; // entries
        self.admin_queue = AdminQueue::new(aq_size).map_err(|_| NvmeError::OutOfMemory)?;

        // Set Admin Queue Attributes
        let aqa = ((aq_size as u32 - 1) << 16) | (aq_size as u32 - 1);
        self.write_reg32(regs::AQA, aqa);

        // Set Admin Queue base addresses
        self.write_reg64(regs::ASQ, self.admin_queue.sq_phys().as_u64());
        self.write_reg64(regs::ACQ, self.admin_queue.cq_phys().as_u64());

        // 4. Enable controller
        // CC: IOCQES=4 (16B), IOSQES=6 (64B), MPS=0 (4K pages), CSS=0 (NVM), EN=1
        let cc = (4 << 20) | (6 << 16) | (0 << 7) | (0 << 4) | 1;
        self.write_reg32(regs::CC, cc);
        self.wait_for_ready(true, timeout_500ms)?;

        // 5. Identify Controller (admin command)
        self.identify_controller()?;

        // 6. Create I/O Completion Queue
        // 7. Create I/O Submission Queue
        self.create_io_queues(64.min(max_queue_entries))?;

        // 8. Identify Namespace 1
        self.identify_namespace(1)?;

        Ok(())
    }

    /// Wait for CSTS.RDY to reach the desired state.
    unsafe fn wait_for_ready(&self, ready: bool, timeout_500ms: u32) -> Result<(), NvmeError> {
        let target = if ready { 1 } else { 0 };
        // Simple spin wait. A real implementation would use a timer.
        let max_spins = (timeout_500ms as u64) * 500_000; // rough approximation
        for _ in 0..max_spins {
            let csts = self.read_reg32(regs::CSTS);
            if (csts & 1) == target {
                return Ok(());
            }
            if csts & 0x2 != 0 {
                // CFS (Controller Fatal Status)
                return Err(NvmeError::ControllerFatal);
            }
            core::hint::spin_loop();
        }
        Err(NvmeError::Timeout)
    }

    /// Send Identify Controller command via admin queue.
    unsafe fn identify_controller(&mut self) -> Result<(), NvmeError> {
        let mut buf = DmaBuf::alloc(4096).map_err(|_| NvmeError::OutOfMemory)?;
        let cmd = SubmissionEntry::identify(0, 1, buf.phys_addr()); // CNS=1: identify controller
        let status = self.admin_submit_wait(cmd, &mut buf)?;
        if status != 0 {
            return Err(NvmeError::CommandFailed(status));
        }
        // Parse identify data if needed (model name, firmware, etc.)
        Ok(())
    }

    /// Create I/O queue pair (CQ first, then SQ per spec).
    unsafe fn create_io_queues(&mut self, size: u16) -> Result<(), NvmeError> {
        let qp = QueuePair::new(1, size).map_err(|_| NvmeError::OutOfMemory)?;

        // Create I/O Completion Queue (admin opcode 0x05)
        let cmd = SubmissionEntry::create_io_cq(1, size, qp.cq_phys());
        let status = self.admin_submit_wait_no_buf(cmd)?;
        if status != 0 {
            return Err(NvmeError::CommandFailed(status));
        }

        // Create I/O Submission Queue (admin opcode 0x01)
        let cmd = SubmissionEntry::create_io_sq(1, size, qp.sq_phys(), 1 /* cqid */);
        let status = self.admin_submit_wait_no_buf(cmd)?;
        if status != 0 {
            return Err(NvmeError::CommandFailed(status));
        }

        self.io_queue = Some(qp);
        Ok(())
    }

    /// Identify Namespace — get block count, block size.
    unsafe fn identify_namespace(&mut self, nsid: u32) -> Result<(), NvmeError> {
        let mut buf = DmaBuf::alloc(4096).map_err(|_| NvmeError::OutOfMemory)?;
        let cmd = SubmissionEntry::identify(nsid, 0, buf.phys_addr()); // CNS=0: identify namespace
        let status = self.admin_submit_wait(cmd, &mut buf)?;
        if status != 0 {
            return Err(NvmeError::CommandFailed(status));
        }

        buf.invalidate_cache();
        let data = buf.as_slice();

        // NSZE: Namespace Size (bytes 0-7) — number of logical blocks
        let block_count = u64::from_le_bytes(data[0..8].try_into().unwrap());

        // FLBAS: Formatted LBA Size (byte 26, bits 3:0 = index into LBA Format table)
        let flbas_index = (data[26] & 0x0F) as usize;

        // LBA Format table starts at byte 128, each entry is 4 bytes
        // Bits 23:16 = LBADS (LBA Data Size as power of 2)
        let lbaf_offset = 128 + flbas_index * 4;
        let lbaf = u32::from_le_bytes(data[lbaf_offset..lbaf_offset + 4].try_into().unwrap());
        let lbads = (lbaf >> 16) & 0xFF;
        let block_size = 1u32 << lbads;
        let metadata_size = (lbaf & 0xFFFF) as u32;

        self.ns_info = Some(NamespaceInfo {
            nsid,
            block_count,
            block_size,
            metadata_size,
        });

        Ok(())
    }

    /// Submit a command on the admin queue and wait for completion.
    unsafe fn admin_submit_wait(
        &mut self,
        cmd: SubmissionEntry,
        _buf: &mut DmaBuf,
    ) -> Result<u16, NvmeError> {
        self.admin_queue.submit(cmd);
        compiler_fence(Ordering::SeqCst);
        self.ring_admin_sq_doorbell();

        // Spin-wait for completion
        loop {
            if let Some(status) = self.admin_queue.poll_completion() {
                self.ring_admin_cq_doorbell();
                return Ok(status);
            }
            core::hint::spin_loop();
        }
    }

    unsafe fn admin_submit_wait_no_buf(
        &mut self,
        cmd: SubmissionEntry,
    ) -> Result<u16, NvmeError> {
        self.admin_queue.submit(cmd);
        compiler_fence(Ordering::SeqCst);
        self.ring_admin_sq_doorbell();

        loop {
            if let Some(status) = self.admin_queue.poll_completion() {
                self.ring_admin_cq_doorbell();
                return Ok(status);
            }
            core::hint::spin_loop();
        }
    }

    // ---- Doorbell helper (no &mut self borrow) ----

    /// Write a doorbell register. Uses bar0 directly to avoid borrow conflicts.
    unsafe fn write_doorbell(bar0: *mut u8, offset: usize, val: u32) {
        core::ptr::write_volatile(bar0.add(offset) as *mut u32, val);
    }

    // ---- Public I/O interface ----

    /// Read `block_count` blocks starting at `lba` into `buf`.
    pub fn read_blocks(
        &mut self,
        lba: u64,
        block_count: u16,
        buf: &mut DmaBuf,
    ) -> Result<(), NvmeError> {
        let ns = self.ns_info.as_ref().ok_or(NvmeError::NotInitialized)?;
        let nsid = ns.nsid;
        let bs = ns.block_size;
        let bar0 = self.bar0;
        let stride = self.doorbell_stride;
        let qp = self.io_queue.as_mut().ok_or(NvmeError::NotInitialized)?;
        let qid = qp.id() as usize;

        let (prp1, prp2, _prp_list) = command::build_prp(buf, block_count as usize * bs as usize);

        let cmd = SubmissionEntry::read(nsid, lba, block_count - 1, prp1, prp2);
        qp.submit(cmd);
        compiler_fence(Ordering::SeqCst);
        let sq_tail = qp.sq_tail();
        unsafe { Self::write_doorbell(bar0, regs::SQ0TDBL + (2 * qid) * stride, sq_tail as u32) };

        loop {
            if let Some(status) = qp.poll_completion() {
                let cq_head = qp.cq_head();
                unsafe { Self::write_doorbell(bar0, regs::SQ0TDBL + (2 * qid + 1) * stride, cq_head as u32) };
                // _prp_list dropped here after command completes
                if status != 0 {
                    return Err(NvmeError::CommandFailed(status));
                }
                buf.invalidate_cache();
                return Ok(());
            }
            core::hint::spin_loop();
        }
    }

    /// Write `block_count` blocks starting at `lba` from `buf`.
    pub fn write_blocks(
        &mut self,
        lba: u64,
        block_count: u16,
        buf: &DmaBuf,
    ) -> Result<(), NvmeError> {
        let ns = self.ns_info.as_ref().ok_or(NvmeError::NotInitialized)?;
        let nsid = ns.nsid;
        let bs = ns.block_size;
        let bar0 = self.bar0;
        let stride = self.doorbell_stride;
        let qp = self.io_queue.as_mut().ok_or(NvmeError::NotInitialized)?;
        let qid = qp.id() as usize;

        buf.flush_cache();
        let (prp1, prp2, _prp_list) = command::build_prp(buf, block_count as usize * bs as usize);

        let cmd = SubmissionEntry::write(nsid, lba, block_count - 1, prp1, prp2);
        qp.submit(cmd);
        compiler_fence(Ordering::SeqCst);
        let sq_tail = qp.sq_tail();
        unsafe { Self::write_doorbell(bar0, regs::SQ0TDBL + (2 * qid) * stride, sq_tail as u32) };

        loop {
            if let Some(status) = qp.poll_completion() {
                let cq_head = qp.cq_head();
                unsafe { Self::write_doorbell(bar0, regs::SQ0TDBL + (2 * qid + 1) * stride, cq_head as u32) };
                // _prp_list dropped here after command completes
                if status != 0 {
                    return Err(NvmeError::CommandFailed(status));
                }
                return Ok(());
            }
            core::hint::spin_loop();
        }
    }

    /// Flush — force all written data to non-volatile storage.
    /// This is the ACID guarantee for SQLite.
    pub fn flush(&mut self) -> Result<(), NvmeError> {
        let ns = self.ns_info.as_ref().ok_or(NvmeError::NotInitialized)?;
        let nsid = ns.nsid;
        let bar0 = self.bar0;
        let stride = self.doorbell_stride;
        let qp = self.io_queue.as_mut().ok_or(NvmeError::NotInitialized)?;
        let qid = qp.id() as usize;

        let cmd = SubmissionEntry::flush(nsid);
        qp.submit(cmd);
        compiler_fence(Ordering::SeqCst);
        let sq_tail = qp.sq_tail();
        unsafe { Self::write_doorbell(bar0, regs::SQ0TDBL + (2 * qid) * stride, sq_tail as u32) };

        loop {
            if let Some(status) = qp.poll_completion() {
                let cq_head = qp.cq_head();
                unsafe { Self::write_doorbell(bar0, regs::SQ0TDBL + (2 * qid + 1) * stride, cq_head as u32) };
                if status != 0 {
                    return Err(NvmeError::CommandFailed(status));
                }
                return Ok(());
            }
            core::hint::spin_loop();
        }
    }

    /// Get namespace info (block size, capacity, etc.).
    pub fn namespace_info(&self) -> Option<&NamespaceInfo> {
        self.ns_info.as_ref()
    }

    // ---- MMIO helpers ----

    unsafe fn read_reg32(&self, offset: usize) -> u32 {
        core::ptr::read_volatile(self.bar0.add(offset) as *const u32)
    }

    unsafe fn read_reg64(&self, offset: usize) -> u64 {
        core::ptr::read_volatile(self.bar0.add(offset) as *const u64)
    }

    unsafe fn write_reg32(&self, offset: usize, val: u32) {
        core::ptr::write_volatile(self.bar0.add(offset) as *mut u32, val);
    }

    unsafe fn write_reg64(&self, offset: usize, val: u64) {
        core::ptr::write_volatile(self.bar0.add(offset) as *mut u64, val);
    }

    unsafe fn ring_admin_sq_doorbell(&mut self) {
        let tail = self.admin_queue.sq_tail();
        self.write_reg32(regs::SQ0TDBL, tail as u32);
    }

    unsafe fn ring_admin_cq_doorbell(&mut self) {
        let head = self.admin_queue.cq_head();
        self.write_reg32(regs::SQ0TDBL + self.doorbell_stride, head as u32);
    }

    unsafe fn ring_io_sq_doorbell(&mut self) {
        let qp = self.io_queue.as_ref().unwrap();
        let qid = qp.id() as usize;
        let offset = regs::SQ0TDBL + (2 * qid) * self.doorbell_stride;
        self.write_reg32(offset, qp.sq_tail() as u32);
    }

    unsafe fn ring_io_cq_doorbell(&mut self) {
        let qp = self.io_queue.as_ref().unwrap();
        let qid = qp.id() as usize;
        let offset = regs::SQ0TDBL + (2 * qid + 1) * self.doorbell_stride;
        self.write_reg32(offset, qp.cq_head() as u32);
    }
}

/// Global NVMe driver instance (initialized during boot).
pub static NVME: Mutex<Option<NvmeDriver>> = Mutex::new(None);
