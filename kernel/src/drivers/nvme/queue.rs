/// NVMe submission and completion queue structures.
///
/// Per NVMe spec 1.4:
/// - Submission Queue Entry (SQE): 64 bytes
/// - Completion Queue Entry (CQE): 16 bytes
/// - Queues are circular buffers in physically contiguous DMA memory.
use crate::mem::{PhysAddr, DmaBuf, AllocError};

/// NVMe Submission Queue Entry — 64 bytes, per spec.
#[repr(C, align(64))]
#[derive(Clone, Copy)]
pub struct SubmissionEntry {
    /// Command Dword 0: Opcode[7:0], FUSE[9:8], PSDT[15:14], CID[31:16]
    pub cdw0: u32,
    /// Namespace Identifier
    pub nsid: u32,
    /// Reserved
    pub cdw2: u32,
    pub cdw3: u32,
    /// Metadata Pointer
    pub mptr: u64,
    /// PRP Entry 1 (or SGL)
    pub prp1: u64,
    /// PRP Entry 2 (or SGL) or PRP List Pointer
    pub prp2: u64,
    /// Command-specific Dwords 10-15
    pub cdw10: u32,
    pub cdw11: u32,
    pub cdw12: u32,
    pub cdw13: u32,
    pub cdw14: u32,
    pub cdw15: u32,
}

static_assertions::const_assert_eq!(core::mem::size_of::<SubmissionEntry>(), 64);

impl SubmissionEntry {
    pub const fn zeroed() -> Self {
        Self {
            cdw0: 0, nsid: 0, cdw2: 0, cdw3: 0, mptr: 0,
            prp1: 0, prp2: 0, cdw10: 0, cdw11: 0, cdw12: 0,
            cdw13: 0, cdw14: 0, cdw15: 0,
        }
    }

    /// Build an Identify command (admin opcode 0x06).
    /// `cns`: 0 = identify namespace, 1 = identify controller
    pub fn identify(nsid: u32, cns: u32, data_phys: PhysAddr) -> Self {
        Self {
            cdw0: 0x06, // Identify opcode
            nsid,
            prp1: data_phys.as_u64(),
            cdw10: cns,
            ..Self::zeroed()
        }
    }

    /// Create I/O Completion Queue (admin opcode 0x05).
    pub fn create_io_cq(qid: u16, size: u16, cq_phys: PhysAddr) -> Self {
        Self {
            cdw0: 0x05,
            prp1: cq_phys.as_u64(),
            // CDW10: QSIZE[31:16] | QID[15:0]
            cdw10: ((size as u32 - 1) << 16) | qid as u32,
            // CDW11: IEN=0, PC=1 (physically contiguous), IV=0
            cdw11: 0x01,
            ..Self::zeroed()
        }
    }

    /// Create I/O Submission Queue (admin opcode 0x01).
    pub fn create_io_sq(qid: u16, size: u16, sq_phys: PhysAddr, cqid: u16) -> Self {
        Self {
            cdw0: 0x01,
            prp1: sq_phys.as_u64(),
            cdw10: ((size as u32 - 1) << 16) | qid as u32,
            // CDW11: CQID[31:16] | QPRIO=0 | PC=1
            cdw11: ((cqid as u32) << 16) | 0x01,
            ..Self::zeroed()
        }
    }

    /// NVM Read command (I/O opcode 0x02).
    pub fn read(nsid: u32, lba: u64, nlb: u16, prp1: PhysAddr, prp2: PhysAddr) -> Self {
        Self {
            cdw0: 0x02,
            nsid,
            prp1: prp1.as_u64(),
            prp2: prp2.as_u64(),
            cdw10: lba as u32,           // Starting LBA (low 32 bits)
            cdw11: (lba >> 32) as u32,   // Starting LBA (high 32 bits)
            cdw12: nlb as u32,           // Number of Logical Blocks (0-indexed)
            ..Self::zeroed()
        }
    }

    /// NVM Write command (I/O opcode 0x01).
    pub fn write(nsid: u32, lba: u64, nlb: u16, prp1: PhysAddr, prp2: PhysAddr) -> Self {
        Self {
            cdw0: 0x01,
            nsid,
            prp1: prp1.as_u64(),
            prp2: prp2.as_u64(),
            cdw10: lba as u32,
            cdw11: (lba >> 32) as u32,
            cdw12: nlb as u32,
            ..Self::zeroed()
        }
    }

    /// NVM Flush command (I/O opcode 0x00).
    pub fn flush(nsid: u32) -> Self {
        Self {
            cdw0: 0x00,
            nsid,
            ..Self::zeroed()
        }
    }
}

/// NVMe Completion Queue Entry — 16 bytes.
#[repr(C, align(16))]
#[derive(Clone, Copy)]
pub struct CompletionEntry {
    /// Command-specific result
    pub dw0: u32,
    /// Reserved
    pub dw1: u32,
    /// SQ Head Pointer[15:0] | SQ Identifier[31:16]
    pub sq_head_sqid: u32,
    /// Command Identifier[15:0] | Phase Tag[0] | Status[15:1]
    pub cid_status: u32,
}

static_assertions::const_assert_eq!(core::mem::size_of::<CompletionEntry>(), 16);

impl CompletionEntry {
    pub const fn zeroed() -> Self {
        Self { dw0: 0, dw1: 0, sq_head_sqid: 0, cid_status: 0 }
    }

    /// Extract the phase bit from this completion entry.
    pub fn phase(&self) -> bool {
        self.cid_status & 1 != 0
    }

    /// Extract the status code (14 bits).
    pub fn status(&self) -> u16 {
        ((self.cid_status >> 1) & 0x7FFF) as u16
    }

    /// Extract the command identifier.
    pub fn command_id(&self) -> u16 {
        (self.cid_status >> 16) as u16
    }
}

/// A queue pair: one submission queue + one completion queue.
pub struct QueuePair {
    id: u16,
    sq_buf: DmaBuf,
    cq_buf: DmaBuf,
    sq_tail: u16,
    cq_head: u16,
    size: u16,
    cq_phase: bool,
    next_cid: u16,
}

impl QueuePair {
    /// Allocate a new queue pair with the given queue ID and size.
    pub fn new(id: u16, size: u16) -> Result<Self, AllocError> {
        let sq_bytes = size as usize * core::mem::size_of::<SubmissionEntry>();
        let cq_bytes = size as usize * core::mem::size_of::<CompletionEntry>();

        let sq_buf = DmaBuf::alloc_aligned(sq_bytes, 1)?;
        let cq_buf = DmaBuf::alloc_aligned(cq_bytes, 1)?;

        Ok(Self {
            id,
            sq_buf,
            cq_buf,
            sq_tail: 0,
            cq_head: 0,
            size,
            cq_phase: true,
            next_cid: 0,
        })
    }

    pub fn id(&self) -> u16 {
        self.id
    }

    pub fn sq_phys(&self) -> PhysAddr {
        self.sq_buf.phys_addr()
    }

    pub fn cq_phys(&self) -> PhysAddr {
        self.cq_buf.phys_addr()
    }

    pub fn sq_tail(&self) -> u16 {
        self.sq_tail
    }

    pub fn cq_head(&self) -> u16 {
        self.cq_head
    }

    /// Place a submission entry in the SQ. Caller must ring the doorbell after.
    pub fn submit(&mut self, mut entry: SubmissionEntry) {
        // Set command ID
        let cid = self.next_cid;
        self.next_cid = self.next_cid.wrapping_add(1);
        entry.cdw0 = (entry.cdw0 & 0xFFFF) | ((cid as u32) << 16);

        let offset = self.sq_tail as usize * core::mem::size_of::<SubmissionEntry>();
        unsafe {
            let dst = self.sq_buf.as_mut_ptr().add(offset) as *mut SubmissionEntry;
            core::ptr::write_volatile(dst, entry);
        }

        self.sq_tail = (self.sq_tail + 1) % self.size;
    }

    /// Poll the CQ for a completion. Returns the status code if a new
    /// completion is available, or None if the CQ is empty.
    pub fn poll_completion(&mut self) -> Option<u16> {
        let offset = self.cq_head as usize * core::mem::size_of::<CompletionEntry>();
        let cqe = unsafe {
            let src = self.cq_buf.as_ptr().add(offset) as *const CompletionEntry;
            core::ptr::read_volatile(src)
        };

        if cqe.phase() == self.cq_phase {
            // Valid completion
            self.cq_head = (self.cq_head + 1) % self.size;
            if self.cq_head == 0 {
                self.cq_phase = !self.cq_phase;
            }
            Some(cqe.status())
        } else {
            None
        }
    }
}

/// Admin queue — same structure, different type for clarity.
pub struct AdminQueue {
    inner: Option<QueuePair>,
}

impl AdminQueue {
    pub const fn uninit() -> Self {
        Self { inner: None }
    }

    pub fn new(size: u16) -> Result<Self, AllocError> {
        Ok(Self {
            inner: Some(QueuePair::new(0, size)?),
        })
    }

    pub fn sq_phys(&self) -> PhysAddr {
        self.inner.as_ref().unwrap().sq_phys()
    }

    pub fn cq_phys(&self) -> PhysAddr {
        self.inner.as_ref().unwrap().cq_phys()
    }

    pub fn sq_tail(&self) -> u16 {
        self.inner.as_ref().unwrap().sq_tail()
    }

    pub fn cq_head(&self) -> u16 {
        self.inner.as_ref().unwrap().cq_head()
    }

    pub fn submit(&mut self, entry: SubmissionEntry) {
        self.inner.as_mut().unwrap().submit(entry);
    }

    pub fn poll_completion(&mut self) -> Option<u16> {
        self.inner.as_mut().unwrap().poll_completion()
    }
}
