# HeavenOS — Inferno/SQLite Bare-Metal Architecture

## Design Document v1.0

**Scope**: This document specifies how SQLite operates directly on NVMe
hardware (no Linux, no libc) inside an Inferno/Plan 9-inspired Rust kernel,
and how the resulting system exposes AI workloads through a Styx (9P2000)
namespace while keeping the GPU data path out of the file abstraction.

---

## 1. Principles

1. **SQLite is the system database, not the system bus.** It stores metadata,
   configuration, agent state, logs, and structured queries. It does NOT
   carry GPU tensors, model weights, or bulk inference data.

2. **"Everything is a file" is the control plane.** Styx exposes system
   state as a namespace. Agents read/write small control messages through
   the namespace. Bulk data moves through shared memory and DMA, signaled
   (not transported) via Styx.

3. **No magic words.** Every claim about performance, isolation, or
   correctness must be backed by a concrete mechanism. If we say "zero
   copy," we show the physical address flow. If we say "crash safe," we
   show the xSync → NVMe Flush path.

4. **Implement the hard 80%.** The VFS shim, the NVMe flush path, the
   non-aligned I/O, the shared memory for WAL — these are the actual
   engineering work, not the architectural diagram.

---

## 2. Hardware Assumptions

| Component        | Assumption                                          |
| ---------------- | --------------------------------------------------- |
| CPU              | x86_64 with MMU, APIC timer, TSC                   |
| NVMe             | PCIe-attached, supports PRP lists, Flush command    |
| RAM              | ≥ 512 MB, identity-mapped or with known phys→virt   |
| GPU (optional)   | PCIe, BAR0-mapped, vendor-specific command protocol |
| Boot             | UEFI → custom bootloader → kernel                   |

---

## 3. Memory Model

HeavenOS uses a **hybrid memory model**:

- **Kernel space**: identity-mapped (virt == phys) for the first N GB.
  This simplifies DMA address translation — any kernel buffer's virtual
  address IS its physical address.
- **User space** (if/when added): separate page tables per process.
  For the initial single-address-space design, all code runs in ring 0.

### 3.1 Physical Page Allocator

A bitmap allocator tracks 4 KiB pages. It provides:

```rust
/// Allocate `count` physically contiguous pages, aligned to `align` pages.
/// Returns the physical base address.
fn alloc_pages_contiguous(count: usize, align: usize) -> Result<PhysAddr, AllocError>;

/// Free previously allocated pages.
fn free_pages(base: PhysAddr, count: usize);
```

**Why contiguous**: NVMe PRP lists require that each entry points to a
physical page. For transfers > 4 KiB, either the pages are contiguous
(single PRP entry) or we build a PRP list (scattered pages). Both paths
must be supported.

### 3.2 DMA-Safe Allocator

Wraps the physical page allocator. Guarantees:
- Pages are not in CPU cache (or cache is flushed before DMA read,
  invalidated after DMA write)
- Alignment is ≥ controller's minimum transfer unit
- Allocations are tracked for cleanup on error paths

```rust
pub struct DmaBuf {
    virt: *mut u8,
    phys: PhysAddr,
    len: usize,
}

impl DmaBuf {
    pub fn alloc(size: usize) -> Result<Self, AllocError> { /* ... */ }

    /// Flush CPU caches for this buffer (before device reads it).
    pub fn flush_cache(&self) { /* clflush / clflushopt loop */ }

    /// Invalidate CPU caches for this buffer (after device wrote it).
    pub fn invalidate_cache(&self) { /* clflush / clflushopt + mfence */ }
}

impl Drop for DmaBuf {
    fn drop(&mut self) { /* free pages back to physical allocator */ }
}
```

---

## 4. NVMe Driver

### 4.1 Queue Pair Architecture

```
┌─────────────────────────────────────┐
│              NVMe Driver            │
│                                     │
│  Admin Queue (1 SQ + 1 CQ)         │
│    - Identify Controller            │
│    - Create I/O Queues              │
│    - Firmware commands              │
│                                     │
│  I/O Queue Pair 0 (SQ + CQ)        │
│    - Read / Write / Flush           │
│    - Interrupt-driven completion    │
│                                     │
│  (Future: 1 QP per CPU core)       │
└─────────────────────────────────────┘
```

### 4.2 Command Submission

```rust
pub struct NvmeCommand {
    opcode: u8,           // 0x02 = Read, 0x01 = Write, 0x00 = Flush
    nsid: u32,            // Namespace ID (usually 1)
    lba: u64,             // Starting Logical Block Address
    block_count: u16,     // Number of 512B or 4KiB blocks (0-indexed)
    prp1: PhysAddr,       // First PRP entry
    prp2: PhysAddr,       // Second PRP entry or PRP list pointer
}

impl NvmeDriver {
    /// Submit a command and block until completion.
    /// Returns the NVMe status code.
    pub fn submit_and_wait(&mut self, cmd: NvmeCommand) -> Result<(), NvmeError>;

    /// Submit a command, return a token for polling/interrupt completion.
    pub fn submit_async(&mut self, cmd: NvmeCommand) -> SubmissionToken;

    /// Check/wait for completion of an async submission.
    pub fn poll_completion(&mut self, token: SubmissionToken) -> Option<Result<(), NvmeError>>;
}
```

### 4.3 PRP List Construction

For transfers spanning multiple non-contiguous pages:

```rust
/// Build a PRP list for a DMA transfer.
/// - If size <= 4096: prp1 = phys_addr, prp2 = 0
/// - If size <= 8192: prp1 = phys_addr, prp2 = phys_addr + 4096
/// - If size > 8192:  prp1 = phys_addr, prp2 = pointer to PRP list
///   The PRP list itself must be in a DMA-accessible page.
fn build_prp(buf: &DmaBuf) -> (PhysAddr, PhysAddr);
fn build_prp_scattered(pages: &[PhysAddr]) -> (PhysAddr, PhysAddr, DmaBuf /* prp list */);
```

### 4.4 Error Classification

NVMe completion status → SQLite error code mapping:

| NVMe Status               | SQLite Error           | Recovery                    |
| -------------------------- | ---------------------- | --------------------------- |
| Success                    | SQLITE_OK              | —                           |
| Invalid Opcode/Field       | SQLITE_MISUSE          | Bug in driver               |
| Data Transfer Error        | SQLITE_IOERR_READ/WRITE | Retry once, then fail      |
| Unrecoverable Media Error  | SQLITE_IOERR_CORRUPTFS | Mark sector bad, fail       |
| Namespace Not Ready        | SQLITE_BUSY            | Retry with backoff          |
| Write Fault                | SQLITE_FULL            | Disk likely failing         |
| Internal Device Error      | SQLITE_IOERR           | Controller reset path       |

---

## 5. SQLite VFS Implementation

### 5.1 Overview

SQLite's VFS is an interface with ~20 methods. We implement a custom VFS
named `"heavenos"` registered via `sqlite3_vfs_register()`.

The VFS manages a **block allocator** on the NVMe namespace, not a
traditional filesystem. SQLite sees "files" but the VFS translates them
to LBA ranges on raw disk.

### 5.2 Block Allocator

The first N blocks of the NVMe namespace contain a **superblock** and a
**bitmap**:

```
LBA 0:        Superblock (magic, version, block_size, total_blocks, bitmap_start, file_table_start)
LBA 1..M:     Bitmap (1 bit per block: 0=free, 1=allocated)
LBA M+1..M+K: File table (fixed entries: name[64], start_lba, length, flags)
LBA M+K+1..:  Data blocks
```

Block size = NVMe LBA size (typically 4096 bytes, queried from Identify Namespace).

```rust
struct BlockAllocator {
    bitmap: DmaBuf,          // In-memory copy of on-disk bitmap
    total_blocks: u64,
    first_data_block: u64,
    dirty: bool,             // Bitmap modified since last flush
}

impl BlockAllocator {
    /// Allocate `count` contiguous blocks. Returns starting LBA.
    fn alloc(&mut self, count: u64) -> Result<u64, AllocError>;

    /// Free `count` blocks starting at `lba`.
    fn free(&mut self, lba: u64, count: u64);

    /// Grow a file's allocation by `additional` blocks, possibly relocating.
    fn grow(&mut self, current_lba: u64, current_count: u64, additional: u64)
        -> Result<u64, AllocError>;

    /// Persist the bitmap to disk (called during xSync).
    fn flush(&mut self, nvme: &mut NvmeDriver) -> Result<(), NvmeError>;
}
```

**No fixed 90/10 partitioning.** The DB file, WAL file, journal, and temp
files all allocate from the same pool. They grow and shrink dynamically.

### 5.3 File Table

A small fixed-size table maps well-known names to LBA ranges:

| Slot | Name           | Purpose                          |
| ---- | -------------- | -------------------------------- |
| 0    | `main.db`      | Primary SQLite database          |
| 1    | `main.db-wal`  | WAL file                         |
| 2    | `main.db-shm`  | Shared memory (WAL index)        |
| 3    | `main.db-journal` | Rollback journal (non-WAL mode) |
| 4-7  | `temp_N`       | Temp files for sort/materialize  |

Each entry: `{ name: [u8; 64], start_lba: u64, block_count: u64, byte_length: u64, flags: u32 }`

### 5.4 VFS Methods — The Complete Set

#### xOpen

```rust
fn xopen(vfs: *mut sqlite3_vfs, name: *const c_char, file: *mut sqlite3_file,
         flags: c_int, out_flags: *mut c_int) -> c_int
{
    let filename = if name.is_null() {
        // Temp file — allocate next free temp slot
        allocate_temp_slot()
    } else {
        CStr::from_ptr(name)
    };

    let entry = file_table.lookup_or_create(filename, flags);
    if entry.is_err() { return SQLITE_CANTOPEN; }

    // Store file handle state
    (*file).start_lba = entry.start_lba;
    (*file).block_count = entry.block_count;
    (*file).byte_length = entry.byte_length;

    SQLITE_OK
}
```

#### xRead — Handling Non-Aligned Reads

This is the critical path. SQLite reads arbitrary byte ranges.

```rust
fn xread(file: *mut sqlite3_file, buf: *mut c_void,
         amount: c_int, offset: i64) -> c_int
{
    let amount = amount as usize;
    let offset = offset as u64;

    let block_size = BLOCK_SIZE as u64; // e.g., 4096
    let start_block = offset / block_size;
    let end_block = (offset + amount as u64 - 1) / block_size;
    let block_count = end_block - start_block + 1;

    let start_lba = file.start_lba + start_block;

    // Check bounds
    if start_block + block_count > file.block_count {
        // SQLite spec: short reads fill remainder with zeros
        // Read what we can, zero-fill the rest
    }

    // Allocate DMA buffer for full blocks
    let dma = DmaBuf::alloc((block_count as usize) * BLOCK_SIZE)?;

    // Issue NVMe read
    let cmd = NvmeCommand::read(start_lba, block_count as u16 - 1, &dma);
    nvme.submit_and_wait(cmd)?;

    // Invalidate CPU cache (device wrote to RAM via DMA)
    dma.invalidate_cache();

    // Copy the requested byte range from the DMA buffer to SQLite's buffer.
    // The offset within the first block:
    let byte_offset_in_first_block = (offset % block_size) as usize;
    unsafe {
        core::ptr::copy_nonoverlapping(
            dma.as_ptr().add(byte_offset_in_first_block),
            buf as *mut u8,
            amount,
        );
    }

    SQLITE_OK
}
```

**Key**: We always read full blocks from NVMe (hardware requirement), then
copy the exact byte range SQLite asked for. No Read-Modify-Write for reads.

#### xWrite — Read-Modify-Write for Partial Blocks

```rust
fn xwrite(file: *mut sqlite3_file, buf: *const c_void,
          amount: c_int, offset: i64) -> c_int
{
    let amount = amount as usize;
    let offset = offset as u64;

    let block_size = BLOCK_SIZE as u64;
    let start_block = offset / block_size;
    let end_block = (offset + amount as u64 - 1) / block_size;
    let block_count = end_block - start_block + 1;
    let start_lba = file.start_lba + start_block;

    // Grow file if needed
    if start_block + block_count > file.block_count {
        let extra = start_block + block_count - file.block_count;
        allocator.grow(file.start_lba, file.block_count, extra)?;
        file.block_count += extra;
    }

    let byte_offset_in_first_block = (offset % block_size) as usize;
    let is_aligned = byte_offset_in_first_block == 0 && amount % BLOCK_SIZE == 0;

    if is_aligned {
        // Fast path: DMA directly from a copy of SQLite's data
        let dma = DmaBuf::alloc(amount)?;
        dma.copy_from_slice(buf, amount);
        dma.flush_cache();
        let cmd = NvmeCommand::write(start_lba, block_count as u16 - 1, &dma);
        nvme.submit_and_wait(cmd)?;
    } else {
        // Slow path: Read-Modify-Write
        let dma = DmaBuf::alloc((block_count as usize) * BLOCK_SIZE)?;

        // 1. READ existing blocks from disk
        let cmd = NvmeCommand::read(start_lba, block_count as u16 - 1, &dma);
        nvme.submit_and_wait(cmd)?;
        dma.invalidate_cache();

        // 2. MODIFY: overlay SQLite's data onto the DMA buffer
        unsafe {
            core::ptr::copy_nonoverlapping(
                buf as *const u8,
                dma.as_mut_ptr().add(byte_offset_in_first_block),
                amount,
            );
        }

        // 3. WRITE the modified blocks back
        dma.flush_cache();
        let cmd = NvmeCommand::write(start_lba, block_count as u16 - 1, &dma);
        nvme.submit_and_wait(cmd)?;
    }

    // Update file byte length
    let new_end = offset + amount as u64;
    if new_end > file.byte_length {
        file.byte_length = new_end;
    }

    SQLITE_OK
}
```

#### xSync — The ACID Guarantee

**This is the most critical function in the entire VFS.**

```rust
fn xsync(file: *mut sqlite3_file, flags: c_int) -> c_int
{
    // 1. Flush the block allocator bitmap if dirty
    if allocator.dirty {
        allocator.flush(&mut nvme)?;
    }

    // 2. Flush the file table metadata
    file_table.flush(&mut nvme)?;

    // 3. Issue NVMe Flush command — forces all written data from the
    //    device's volatile write cache to non-volatile storage.
    let cmd = NvmeCommand {
        opcode: 0x00, // Flush
        nsid: 1,
        ..Default::default()
    };
    let result = nvme.submit_and_wait(cmd);

    match result {
        Ok(()) => SQLITE_OK,
        Err(_) => SQLITE_IOERR_FSYNC,
    }
}
```

**Without this NVMe Flush command, ACID guarantees do not exist on power loss.**
The device's volatile write cache may reorder or lose writes. The Flush
command is the barrier that makes WAL commit durable.

#### xShmMap, xShmLock, xShmBarrier, xShmUnmap — WAL Shared Memory

In WAL mode, SQLite uses shared memory to coordinate readers and the writer.
The `-shm` file contains the WAL index (a hash table of page numbers).

**In our single-address-space bare-metal kernel**, this is straightforward:

```rust
// The shm region is a kernel-allocated buffer, shared by reference.
// No mmap, no VMA — just a pointer.
static SHM_REGION: Mutex<Option<ShmState>> = Mutex::new(None);

struct ShmState {
    regions: Vec<(*mut u8, usize)>, // Up to SQLITE_SHM_NLOCK regions
    locks: [ShmLockState; SQLITE_SHM_NLOCK],
}

fn xshmmap(file: *mut sqlite3_file, region: c_int, region_size: c_int,
           extend: c_int, pp: *mut *mut c_void) -> c_int
{
    let mut shm = SHM_REGION.lock();
    let shm = shm.get_or_insert_with(ShmState::new);

    let idx = region as usize;

    // Extend if needed
    while shm.regions.len() <= idx {
        let buf = alloc::alloc(Layout::from_size_align(region_size as usize, 4096).unwrap());
        buf.write_bytes(0, region_size as usize);
        shm.regions.push((buf, region_size as usize));
    }

    *pp = shm.regions[idx].0 as *mut c_void;
    SQLITE_OK
}

fn xshmlock(file: *mut sqlite3_file, offset: c_int, n: c_int, flags: c_int) -> c_int
{
    let mut shm = SHM_REGION.lock();
    let shm = shm.as_mut().unwrap();

    // In single-process bare-metal: if we ever support multiple threads
    // accessing SQLite concurrently, implement reader/writer locks here.
    // For single-threaded use: always succeed.
    // For multi-threaded use:
    for i in offset..(offset + n) {
        let lock = &mut shm.locks[i as usize];
        if flags & SQLITE_SHM_LOCK != 0 {
            if flags & SQLITE_SHM_EXCLUSIVE != 0 {
                if !lock.try_exclusive() { return SQLITE_BUSY; }
            } else {
                if !lock.try_shared() { return SQLITE_BUSY; }
            }
        } else {
            lock.release();
        }
    }
    SQLITE_OK
}

fn xshmbarrier(_file: *mut sqlite3_file) {
    // Memory barrier — ensures all writes to shm are visible to other threads.
    core::sync::atomic::fence(Ordering::SeqCst);
}

fn xshmunmap(file: *mut sqlite3_file, delete_flag: c_int) -> c_int {
    if delete_flag != 0 {
        let mut shm = SHM_REGION.lock();
        if let Some(state) = shm.take() {
            for (ptr, size) in state.regions {
                alloc::dealloc(ptr, Layout::from_size_align(size, 4096).unwrap());
            }
        }
    }
    SQLITE_OK
}
```

**Key insight**: In a single-address-space OS, xShm is trivial — just
allocate RAM and hand out pointers. No page table manipulation needed.
If HeavenOS later adds process isolation, this becomes a `map_shared_pages()`
call in the page table code.

#### xSleep — Requires Timer Hardware

```rust
fn xsleep(_vfs: *mut sqlite3_vfs, microseconds: c_int) -> c_int {
    // Use APIC timer or HPET to sleep for the requested duration.
    // Cannot just yield — SQLite expects an actual time delay.
    timer::sleep_us(microseconds as u64);
    microseconds
}
```

The timer subsystem must be initialized during boot (calibrate TSC or
program APIC timer frequency).

#### xCurrentTime / xCurrentTimeInt64

```rust
fn xcurrenttime64(_vfs: *mut sqlite3_vfs, time: *mut i64) -> c_int {
    // Julian day number in milliseconds since noon on Nov 24, 4714 BC.
    // Read TSC or RTC, convert to Unix timestamp, then to Julian.
    let unix_ms = rtc::read_unix_time_ms();
    let julian_ms = unix_ms + 210866760000000_i64; // Unix epoch in Julian ms
    *time = julian_ms;
    SQLITE_OK
}
```

Requires an RTC driver (CMOS RTC on x86) or NTP-synced time source.

#### xRandomness

```rust
fn xrandomness(_vfs: *mut sqlite3_vfs, n: c_int, buf: *mut c_char) -> c_int {
    // Use RDRAND/RDSEED on modern x86, or a CSPRNG seeded from TSC jitter.
    for i in 0..n {
        *buf.add(i as usize) = rdrand_u8() as c_char;
    }
    n
}
```

### 5.5 Bootstrap Sequence

When the NVMe device is blank (no superblock magic):

```
1. Read LBA 0 → check magic bytes
2. If no magic:
   a. Query NVMe Identify Namespace → get LBA size, capacity
   b. Compute bitmap size = ceil(total_blocks / 8)
   c. Compute file table size = FILE_TABLE_ENTRIES * ENTRY_SIZE
   d. Write superblock to LBA 0
   e. Write zeroed bitmap to LBA 1..M (mark system LBAs as allocated)
   f. Write zeroed file table to LBA M+1..M+K
   g. NVMe Flush
   h. Open SQLite on the fresh allocator → creates main.db
   i. Execute schema DDL (CREATE TABLE for Styx metadata, etc.)
   j. NVMe Flush
3. If magic present:
   a. Read superblock → validate version, block_size
   b. Read bitmap into RAM
   c. Read file table into RAM
   d. Open SQLite → WAL recovery happens automatically
```

### 5.6 Block Cache

The VFS maintains a small LRU block cache to avoid redundant NVMe reads:

```rust
struct BlockCache {
    entries: BTreeMap<u64, CacheEntry>, // LBA → cached block
    lru: VecDeque<u64>,                 // LBA eviction order
    capacity: usize,                    // Max cached blocks (e.g., 256 = 1 MB)
}

struct CacheEntry {
    data: [u8; BLOCK_SIZE],
    dirty: bool,
}
```

SQLite already has its own page cache, so this is a small L2 to absorb
repeated reads to the same blocks (especially file table, bitmap, and
WAL index pages). Dirty cache entries are flushed on xSync.

**This partially replaces the Linux page cache** we gave up by going
bare metal. The tradeoff is explicit: we control exactly what's cached,
but we lose the kernel's LRU sophistication. For an embedded system
with predictable access patterns, this is acceptable.

---

## 6. SQLite Compilation for Bare Metal

SQLite is compiled as a single C file (`sqlite3.c`) with:

```makefile
CFLAGS += -DSQLITE_OS_OTHER=1          # Disable all built-in VFS
CFLAGS += -DSQLITE_OMIT_WAL=0          # Keep WAL support
CFLAGS += -DSQLITE_OMIT_LOAD_EXTENSION # No dlopen
CFLAGS += -DSQLITE_THREADSAFE=0        # Single-threaded (for now)
CFLAGS += -DSQLITE_TEMP_STORE=2        # Temp tables in memory
CFLAGS += -DSQLITE_DEFAULT_MEMSTATUS=0 # Disable memory tracking
CFLAGS += -nostdlib -ffreestanding      # No libc
CFLAGS += -target x86_64-unknown-none  # Bare metal target
```

Required libc shims (minimal):
- `memcpy`, `memset`, `memmove`, `memcmp` — from `compiler_builtins` or hand-written
- `strlen`, `strcmp` — trivial implementations
- `malloc`, `free`, `realloc` — routed to our kernel allocator via `SQLITE_CONFIG_MALLOC`

We do NOT need: `open`, `close`, `read`, `write`, `fstat`, `fcntl`,
`mmap`, `munmap`, `dlopen`, `getpid`, `sleep`, `gettimeofday` — these
are all replaced by VFS methods.

### 6.1 Custom Malloc

```rust
#[no_mangle]
pub extern "C" fn heavenos_malloc(n: c_int) -> *mut c_void {
    alloc::alloc(Layout::from_size_align(n as usize, 8).unwrap()) as *mut c_void
}

#[no_mangle]
pub extern "C" fn heavenos_free(ptr: *mut c_void) {
    // We need size tracking — use a header or a slab allocator
    // that can determine size from the pointer.
    slab_allocator::dealloc(ptr as *mut u8);
}

#[no_mangle]
pub extern "C" fn heavenos_realloc(ptr: *mut c_void, n: c_int) -> *mut c_void {
    slab_allocator::realloc(ptr as *mut u8, n as usize) as *mut c_void
}

// Registered at init:
// sqlite3_config(SQLITE_CONFIG_MALLOC, &methods);
```

**Note**: `sqlite3_free()` calls `xFree(ptr)` with no size argument.
Our allocator must be able to determine the allocation size from the
pointer alone (slab allocator, or size stored in a header before the
returned pointer).

---

## 7. Styx (9P2000) Namespace

### 7.1 How SQLite State Becomes a File Tree

```
/                           (root)
├── db/                     (direct SQL interface)
│   ├── ctl                 (write SQL, read results)
│   └── schema              (read: current schema DDL)
├── agents/                 (per-agent state)
│   ├── agent-001/
│   │   ├── status          (read: running/idle/error)
│   │   ├── config          (read/write: JSON config)
│   │   ├── log             (read: tail of agent log)
│   │   └── ctl             (write: start/stop/restart)
│   └── agent-002/
│       └── ...
├── hw/                     (hardware state)
│   ├── nvme/
│   │   ├── info            (read: model, capacity, temperature)
│   │   ├── smart           (read: SMART attributes)
│   │   └── stats           (read: IOPS, latency histogram)
│   └── gpu/
│       ├── info            (read: model, VRAM, clocks)
│       ├── ctl             (write: config commands)
│       ├── stats           (read: utilization, temperature)
│       └── compute/        (see Section 8)
├── sys/                    (system metadata)
│   ├── uptime              (read)
│   ├── meminfo             (read)
│   └── log                 (read: kernel log ring buffer)
└── net/                    (network stack)
    └── ...                 (Styx-over-TCP for remote access)
```

### 7.2 The /db/ctl Interface

```
$ echo "SELECT name FROM agents WHERE status='running'" > /db/ctl
$ cat /db/ctl
agent-001
agent-003
```

Implementation: the Styx server intercepts writes to `/db/ctl`, executes
them as SQL on the embedded SQLite instance, and buffers the result for
the next read. This is the **only** path for ad-hoc queries. Structured
agent access should use the typed paths under `/agents/`.

### 7.3 Synthetic Files

Most files under `/agents/` and `/hw/` are **synthetic** — generated on
read from SQLite queries or hardware register reads. They don't correspond
to disk blocks. The Styx server translates `Tread` into a query and
formats the result as a byte stream.

---

## 8. GPU Integration — Control Plane vs Data Plane

### 8.1 What Does NOT Go Through Files

- Model weights (GB)
- Activation tensors (hundreds of MB)
- KV cache (hundreds of MB per inference context)
- Intermediate compute results between GPU kernel launches

These reside in **VRAM** and move via **DMA between system RAM and VRAM**,
never through SQLite, never through Styx.

### 8.2 What Goes Through Files (Control Plane)

- Model selection: `echo "load mistral-7b" > /hw/gpu/compute/ctl`
- Inference request: `echo '{"prompt":"hello","max_tokens":128}' > /hw/gpu/compute/ctl`
- Status: `cat /hw/gpu/compute/status` → `"generating token 42/128"`
- Result: `cat /hw/gpu/compute/result` → `"Hello! How can I help you?"`

### 8.3 The Actual GPU Data Path

```
                    Control Plane (Styx)          Data Plane (shared memory + DMA)
                    ────────────────────          ──────────────────────────────────

Agent writes       "load model X"                 Kernel reads model weights from
to /gpu/ctl  ───────────────────────►             NVMe → DMA to system RAM → DMA
                                                  to VRAM via PCIe BAR/IOMMU
                                                        │
Agent writes       "infer: prompt"                      │
to /gpu/ctl  ───────────────────────►             Tokenize prompt, DMA token IDs
                                                  to VRAM input buffer
                                                        │
                                                  GPU executes attention layers,
                                                  MLP, softmax — thousands of
                                                  kernel launches, all in VRAM
                                                        │
                                                  Output token DMA'd back to RAM
                                                        │
Agent reads        "generated text"               ◄─────┘
from /gpu/result ◄───────────────────
```

### 8.4 GPU BAR Mapping — Limitations

The GPU exposes VRAM through BAR0 (Base Address Register 0) on PCIe.
Typical constraints:

- **BAR0 window is often 256 MB** even on GPUs with 8+ GB VRAM
- The kernel maps BAR0 into its virtual address space during PCI enumeration
- For VRAM access beyond the BAR window: use **GPU-side DMA engines**
  (the GPU copies data between visible and non-visible VRAM regions)
- CPU writes to BAR-mapped VRAM are **uncacheable** (mapped as UC or WC
  in page tables) — no CPU cache coherence issues, but writes are slow
  (~1-2 GB/s vs ~30 GB/s for cached RAM)

**Correct approach**: Use CPU→VRAM only for small control data.
For bulk transfers, use the GPU's DMA engine or the host-side IOMMU
to DMA from system RAM to VRAM without CPU involvement.

### 8.5 The Inference Engine is Kernel Code

The GPU compute pipeline (shader compilation, command buffer management,
descriptor sets, queue submission, fence synchronization) runs in **kernel
space as a Rust driver**. It is not abstracted behind files.

An agent that wants to run inference sends a control message via Styx.
The kernel's inference driver does the actual work:

```rust
pub struct InferenceDriver {
    gpu: GpuDriver,
    models: BTreeMap<String, LoadedModel>,
}

pub struct LoadedModel {
    weights_vram: VramAllocation,
    kv_cache: VramAllocation,
    config: ModelConfig,
}

impl InferenceDriver {
    /// Load model weights from NVMe to VRAM.
    pub fn load_model(&mut self, name: &str) -> Result<(), InferenceError>;

    /// Run inference. Returns generated tokens.
    pub fn infer(&mut self, model: &str, prompt: &[u32], params: InferenceParams)
        -> Result<Vec<u32>, InferenceError>;
}
```

---

## 9. WASM Agent Sandbox (Future)

### 9.1 Realistic Assessment

| Option           | Maturity  | Speed       | no_std    | Verdict                           |
| ---------------- | --------- | ----------- | --------- | --------------------------------- |
| wasmtime         | High      | JIT (fast)  | No        | Requires OS-level porting effort  |
| wasmi            | Medium    | Interpreter | Yes       | 10-50x slower, but works today    |
| wasm3            | Medium    | Interpreter | Yes       | Fastest interpreter, C-based      |
| Custom JIT       | N/A       | JIT (fast)  | Yes       | Months of work                    |

### 9.2 Recommended Path

**Phase 1**: Use `wasmi` (pure Rust, `no_std`). Accept the performance
penalty for agent orchestration logic (which is not compute-heavy).

**Phase 2**: Port wasm3 (C) to bare metal — it's smaller than wasmtime
and designed for embedded use.

**Phase 3**: If JIT performance is needed, investigate cranelift-jit
isolation (the codegen backend is separable from wasmtime's runtime).

### 9.3 What Agents Can Do

Agents run in WASM and communicate with the kernel through **host functions**,
NOT through emulated WASI filesystem calls:

```rust
// Host functions exposed to WASM agents:
extern "C" {
    /// Send a Styx message. Returns response bytes.
    fn styx_request(msg_ptr: *const u8, msg_len: u32,
                    resp_ptr: *mut u8, resp_cap: u32) -> i32;

    /// Request inference on a loaded model.
    fn inference_request(prompt_ptr: *const u8, prompt_len: u32,
                         params_ptr: *const u8, params_len: u32,
                         result_ptr: *mut u8, result_cap: u32) -> i32;

    /// Log a message to the kernel log.
    fn log(level: u32, msg_ptr: *const u8, msg_len: u32);
}
```

This avoids the WASI compatibility problem entirely. We don't pretend
WASM agents are POSIX programs. They're sandboxed plugins with a
well-defined kernel ABI.

### 9.4 Memory Budget

| Component            | Estimate      | Notes                            |
| -------------------- | ------------- | -------------------------------- |
| Kernel + drivers     | 16-32 MB      | Rust binary, page tables, stacks |
| SQLite instance       | 2-8 MB        | Page cache + WAL                 |
| Block cache          | 1-4 MB        | 256-1024 cached blocks           |
| WASM runtime (wasmi) | 1-2 MB        | Per-module overhead              |
| WASM agent memory    | 4-16 MB each  | Linear memory per agent          |
| × 4 agents           | 16-64 MB      |                                  |
| GPU VRAM             | Separate      | Not counted in system RAM        |
| **Total RAM**        | **~64-128 MB** | Fits in 512 MB with headroom    |

---

## 10. Concurrency Model

### 10.1 Phase 1 — Single-Threaded Cooperative

- One main loop: poll NVMe completions, service Styx requests, run
  WASM agent steps (cooperative yield points in the interpreter)
- SQLite is `THREADSAFE=0` — no mutexes needed
- `xShmLock` always succeeds (single accessor)

### 10.2 Phase 2 — Multi-Core

- One NVMe I/O queue pair per core
- SQLite access serialized through a spinlock or ticket lock
- `THREADSAFE=1`, VFS locking becomes real
- WASM agents pinned to cores, communicate via message queues

---

## 11. Implementation Order

```
Phase 0: Boot + Memory                    [Weeks 1-4]
  ├── UEFI bootloader → long mode
  ├── Physical page allocator (bitmap)
  ├── Kernel heap (slab allocator)
  ├── APIC timer + TSC calibration
  └── Serial console for debug output

Phase 1: NVMe Driver                      [Weeks 5-8]
  ├── PCIe enumeration (find NVMe controller)
  ├── Admin queue setup
  ├── I/O queue pair setup
  ├── Read/Write/Flush commands
  ├── DMA buffer allocator
  └── Error handling + timeout

Phase 2: SQLite VFS                        [Weeks 9-14]
  ├── Block allocator (bitmap on disk)
  ├── File table
  ├── Compile sqlite3.c for bare metal
  ├── libc shims (memcpy, malloc, etc.)
  ├── VFS: xOpen, xClose, xDelete
  ├── VFS: xRead (non-aligned), xWrite (RMW)
  ├── VFS: xSync (NVMe Flush)
  ├── VFS: xShmMap/Lock/Barrier/Unmap
  ├── VFS: xSleep, xCurrentTime, xRandomness
  ├── Bootstrap (blank disk → initialized DB)
  ├── Block cache
  └── Crash recovery testing

Phase 3: Styx Server                       [Weeks 15-18]
  ├── 9P2000 message parsing
  ├── Synthetic file tree
  ├── /db/ctl SQL interface
  ├── /hw/* hardware info files
  └── TCP transport (Styx-over-TCP)

Phase 4: GPU Driver                        [Weeks 19-24]
  ├── PCI enumeration for GPU
  ├── BAR0 mapping
  ├── Command buffer / queue submission
  ├── Simple compute shader test
  ├── DMA: RAM ↔ VRAM
  └── Inference driver skeleton

Phase 5: WASM Agents                       [Weeks 25-28]
  ├── wasmi integration
  ├── Host function ABI
  ├── Agent lifecycle (load/start/stop)
  └── Styx integration for agents
```

---

## 12. What This Design Does NOT Cover (Intentionally)

- **Networking stack**: TCP/IP is a separate subsystem. Styx-over-TCP
  needs it but the Styx protocol itself is transport-agnostic.
- **Display / framebuffer**: Not needed for a headless AI server.
- **USB / keyboard**: Serial console suffices for Phase 1-5.
- **Multi-user security**: Single-operator system initially.
- **Redundancy / RAID**: Single NVMe device. Backups are the operator's
  responsibility.

---

## 13. Risks and Open Questions

| Risk                               | Mitigation                                      |
| ---------------------------------- | ----------------------------------------------- |
| SQLite C code has hidden libc deps | Audit with `nm`; stub missing symbols            |
| NVMe controller quirks             | Test on 2-3 different NVMe models               |
| Block allocator fragmentation      | Monitor, add compaction if needed                |
| WASM interpreter too slow for agents | Profile first; wasm3 fallback; native Rust escape hatch |
| GPU driver complexity              | Start with one GPU vendor (e.g., simple virtio-gpu for QEMU) |
| Timer drift without NTP            | Acceptable for non-distributed initial design   |

---

*This document is the architecture. The implementation is the next step.*
