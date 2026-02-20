# HeavenOS / OSqlite Security & Code Audit

**Date:** 2026-02-20
**Scope:** Full codebase audit — kernel, drivers, SQLite integration, networking, VFS
**Codebase:** ~8,300 lines of Rust + ~800 lines of C stubs + SQLite 3.51.2 amalgamation

---

## Project Summary

HeavenOS is a bare-metal x86_64 kernel written in Rust that boots via the Limine
bootloader and integrates SQLite backed by NVMe storage. It includes:

- Physical page allocator (bitmap), slab heap allocator, DMA buffer management
- NVMe block driver with PRP list support
- VirtIO legacy networking with smoltcp TCP/IP stack
- In-kernel TLS 1.3 (embedded-tls) for direct HTTPS to the Anthropic API
- SQLite 3.51.2 with a custom VFS bridging to NVMe block I/O
- On-disk filesystem: superblock + bitmap allocator + file table
- Styx (Plan 9-style) namespace server
- Interactive serial shell

---

## Findings by Severity

### P0 — Critical

#### 1. TLS Certificate Verification Disabled
**File:** `kernel/src/api/mod.rs:155`
```rust
UnsecureProvider::new::<Aes128GcmSha256>(rng),
```
The TLS connection to api.anthropic.com uses `UnsecureProvider`, which **skips all
certificate validation**. Any machine on the network path (e.g., QEMU host, upstream
router) can perform a man-in-the-middle attack, intercepting the API key sent in
`X-API-Key` headers and all request/response content.

**Impact:** Complete compromise of API credentials and data confidentiality.
**Recommendation:** Implement certificate pinning or embed a root CA certificate
for the Anthropic API endpoint. `embedded-tls` supports certificate verification
via `CertVerifier` — use it with a pinned certificate or a minimal CA bundle.

---

### P1 — High

#### 2. NVMe I/O Spin Loops Have No Timeout
**Files:** `kernel/src/drivers/nvme/mod.rs:209-215, 267-279, 307-317, 335-346`

All NVMe command submission paths (`admin_submit_wait`, `read_blocks`, `write_blocks`,
`flush`) spin-loop indefinitely waiting for completion:
```rust
loop {
    if let Some(status) = qp.poll_completion() { ... }
    core::hint::spin_loop();
}
```
If the NVMe controller becomes unresponsive (hardware fault, firmware hang), the
kernel locks up permanently with no recovery path.

**Impact:** Kernel hang on any NVMe hardware fault.
**Recommendation:** Add a TSC-based timeout (e.g., 30 seconds) to all command
submission loops. Return `NvmeError::Timeout` on expiry.

#### 3. `block_count` Truncated to `u16` Without Bounds Check
**Files:** `kernel/src/vfs/sqlite_vfs.rs:239, 371, 382`

NVMe `read_blocks`/`write_blocks` accept `block_count: u16`, but the VFS computes
block counts as `u64` and casts directly:
```rust
nvme.read_blocks(start_lba, block_count as u16, &mut dma)
```
If a SQLite write spans more than 65535 blocks (~256 MB at 4K block size), the cast
silently truncates, causing partial reads/writes and **data corruption**.

**Impact:** Silent data corruption on large I/O operations.
**Recommendation:** Add a bounds check before the cast. Split large I/O operations
into multiple NVMe commands of at most `u16::MAX` blocks each.

#### 4. `strtol` / `strtoll` Integer Overflow
**File:** `kernel/vendor/sqlite/heaven_stubs.c:130`
```c
result = result * base + digit;
```
No overflow check. `strtoll` and `strtoull` are implemented as simple casts from
`strtol`, inheriting the overflow bug and losing the full 64-bit range. SQLite
relies on these for parsing integer literals — overflows will produce incorrect
query results silently.

**Impact:** Incorrect SQL query results for large integer values.
**Recommendation:** Implement proper 64-bit `strtoll`/`strtoull` with overflow
detection. Have `strtol` call `strtoll` (not the reverse).

#### 5. `sprintf` Assumes 1024-byte Buffer
**File:** `kernel/vendor/sqlite/heaven_stubs.c:486-494`
```c
int sprintf(char *buf, const char *fmt, ...) {
    int ret = vsnprintf(buf, 1024, fmt, ap);
}
```
If the caller passes a buffer smaller than 1024 bytes, this overflows.
SQLite itself uses `sqlite3_snprintf` internally, but the libc stubs may be called
from math/string functions during number formatting.

**Impact:** Stack/heap buffer overflow if called with small buffers.
**Recommendation:** Use a smaller default (e.g., 256) or audit all call sites to
confirm minimum buffer sizes. Alternatively, implement `sprintf` as a thin wrapper
that always writes the full length.

---

### P2 — Medium

#### 6. `static mut` in GDT/TSS
**File:** `kernel/src/arch/x86_64/gdt.rs:80, 113`
```rust
static mut TSS: Tss = ...;
static mut GDT: Gdt = ...;
```
`static mut` is being deprecated in Rust due to unsoundness. While these are only
written during single-threaded boot, the pattern is fragile and prevented by
future Rust editions.

**Recommendation:** Wrap in `UnsafeCell` with a `Sync` marker, or use `spin::Once`
for one-time initialization.

#### 7. RDRAND Failure Returns Zero
**File:** `kernel/src/crypto/mod.rs:40-41`
```rust
fn next_u32(&mut self) -> u32 {
    Self::rdrand64().unwrap_or(0) as u32
}
```
If RDRAND fails all 32 retries, the RNG returns 0. This would make TLS session
keys predictable. While RDRAND failure is extremely rare, the failure mode should
be a panic rather than silent degradation.

**Impact:** Cryptographic weakness if RDRAND hardware fails.
**Recommendation:** Panic on RDRAND failure in `next_u32`/`next_u64`, or at minimum
log a warning and retry with a longer backoff.

#### 8. `qsort` Silent Failure for Large Elements
**File:** `kernel/vendor/sqlite/heaven_stubs.c:726-727`
```c
char tmp[256];
if (width > sizeof(tmp)) return; /* safety */
```
If SQLite tries to sort elements larger than 256 bytes, `qsort` silently returns
without sorting. This would produce incorrect `ORDER BY` results.

**Impact:** Incorrect query results for sorts with large row structures.
**Recommendation:** Use a heap-allocated temporary buffer (`heavenos_malloc`) for
elements exceeding the stack buffer, or increase the stack buffer to 512 bytes
(SQLite internal sort records are typically < 500 bytes).

#### 9. PCI Scan Only Checks Function 0
**Files:** `kernel/src/drivers/nvme/pci.rs:47-48`, `kernel/src/drivers/virtio/net.rs:258`

Both PCI scans iterate `bus 0..255, device 0..31` but only check function 0. Devices
behind PCI bridges or multi-function devices will be missed.

**Recommendation:** Check the Header Type register (offset 0x0E) bit 7 to detect
multi-function devices and scan functions 0..7 when set.

#### 10. CMOS RTC Read Race Condition
**File:** `kernel/src/vfs/sqlite_vfs.rs:700-709`

The RTC read waits for "not updating" then reads all registers sequentially, but the
RTC could start a new update cycle between reading individual registers, producing
inconsistent timestamps (e.g., 23:59:59 on day 1 → 00:00:00 on day 1 instead of
day 2).

**Recommendation:** Read the RTC twice and compare results. If they differ, read
again. This is the standard "double-read" algorithm for CMOS RTC.

#### 11. Lock Ordering Not Enforced at Compile Time
**Files:** `kernel/src/vfs/sqlite_vfs.rs` (multiple methods)

Lock ordering (NVME → allocator → file_table) is documented in comments but could
be violated by future code changes. There is no compile-time or runtime enforcement.

**Recommendation:** Consider a lock-ordering wrapper that panics in debug builds
if locks are acquired out of order, or use a single coarse lock for the VFS to
eliminate the deadlock risk entirely (acceptable for single-threaded SQLite).

---

### P3 — Low

#### 12. API Key Stored in Plaintext Memory
**File:** `kernel/src/api/mod.rs:453`
The API key is stored as a `String` in a `Mutex<Option<String>>`. No zeroization
on drop. A memory dump or panic dump would expose it. Acceptable for a research
kernel but not for production.

#### 13. HTTP Request Formatting Has Extra Whitespace
**File:** `kernel/src/api/mod.rs:76-89`
The HTTP request uses a Rust multi-line string that introduces leading whitespace
on the body line. Most HTTP servers tolerate this, but it technically violates the
HTTP/1.1 specification (no whitespace between headers and body).

#### 14. `extract_non_streaming_content` Naive JSON Parsing
**File:** `kernel/src/api/mod.rs:342-348`
The fallback JSON extractor finds the first `"text":"` and reads until the next
unescaped `"`. This breaks on escaped quotes within the content text.

#### 15. File Table Limited to 42 Entries
**File:** `kernel/src/storage/file_table.rs:17`
The file table occupies a single 4096-byte block, limiting to 42 files. Sufficient
for SQLite's needs (main.db + journal + WAL + temp), but prevents future expansion.

#### 16. Physical Allocator Limited to 4 GiB
**File:** `kernel/src/mem/phys.rs:74`
`MAX_PAGES = 1M = 4 GiB`. Systems with more RAM will have the excess ignored.
Acceptable for current QEMU-based development.

#### 17. Heap Allocator Slab Refill Holds Global Lock
**File:** `kernel/src/mem/heap.rs:83-119`
When a slab class is empty, `refill_class` allocates a physical page while holding
the slab allocator mutex. This blocks all other allocations during refill. Acceptable
for single-threaded kernel, but would be a bottleneck with SMP.

#### 18. Shell `cat` Command Has Hardcoded Paths
**File:** `kernel/src/shell/commands.rs:214-229`
The `cat` command only recognizes a few hardcoded paths rather than reading from the
actual Styx namespace. The comment says "when the Styx server is wired in" — this
is incomplete integration.

---

## Positive Observations

1. **Structure size assertions** — `static_assertions::const_assert_eq!` verifies
   NVMe SQE (64B), CQE (16B), FileEntry (96B), Superblock (<=4096B), and TSS (104B)
   at compile time.

2. **Proper volatile MMIO** — All NVMe BAR0 register accesses use `read_volatile` /
   `write_volatile`, preventing the compiler from eliding or reordering MMIO.

3. **DMA cache coherence** — `DmaBuf::flush_cache()` (clflushopt + sfence before
   device reads) and `invalidate_cache()` (clflushopt + mfence after device writes)
   correctly maintain cache coherence for DMA transfers.

4. **ACID sync path** — The VFS `sync()` method writes bitmap → file table → NVMe
   Flush in order, with consistent lock acquisition. This provides crash-safe
   durability for SQLite commits.

5. **Guard page for stack overflow** — The kernel allocates a guarded stack (unmapped
   page at bottom), and the page fault handler detects hits to the guard page,
   printing diagnostics instead of silently corrupting memory.

6. **Double-fault IST1 stack** — The double-fault handler runs on a separate IST1
   stack, preventing triple faults when the main kernel stack overflows.

7. **Header injection prevention** — The API client validates that model name and
   API key don't contain `\r` or `\n` before building HTTP headers
   (`kernel/src/api/mod.rs:62-67`).

8. **SQLite hardening** — The SQLite configuration disables WAL, shared cache,
   extensions, and tracing (`SQLITE_OMIT_*`), reducing attack surface.
   `SQLITE_DQS=0` prevents the common mistake of using double-quoted strings as
   string literals.

9. **Double-free prevention** — Both the physical page allocator
   (`kernel/src/mem/phys.rs:219`) and block allocator
   (`kernel/src/storage/block_alloc.rs:281`) silently skip already-freed entries
   instead of corrupting the free count.

10. **Crash-safe file growth** — The VFS write path allocates new blocks, copies
    data, flushes, updates the file table, then frees old blocks — ensuring the
    file table always points to valid data even on power loss
    (`kernel/src/vfs/sqlite_vfs.rs:293-341`).

---

## Summary

| Severity | Count | Key Themes |
|----------|-------|------------|
| P0 Critical | 1 | TLS cert verification disabled |
| P1 High | 4 | NVMe hangs, integer truncation, C stub overflow |
| P2 Medium | 6 | `static mut`, RNG failure mode, qsort, PCI scan, RTC race |
| P3 Low | 7 | API key storage, JSON parsing, capacity limits |

The kernel demonstrates competent bare-metal systems programming with good attention
to DMA coherence, ACID durability, and stack safety. The most critical issue is the
disabled TLS certificate verification, which completely undermines the security of
the API communication channel. The NVMe timeout and integer truncation issues are
the next priorities, as they can cause hangs and data corruption respectively.
