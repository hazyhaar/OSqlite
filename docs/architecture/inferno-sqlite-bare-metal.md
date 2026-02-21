# HeavenOS — Inferno/SQLite Bare-Metal Architecture

## Design Document v2.0

**Scope**: This document specifies how SQLite operates directly on NVMe
hardware (no Linux, no libc) inside an Inferno/Plan 9-inspired Rust kernel,
how the system exposes state through a namespace backed by SQLite, and how
an agentic loop allows Claude to read, write, and query the system via
tool use over TLS — all from bare metal.

**Status**: Phases 0-3 of the original plan are fully implemented. The AI
inference path pivoted from local GPU compute to Anthropic API access over
an in-kernel TLS 1.3 stack. The agent sandbox pivoted from WASM to Lua 5.5.

---

## 1. Principles

1. **SQLite is the system database, not the system bus.** It stores metadata,
   configuration, agent state, logs, and structured queries. It does NOT
   carry GPU tensors, model weights, or bulk inference data.

2. **"Everything is a file" is the control plane.** The namespace exposes
   system state as paths. Agents read/write files through the namespace,
   backed by the SQLite `namespace` table. Bulk data moves through shared
   memory and DMA, signaled (not transported) via the namespace.

3. **No magic words.** Every claim about performance, isolation, or
   correctness must be backed by a concrete mechanism. If we say "zero
   copy," we show the physical address flow. If we say "crash safe," we
   show the xSync -> NVMe Flush path.

4. **Implement the hard 80%.** The VFS shim, the NVMe flush path, the
   non-aligned I/O, the shared memory for WAL — these are the actual
   engineering work, not the architectural diagram.

5. **The AI is remote, not local.** Instead of running inference on a local
   GPU, the system calls the Anthropic API over TLS 1.3 from bare metal.
   Claude acts as the intelligence layer; OSqlite is the execution substrate.

---

## 2. Hardware Assumptions

| Component        | Assumption                                          |
| ---------------- | --------------------------------------------------- |
| CPU              | x86_64 with MMU, APIC timer, TSC, RDRAND           |
| NVMe             | PCIe-attached, supports PRP lists, Flush command    |
| NIC              | virtio-net (QEMU) or compatible                    |
| RAM              | >= 512 MB, accessed via HHDM (higher-half direct map)|
| GPU (future)     | PCIe, BAR0-mapped, vendor-specific command protocol |
| Boot             | Limine bootloader (v9.x, limine protocol)           |

---

## 3. Memory Model

HeavenOS uses a **hybrid memory model**:

- **Kernel space**: higher-half direct map (HHDM) provided by Limine.
  All physical memory is mapped at a fixed offset (typically 0xFFFF800000000000).
  `PhysAddr::as_ptr()` adds the HHDM offset to convert phys->virt.
- All code runs in ring 0 in a single address space.

### 3.1 Physical Page Allocator

**Implemented**: `kernel/src/mem/phys.rs`

A bitmap allocator tracks 4 KiB pages (up to 4 GiB via 128 KiB bitmap):

```rust
/// Allocate `count` physically contiguous pages, aligned to `align` pages.
fn alloc_pages_contiguous(count: usize, align: usize) -> Result<PhysAddr, AllocError>;

/// Free previously allocated pages.
fn free_pages(base: PhysAddr, count: usize);
```

**Why contiguous**: NVMe PRP lists require that each entry points to a
physical page. For transfers > 4 KiB, either the pages are contiguous
(single PRP entry) or we build a PRP list (scattered pages). Both paths
are supported.

### 3.2 DMA-Safe Allocator

**Implemented**: `kernel/src/mem/dma.rs`

Wraps the physical page allocator. Guarantees:
- Pages are not in CPU cache (flushed before DMA read, invalidated after
  DMA write via clflushopt + mfence)
- Alignment >= controller's minimum transfer unit
- Allocations tracked for cleanup on error paths

```rust
pub struct DmaBuf {
    virt: *mut u8,
    phys: PhysAddr,
    len: usize,
}
```

---

## 4. NVMe Driver

**Implemented**: `kernel/src/drivers/nvme/` (mod.rs, queue.rs, command.rs, pci.rs)

### 4.1 Queue Pair Architecture

```
+-------------------------------------+
|              NVMe Driver             |
|                                      |
|  Admin Queue (1 SQ + 1 CQ)          |
|    - Identify Controller/Namespace   |
|    - Create I/O Queues               |
|                                      |
|  I/O Queue Pair 0 (SQ + CQ)         |
|    - Read / Write / Flush            |
|    - TSC-based polling (30s timeout) |
|                                      |
|  (Future: 1 QP per CPU core)        |
+-------------------------------------+
```

### 4.2 Command Submission

Synchronous submit-and-wait with TSC-based timeout. Large transfers
(>65535 blocks) are automatically chunked. PRP lists constructed in
DMA-safe memory for multi-page transfers.

### 4.3 Error Classification

| NVMe Status               | SQLite Error           | Recovery                    |
| -------------------------- | ---------------------- | --------------------------- |
| Success                    | SQLITE_OK              | --                          |
| Invalid Opcode/Field       | SQLITE_MISUSE          | Bug in driver               |
| Data Transfer Error        | SQLITE_IOERR_READ/WRITE | Retry once, then fail      |
| Namespace Not Ready        | SQLITE_BUSY            | Retry with backoff          |
| Internal Device Error      | SQLITE_IOERR           | Controller reset path       |

---

## 5. SQLite VFS Implementation

**Implemented**: `kernel/src/vfs/sqlite_vfs.rs`, `kernel/src/storage/`

### 5.1 Overview

SQLite's VFS is an interface with ~20 methods. We implement a custom VFS
named `"heavenos"` registered via `sqlite3_vfs_register()`.

The VFS manages a **block allocator** on the NVMe namespace, not a
traditional filesystem. SQLite sees "files" but the VFS translates them
to LBA ranges on raw disk.

### 5.2 Block Allocator

**Implemented**: `kernel/src/storage/block_alloc.rs`

```
LBA 0:        Superblock (magic=0x0000_01_534F4E5648 "HVNOS", version, block_size, total_blocks)
LBA 1..M:     Bitmap (1 bit per block: 0=free, 1=allocated)
LBA M+1..M+K: File table (42 fixed entries)
LBA M+K+1..:  Data blocks
```

Operations: `format()` (blank disk), `load()` (existing disk), `alloc()`,
`free()`, `grow()`, `flush()` (bitmap -> NVMe Flush).

### 5.3 File Table

**Implemented**: `kernel/src/storage/file_table.rs`

42 entries per 4 KiB block. Each entry:
`{ name: [u8; 64], start_block: u64, block_count: u64, byte_length: u64, flags: u32 }`

| Slot | Name              | Purpose                          |
| ---- | ----------------- | -------------------------------- |
| 0    | `main.db`         | Primary SQLite database          |
| 1    | `main.db-wal`     | WAL file                         |
| 2    | `main.db-shm`     | Shared memory (WAL index)        |
| 3    | `main.db-journal` | Rollback journal                 |
| 4-7  | `temp_N`          | Temp files for sort/materialize  |

### 5.4 VFS Methods

All critical VFS methods are implemented:

- **xOpen**: Look up or create file table entry
- **xRead**: Non-aligned reads (full block DMA -> copy requested bytes)
- **xWrite**: Aligned fast path (direct DMA) or Read-Modify-Write for
  partial blocks
- **xSync**: Bitmap flush + file table flush + **NVMe Flush** (ACID guarantee)
- **xShmMap/Lock/Barrier/Unmap**: RAM-backed WAL index (trivial in
  single-address-space kernel)
- **xSleep**: TSC-calibrated busy-wait
- **xCurrentTime**: Monotonic timestamp via TSC
- **xRandomness**: RDRAND

**xSync is the most critical function.** Without the NVMe Flush command,
ACID guarantees do not exist on power loss. The Flush command is the barrier
that makes WAL commit durable.

### 5.5 Block Cache

**Decision**: Omitted. SQLite's own page cache (1-4 MB) absorbs most
repeated reads. A VFS-level LRU cache was planned in v1.0 but proved
unnecessary given the QEMU/NVMe latency profile. Can be added later if
profiling shows redundant NVMe reads on real hardware.

### 5.6 Bootstrap Sequence

```
1. Read LBA 0 -> check magic bytes
2. If no magic:
   a. Query NVMe Identify Namespace -> LBA size, capacity
   b. Write superblock, zeroed bitmap, zeroed file table
   c. NVMe Flush
   d. Open SQLite -> creates main.db
   e. CREATE TABLE namespace (path, type, content, mode, mtime)
   f. CREATE TABLE audit (id, ts, level, agent, action, target, detail)
   g. NVMe Flush
3. If magic present:
   a. Read superblock, validate version
   b. Read bitmap + file table into RAM
   c. Open SQLite -> WAL recovery happens automatically
```

---

## 6. SQLite Compilation for Bare Metal

**Implemented**: `kernel/src/sqlite/` (mod.rs, ffi.rs, vfs_bridge.rs)

SQLite is compiled as a single C file (`sqlite3.c`) with:

```makefile
CFLAGS += -DSQLITE_OS_OTHER=1          # Disable all built-in VFS
CFLAGS += -DSQLITE_OMIT_WAL=0          # Keep WAL support
CFLAGS += -DSQLITE_OMIT_LOAD_EXTENSION # No dlopen
CFLAGS += -DSQLITE_THREADSAFE=0        # Single-threaded
CFLAGS += -DSQLITE_TEMP_STORE=2        # Temp tables in memory
CFLAGS += -DSQLITE_DEFAULT_MEMSTATUS=0 # Disable memory tracking
CFLAGS += -nostdlib -ffreestanding
```

Required libc shims: `memcpy`, `memset`, `memmove`, `memcmp`, `strlen`,
`strcmp` (from `compiler_builtins`), plus `malloc`/`free`/`realloc` routed
to the kernel allocator via `SQLITE_CONFIG_MALLOC`.

### 6.1 Public API

```rust
pub static DB: Mutex<Option<SqliteDb>> = Mutex::new(None);

impl SqliteDb {
    pub fn open(name: &str) -> Result<Self, String>;
    pub fn exec(&self, sql: &str) -> Result<(), String>;
    pub fn query(&self, sql: &str) -> Result<QueryResult, String>;
    pub fn query_value(&self, sql: &str) -> Result<Option<String>, String>;
    pub fn query_column(&self, sql: &str) -> Result<Vec<String>, String>;
}
```

---

## 7. Namespace

**Implemented**: `kernel/src/fs/styx/` + `kernel/src/shell/commands.rs`

### 7.1 Design Change from v1.0

The original plan called for a full Styx (9P2000) protocol server with
synthetic files. The actual implementation uses a **hybrid approach**:

- **SQLite `namespace` table** stores all persistent namespace entries
  (path, type, content, mtime). This is the source of truth.
- **Shell commands** (`ls`, `cat`, `sql`) provide direct access.
- **Lua builtins** (`read()`, `write()`, `ls()`, `sql()`) provide
  programmatic access.
- **Agent tools** (`read_file`, `write_file`, `list_dir`, `sql_query`,
  `str_replace`) expose the same operations to Claude via tool_use.
- The Styx 9P2000 message parser exists for future TCP transport.

### 7.2 Namespace Layout

```
/                           (root)
+-- db/                     (direct SQL interface)
|   +-- ctl                 (sql command: exec_and_format)
|   +-- schema              (SELECT sql FROM sqlite_master)
+-- agents/                 (Lua scripts stored in namespace table)
|   +-- indexer             (content = Lua source code)
|   +-- monitor
|   +-- ...
+-- hw/                     (hardware state, synthetic)
|   +-- nvme/
|       +-- info            (NVMe controller info)
+-- sys/                    (system metadata, synthetic)
|   +-- uptime              (monotonic uptime)
|   +-- meminfo             (physical memory stats)
```

### 7.3 Access Paths

| Access Method    | Read                        | Write                        |
| ---------------- | --------------------------- | ---------------------------- |
| Shell            | `cat /agents/indexer`       | `store /agents/indexer <code>` |
| Lua              | `read("/agents/indexer")`   | `write("/agents/indexer", code)` |
| Agent (Claude)   | tool: `read_file`           | tool: `write_file` / `str_replace` |
| SQL              | `sql SELECT ...`            | `sql INSERT ...` (REPL only) |

---

## 8. Network Stack

**Implemented**: `kernel/src/net/` + `kernel/src/drivers/virtio/`

Not in the original v1.0 plan. Added to enable Claude API access.

### 8.1 Architecture

```
+------------------------------------------------------+
|  Claude API Client (api/)                            |
|    TLS 1.3 (embedded-tls, AES-128-GCM, P-256)       |
+------------------------------------------------------+
|  TCP / UDP sockets (smoltcp)                         |
+------------------------------------------------------+
|  IP: 10.0.2.15  GW: 10.0.2.2  DNS: 10.0.2.3        |
+------------------------------------------------------+
|  virtio-net driver (drivers/virtio/net.rs)           |
|    DMA ring buffers, MAC address from device config  |
+------------------------------------------------------+
|  QEMU user-mode networking (-netdev user)            |
+------------------------------------------------------+
```

### 8.2 DNS Resolver

**Implemented**: `kernel/src/net/dns.rs`

RFC 1035 A-record queries over UDP to QEMU's DNS forwarder (10.0.2.3).
8-entry cache with TTL (capped at 5 min), proper oldest-entry eviction.
Handles compression pointers.

### 8.3 TLS 1.3

**Implemented**: `kernel/src/crypto/`

- **embedded-tls** 0.18: In-kernel TLS 1.3 with AES-128-GCM + P-256
- **RDRAND RNG**: Hardware random via `RdRandRng` (implements `CryptoRng`)
- **DER parser**: `kernel/src/crypto/der.rs` — X.509 certificate parsing
- **SPKI pin infrastructure**: SHA-256 hash of server public key, runtime
  set/clear via `pin set <hex>` / `pin clear` shell commands

**Known limitation**: `ENFORCE_PINNING = false` because embedded-tls 0.18
marks `CertificateRef.entries` as `pub(crate)`, preventing external
certificate inspection. The complete infrastructure (DER parser, SHA-256
via `sha2` crate, shell commands) is ready and waiting for the library
to expose the certificate chain.

---

## 9. Claude API Client

**Implemented**: `kernel/src/api/` (mod.rs, http.rs, json.rs, tools.rs)

### 9.1 Two Modes

| Mode      | Path                   | Use Case                    |
| --------- | ---------------------- | --------------------------- |
| **TLS**   | Direct HTTPS :443      | Production (auto DNS)       |
| **Proxy** | Plain HTTP :8080       | Debug (socat on host)       |

### 9.2 SSE Streaming Parser

The API uses Server-Sent Events. The parser handles:

- `content_block_start` (type: `text` or `tool_use`) — captures tool id/name
- `content_block_delta` (type: `text_delta` or `input_json_delta`) —
  streams text to COM1, accumulates tool input JSON
- `content_block_stop` — finalizes tool call
- `message_delta` — captures `stop_reason` (`end_turn` or `tool_use`)
- `message_stop` — signals end of response

### 9.3 JSON Parser

**Implemented**: `kernel/src/api/json.rs`

Recursive descent, no_std, no external deps. Handles:
- Full JSON spec (null, bool, number, string, array, object)
- UTF-16 surrogate pairs in `\uXXXX` escapes
- Multi-byte UTF-8 passthrough (2/3/4-byte sequences)
- Custom `parse_f64` for no_std environments

### 9.4 Retry Logic

- 3 retries with exponential backoff (1s, 2s, 4s)
- Honors `Retry-After` header on 429 (capped at 60s)
- Retries on 429/500/529; fails immediately on 401/403/404
- CRLF injection prevention on api_key and model strings

---

## 10. Agentic Loop

**Implemented**: `kernel/src/shell/agent.rs` + `kernel/src/api/tools.rs`

### 10.1 Architecture

```
COM1 terminal (operator)
    | "agent refactore l'indexer"
    v
Shell dispatch -> run_agent_loop()
    |
    v
loop (max 20 turns):
    |
    +-- Build HTTP request with tools + conversation history
    +-- TLS handshake -> api.anthropic.com:443
    +-- Send Messages API request (stream=true, tools=[...])
    +-- Parse SSE stream:
    |     text_delta -> print to COM1
    |     tool_use   -> accumulate input JSON
    |     message_stop -> check stop_reason
    |
    +-- If stop_reason == "end_turn": print final text, return
    +-- If stop_reason == "tool_use":
          +-- For each tool call:
          |     dispatch_tool(name, input_json)
          |       -> read_file:   SELECT content FROM namespace
          |       -> write_file:  INSERT OR REPLACE INTO namespace
          |       -> sql_query:   exec_and_format (read-only)
          |       -> list_dir:    SELECT path FROM namespace WHERE substr(...)
          |       -> str_replace: read + replacen + UPDATE
          +-- Append assistant message (tool_use blocks) to history
          +-- Append user message (tool_result blocks) to history
          +-- Continue loop
```

### 10.2 Tool Definitions

5 tools exposed to Claude with JSON Schema:

| Tool          | Description                                    |
| ------------- | ---------------------------------------------- |
| `read_file`   | Read file from namespace (path -> content)     |
| `write_file`  | Write/create file in namespace                 |
| `sql_query`   | Read-only SQL (SELECT/EXPLAIN/PRAGMA only)     |
| `list_dir`    | List namespace entries under a prefix           |
| `str_replace` | Find-and-replace in a file (first occurrence)  |

### 10.3 Security

- SQL queries from tools are restricted to SELECT/EXPLAIN/PRAGMA
  (case-insensitive check)
- `ls()` uses `substr()` instead of `LIKE` to prevent wildcard injection
- Tool results are truncated in display (200 chars) but sent in full to API
- Rate limiting on Lua `ask()`: 10s minimum between calls
- max_tokens set to 4096 for tool-use requests

### 10.4 Conversation Format

Messages use the Anthropic Messages API format with structured content blocks:

```json
// Assistant message with tool calls:
{"role": "assistant", "content": [
  {"type": "text", "text": "Let me read that file."},
  {"type": "tool_use", "id": "toolu_01...", "name": "read_file",
   "input": {"path": "/agents/indexer"}}
]}

// User message with tool results:
{"role": "user", "content": [
  {"type": "tool_result", "tool_use_id": "toolu_01...",
   "content": "local db = sql(...)"}
]}
```

---

## 11. Lua Agent Runtime

**Implemented**: `kernel/src/lua/` (mod.rs, ffi.rs, alloc.rs, builtins.rs, repl.rs)

### 11.1 Design Change from v1.0

The original plan called for WASM sandboxes (wasmi). The actual
implementation uses **Lua 5.5.0** compiled to bare metal:

| Aspect         | WASM (planned)           | Lua (actual)               |
| -------------- | ------------------------ | -------------------------- |
| Isolation      | Linear memory sandbox    | Memory-limited allocator   |
| Language       | Any -> WASM              | Lua only                   |
| Performance    | Interpreter (wasmi)      | Interpreter (Lua VM)       |
| Integration    | Host function ABI        | Direct FFI to kernel       |
| Complexity     | ~months of work          | ~days of work              |

The tradeoff is acceptable: Lua agents are not sandboxed against malicious
code (they run in kernel space), but they are resource-limited and have
read-only SQL access.

### 11.2 Builtins

| Function                | Description                              |
| ----------------------- | ---------------------------------------- |
| `sql(query, ...)`       | Execute SQL (read-only for agents)       |
| `read(path)`            | Read file from namespace                 |
| `write(path, data)`     | Write file to namespace                  |
| `ls(path)`              | List namespace entries                   |
| `log(msg)`              | Print to serial console                  |
| `sleep(ms)`             | TSC-based delay (max 60s)                |
| `now()`                 | Monotonic timestamp (ms)                 |
| `audit(level, action)`  | Write to audit table                     |
| `ask(prompt)` / `ask(table)` | Call Claude API (10s rate limit)    |

### 11.3 Execution Model

- **Agent mode** (`run /agents/foo`): Load Lua from namespace table, execute
  with 30s timeout, read-only SQL, audit logging
- **REPL mode** (`lua`): Interactive, no timeout, full SQL access
- **Memory limit**: Configurable per agent (default 16 MiB)
- **GC**: Incremental mode (pause=100, stepmul=200, stepsize=10)

---

## 12. Shell Interface

**Implemented**: `kernel/src/shell/` (mod.rs, commands.rs, agent.rs, line.rs)

```
heaven% help
HeavenOS shell commands:

  help          show this help
  mem           physical memory info
  nvme          NVMe controller info
  net           network interface info
  cpu           CPU features
  uptime        system uptime
  ls [path]     list namespace entries
  cat <path>    read a namespace file
  echo <text>   print text
  sql <stmt>    execute SQL on the system database

Lua:
  lua             interactive Lua REPL
  run <path>      execute a Lua agent from namespace
  store <p> <c>   store Lua script at path

Claude API:
  apikey <key>     set Anthropic API key
  resolve <ip>     set api.anthropic.com IP (override DNS)
  ask <prompt>     send message via TLS (auto-resolves DNS)
  askp <prompt>    send message via proxy (plain HTTP)
  agent <prompt>   agentic loop with tool use (read/write/sql)
  agentp <prompt>  agentic loop via proxy
  model <name>     set model (default: claude-sonnet-4-6-20250514)
  pin [show|set]   manage TLS certificate SPKI pin
```

Line editor supports backspace, Ctrl-C (cancel), Ctrl-U (clear line).

---

## 13. Concurrency Model

### Current: Single-Threaded Cooperative

- One main loop: shell prompt -> command dispatch -> return
- SQLite is `THREADSAFE=0` -- no mutexes needed
- Network polling is synchronous (spin-loop during API calls)
- `xShmLock` always succeeds (single accessor)

### Future: Multi-Core

- One NVMe I/O queue pair per core
- SQLite access serialized through spinlock
- `THREADSAFE=1`, VFS locking becomes real
- Agents pinned to cores

---

## 14. Implementation Status

```
Phase 0: Boot + Memory                    [DONE]
  +-- Limine bootloader -> long mode
  +-- Physical page allocator (bitmap, 4KiB pages, up to 4 GiB)
  +-- DMA-safe allocator (clflushopt + mfence)
  +-- APIC timer + TSC calibration
  +-- GDT, PIC, IDT
  +-- Serial console (COM1)

Phase 1: NVMe Driver                      [DONE]
  +-- PCI enumeration (find NVMe by class 01:08)
  +-- Admin queue setup (Identify Controller/Namespace)
  +-- I/O queue pair (Create CQ + SQ)
  +-- Read/Write/Flush with PRP lists
  +-- Auto-chunking for >65535 blocks
  +-- 30s TSC-based timeout

Phase 2: SQLite VFS                        [DONE]
  +-- Block allocator (superblock + bitmap on disk)
  +-- File table (42 entries)
  +-- sqlite3.c compiled for bare metal (SQLITE_OS_OTHER=1)
  +-- libc shims (compiler_builtins + custom malloc)
  +-- VFS: xOpen, xClose, xDelete
  +-- VFS: xRead (non-aligned DMA), xWrite (RMW)
  +-- VFS: xSync (bitmap + file table + NVMe Flush)
  +-- VFS: xShmMap/Lock/Barrier/Unmap (RAM-backed)
  +-- VFS: xSleep (TSC), xCurrentTime, xRandomness (RDRAND)
  +-- Bootstrap (blank disk -> schema DDL)
  +-- [Block cache: omitted, SQLite page cache sufficient]

Phase 3: Namespace                         [DONE]
  +-- Styx 9P2000 message parser
  +-- SQLite namespace table (path, type, content, mtime)
  +-- SQLite audit table (agent, action, target)
  +-- Shell: ls, cat, sql, store
  +-- Lua builtins: read, write, ls, sql

Phase 4: Network + TLS + Claude API        [DONE]
  +-- virtio-net driver (DMA ring buffers)
  +-- smoltcp TCP/UDP/DHCP stack
  +-- DNS resolver (RFC 1035, 8-entry cache)
  +-- TLS 1.3 (embedded-tls, AES-128-GCM, P-256)
  +-- RDRAND-based CryptoRng
  +-- DER parser + SPKI pin infrastructure
  +-- HTTP request builder (Messages API)
  +-- SSE streaming parser (text_delta + tool_use)
  +-- JSON parser (recursive descent, no_std)
  +-- Retry with exponential backoff + Retry-After
  +-- ask/askp shell commands

Phase 5: Agents                            [DONE]
  +-- Lua 5.5.0 FFI integration
  +-- Memory-limited allocator (16 MiB default)
  +-- 9 builtins (sql, read, write, ls, log, sleep, now, audit, ask)
  +-- 30s execution timeout
  +-- Read-only SQL enforcement for agents
  +-- Interactive REPL (full SQL access)
  +-- Agentic loop (agent/agentp commands)
  +-- 5 Claude tools (read_file, write_file, sql_query, list_dir, str_replace)
  +-- Multi-turn conversation with tool_result
  +-- Up to 20 agentic turns

Phase 6: GPU Driver                        [NOT STARTED]
  +-- PCI enumeration for GPU
  +-- BAR0 mapping
  +-- Command buffer / queue submission
  +-- DMA: RAM <-> VRAM
  +-- Inference driver skeleton
```

---

## 15. What This Design Does NOT Cover (Intentionally)

- **Local GPU inference**: The AI path goes through the Anthropic API,
  not local compute. GPU support is deferred to Phase 6.
- **WASM sandbox**: Replaced by Lua 5.5 for agent scripting. WASM may be
  revisited if stronger isolation is needed.
- **Display / framebuffer**: Not needed for a headless system.
- **USB / keyboard**: Serial console suffices.
- **Multi-user security**: Single-operator system.
- **Config persistence**: API key and model are in-memory (lost on reboot).
  Phase 6 should persist them in the namespace table.
- **Certificate pinning enforcement**: Infrastructure ready, blocked by
  embedded-tls 0.18 `pub(crate)` limitation.

---

## 16. Risks and Open Questions

| Risk                               | Status / Mitigation                             |
| ---------------------------------- | ----------------------------------------------- |
| SQLite C code has hidden libc deps | **Resolved**: All stubs in place, builds clean  |
| NVMe controller quirks             | Tested on QEMU virtio-blk; real hardware TBD    |
| Block allocator fragmentation      | Monitor; add compaction if needed                |
| TLS without cert pinning           | SPKI infra ready; waiting on embedded-tls update |
| API key in memory (not persisted)  | Acceptable for Phase 5; persist in Phase 6       |
| Lua agents not sandboxed           | Run in kernel space; resource-limited only       |
| No rate limit on agent command     | 20-turn cap; Lua ask() has 10s rate limit        |
| UTF-8 in API responses             | **Fixed**: JSON parser handles multi-byte UTF-8  |
| Model string staleness             | **Fixed**: Updated to claude-sonnet-4-6-20250514 |

---

## 17. Code Map

```
kernel/src/
+-- lib.rs                  Module declarations
+-- main.rs                 Boot sequence + shell loop
+-- arch/x86_64/            GDT, PIC, IDT, timer, serial, CPU features
+-- mem/                    Physical page allocator, DMA allocator, heap
+-- drivers/
|   +-- nvme/               NVMe driver (PCI, queues, commands)
|   +-- virtio/             virtio-net NIC driver
+-- storage/                Block allocator, file table (on-disk layout)
+-- vfs/                    SQLite VFS bridge (xRead, xWrite, xSync, xShm*)
+-- sqlite/                 SQLite FFI, DB wrapper, VFS registration
+-- fs/styx/                9P2000 message parser, namespace server
+-- net/                    smoltcp stack, DNS resolver
+-- crypto/                 RDRAND RNG, DER parser, SPKI pin verifier
+-- api/
|   +-- mod.rs              HTTP client, SSE parser, retry logic
|   +-- http.rs             HTTP response parser
|   +-- json.rs             Recursive descent JSON parser
|   +-- tools.rs            Tool definitions (5 tools with JSON Schema)
+-- lua/
|   +-- mod.rs              Lua 5.5 execution engine
|   +-- ffi.rs              Raw C FFI bindings
|   +-- alloc.rs            Memory-limited allocator
|   +-- builtins.rs         9 builtin functions
|   +-- repl.rs             Interactive REPL
+-- shell/
    +-- mod.rs              Shell loop
    +-- commands.rs         Built-in command dispatch
    +-- agent.rs            Agentic loop (tool dispatch, conversation)
    +-- line.rs             Line editor (backspace, Ctrl-C, Ctrl-U)
```

---

*v1.0 was the architecture. v2.0 is the implementation.*
