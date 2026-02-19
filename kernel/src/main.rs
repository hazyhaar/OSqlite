//! HeavenOS Kernel — entry point.
//!
//! This is where the kernel begins execution after the bootloader
//! has set up long mode, page tables, and jumped to _start.
#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate alloc;

use heavenos_kernel::arch::x86_64::{self, serial};
use heavenos_kernel::drivers::nvme;
use heavenos_kernel::fs::styx;
use heavenos_kernel::mem;
use heavenos_kernel::storage;
use heavenos_kernel::vfs;
use heavenos_kernel::serial_println;

use core::panic::PanicInfo;

/// Kernel entry point. Called by the bootloader after setting up
/// long mode and identity-mapping physical memory.
#[no_mangle]
pub extern "C" fn _start() -> ! {
    // 1. Initialize serial console for debug output
    serial::SERIAL.lock().init();
    serial_println!("HeavenOS v0.1.0 — booting...");

    // 2. Initialize GDT, PIC, and IDT (must be done before any exception can fire)
    unsafe { x86_64::gdt::init(); }
    serial_println!("[cpu] GDT loaded");
    unsafe { x86_64::pic::init(); }
    serial_println!("[cpu] PIC remapped (IRQs masked)");
    unsafe { x86_64::idt::init(); }
    serial_println!("[cpu] IDT loaded (exception handlers active)");

    // 3. Initialize physical memory allocator
    // TODO: parse memory map from bootloader
    // For now, hardcode a 128 MB region starting at 1 MB
    let memory_regions = [(0x100000u64, 128 * 1024 * 1024u64)];
    mem::phys::PHYS_ALLOCATOR.init(&memory_regions);
    serial_println!("[mem] Physical allocator: {} pages free",
        mem::phys::PHYS_ALLOCATOR.free_count());

    // 3. Mark kernel image as used
    // TODO: get actual kernel bounds from linker symbols
    // For now, mark first 2 MB as used (kernel + boot structures)
    mem::phys::PHYS_ALLOCATOR.mark_used(mem::PhysAddr::new(0x100000), 512);
    serial_println!("[mem] Kernel pages reserved");

    // 4. Check CPU features
    serial_println!("[cpu] RDRAND: {}", x86_64::cpu::has_rdrand());
    serial_println!("[cpu] CLFLUSHOPT: {}", x86_64::cpu::has_clflushopt());
    serial_println!("[cpu] Invariant TSC: {}", x86_64::cpu::has_invariant_tsc());

    // 5. Scan PCI for NVMe controller
    serial_println!("[pci] Scanning for NVMe controller...");
    match nvme::pci::find_nvme_controller() {
        Some(dev) => {
            serial_println!("[pci] Found NVMe: {:04x}:{:04x} at bus={} dev={} BAR0={:#x}",
                dev.vendor_id, dev.device_id, dev.bus, dev.device, dev.bar0);

            // 6. Initialize NVMe driver
            let bar0_ptr = dev.bar0 as *mut u8; // Identity mapped
            match unsafe { nvme::NvmeDriver::new(bar0_ptr) } {
                Ok(driver) => {
                    let ns = driver.namespace_info().unwrap();
                    serial_println!("[nvme] Namespace 1: {} blocks x {} bytes = {} MB",
                        ns.block_count, ns.block_size,
                        ns.block_count * ns.block_size as u64 / (1024 * 1024));

                    *nvme::NVME.lock() = Some(driver);

                    // 7. Initialize storage (block allocator + file table)
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

    // 8. Initialize Styx namespace
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
