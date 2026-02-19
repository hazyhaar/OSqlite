//! HeavenOS Kernel — entry point.
//!
//! Booted by the Limine bootloader. Limine sets up long mode, page tables
//! (kernel in upper 2 GiB + HHDM for all physical memory), and jumps to kmain.
#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate alloc;

use limine::BaseRevision;
use limine::memory_map::EntryType;
use limine::request::{
    HhdmRequest, MemoryMapRequest,
    RequestsEndMarker, RequestsStartMarker,
};

use heavenos_kernel::arch::x86_64::{self, serial};
use heavenos_kernel::drivers::nvme;
use heavenos_kernel::fs::styx;
use heavenos_kernel::mem;
use heavenos_kernel::storage;
use heavenos_kernel::vfs;
use heavenos_kernel::serial_println;

use core::panic::PanicInfo;

// ---- Limine requests ----
// Must be #[used] and in .requests section for Limine to discover them.
// Must also be referenced in kmain so the linker doesn't drop them.

#[used]
#[link_section = ".requests"]
static BASE_REVISION: BaseRevision = BaseRevision::new();

#[used]
#[link_section = ".requests"]
static HHDM_REQUEST: HhdmRequest = HhdmRequest::new();

#[used]
#[link_section = ".requests"]
static MEMMAP_REQUEST: MemoryMapRequest = MemoryMapRequest::new();

#[used]
#[link_section = ".requests_start_marker"]
static _START_MARKER: RequestsStartMarker = RequestsStartMarker::new();

#[used]
#[link_section = ".requests_end_marker"]
static _END_MARKER: RequestsEndMarker = RequestsEndMarker::new();

/// Kernel entry point — called by Limine after setting up long mode,
/// page tables (HHDM + kernel higher-half), and a stack.
#[no_mangle]
pub extern "C" fn kmain() -> ! {
    // 1. Initialize serial console for debug output (before anything else)
    serial::SERIAL.lock().init();
    serial_println!("HeavenOS v0.1.0 — booting...");

    // 2. Verify Limine boot protocol
    assert!(BASE_REVISION.is_supported(), "Limine base revision not supported");
    serial_println!("[boot] Limine protocol OK");

    // 3. Get HHDM offset from Limine — all PhysAddr::as_ptr() calls use this
    let hhdm_response = HHDM_REQUEST.get_response()
        .expect("Limine HHDM response missing");
    let hhdm_offset = hhdm_response.offset();
    mem::set_hhdm_offset(hhdm_offset);
    serial_println!("[boot] HHDM offset: {:#x}", hhdm_offset);

    // 4. Initialize GDT, PIC, and IDT (must be done before any exception can fire)
    unsafe { x86_64::gdt::init(); }
    serial_println!("[cpu] GDT loaded");
    unsafe { x86_64::pic::init(); }
    serial_println!("[cpu] PIC remapped (IRQs masked)");
    unsafe { x86_64::idt::init(); }
    serial_println!("[cpu] IDT loaded (exception handlers active)");

    // 5. Initialize physical memory allocator from Limine memory map
    let memmap_response = MEMMAP_REQUEST.get_response()
        .expect("Limine memory map response missing");

    let mut usable_regions = [(0u64, 0u64); 64];
    let mut region_count = 0usize;
    let mut total_usable: u64 = 0;

    for entry in memmap_response.entries() {
        if entry.entry_type == EntryType::USABLE {
            if region_count < usable_regions.len() {
                usable_regions[region_count] = (entry.base, entry.length);
                region_count += 1;
                total_usable += entry.length;
            }
        }
    }

    serial_println!("[mem] {} usable regions, {} MiB total",
        region_count, total_usable / (1024 * 1024));

    mem::phys::PHYS_ALLOCATOR.init(&usable_regions[..region_count]);
    serial_println!("[mem] Physical allocator: {} pages free",
        mem::phys::PHYS_ALLOCATOR.free_count());

    // 6. Check CPU features
    serial_println!("[cpu] RDRAND: {}", x86_64::cpu::has_rdrand());
    serial_println!("[cpu] CLFLUSHOPT: {}", x86_64::cpu::has_clflushopt());
    serial_println!("[cpu] Invariant TSC: {}", x86_64::cpu::has_invariant_tsc());

    // 7. Scan PCI for NVMe controller
    serial_println!("[pci] Scanning for NVMe controller...");
    match nvme::pci::find_nvme_controller() {
        Some(dev) => {
            serial_println!("[pci] Found NVMe: {:04x}:{:04x} at bus={} dev={} BAR0={:#x}",
                dev.vendor_id, dev.device_id, dev.bus, dev.device, dev.bar0);

            // 8. Initialize NVMe driver — BAR0 accessed via HHDM
            let bar0_ptr = mem::PhysAddr::new(dev.bar0).as_ptr::<u8>();
            match unsafe { nvme::NvmeDriver::new(bar0_ptr) } {
                Ok(driver) => {
                    let ns = driver.namespace_info().unwrap();
                    serial_println!("[nvme] Namespace 1: {} blocks x {} bytes = {} MB",
                        ns.block_count, ns.block_size,
                        ns.block_count * ns.block_size as u64 / (1024 * 1024));

                    *nvme::NVME.lock() = Some(driver);

                    // 9. Initialize storage (block allocator + file table)
                    init_storage();
                }
                Err(e) => {
                    serial_println!("[nvme] Init failed: {}", e);
                }
            }
        }
        None => {
            serial_println!("[pci] No NVMe controller found");
        }
    }

    // 10. Initialize Styx namespace
    let root = styx::namespace::build_root();
    let _server = styx::StyxServer::new(root);
    serial_println!("[styx] Namespace ready");

    serial_println!("HeavenOS boot complete.");

    // Drop into interactive shell over serial console
    heavenos_kernel::shell::run();
}

/// Initialize the storage subsystem — format or load from disk.
fn init_storage() {
    let mut nvme_guard = nvme::NVME.lock();
    let nvme = match nvme_guard.as_mut() {
        Some(n) => n,
        None => return,
    };

    let ns = nvme.namespace_info().unwrap().clone();

    // Try to load existing block allocator
    match storage::BlockAllocator::load(nvme) {
        Ok(alloc) => {
            serial_println!("[storage] Loaded existing filesystem: {} free blocks",
                alloc.free_count());

            let sb_block_size = alloc.block_size();
            let ft_lba = alloc.data_start_lba() - 1; // file table is right before data

            match storage::FileTable::load(nvme, ft_lba, sb_block_size) {
                Ok(ft) => {
                    serial_println!("[storage] File table loaded");
                    let _vfs = vfs::HeavenVfs::new(alloc, ft);
                    serial_println!("[vfs] SQLite VFS ready");
                }
                Err(e) => {
                    serial_println!("[storage] Failed to load file table: {}", e);
                }
            }
        }
        Err(_) => {
            // Blank disk — format
            serial_println!("[storage] No filesystem found, formatting...");
            match storage::BlockAllocator::format(nvme, ns.block_count, ns.block_size) {
                Ok(alloc) => {
                    serial_println!("[storage] Formatted: {} data blocks available",
                        alloc.free_count());

                    let sb_block_size = alloc.block_size();
                    let ft_lba = alloc.data_start_lba() - 1;

                    let ft = storage::FileTable::new(ft_lba, sb_block_size);
                    let _vfs = vfs::HeavenVfs::new(alloc, ft);
                    serial_println!("[vfs] SQLite VFS ready (fresh format)");
                }
                Err(e) => {
                    serial_println!("[storage] Format failed: {}", e);
                }
            }
        }
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    serial_println!("!!! KERNEL PANIC !!!");
    serial_println!("{}", info);
    loop {
        x86_64::hlt();
    }
}
