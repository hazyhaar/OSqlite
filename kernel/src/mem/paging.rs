/// Page table manipulation for x86_64 4-level paging.
///
/// Limine sets up the initial page tables (HHDM + higher-half kernel).
/// We walk those tables to unmap individual pages (e.g., guard pages).
///
/// We access page table entries via the HHDM: since all physical memory
/// is mapped at virt = phys + hhdm_offset, we can simply convert the
/// physical addresses in PTEs to virtual pointers.
use super::phys::{hhdm_offset, PAGE_SIZE, PHYS_ALLOCATOR};

const ENTRIES_PER_TABLE: usize = 512;

/// Page table entry flags.
const PTE_PRESENT: u64 = 1 << 0;
const PTE_WRITABLE: u64 = 1 << 1;
const PTE_ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000; // bits 51:12

/// Read CR3 (PML4 physical base address).
fn read_cr3() -> u64 {
    let cr3: u64;
    unsafe { core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nostack, nomem)); }
    cr3 & PTE_ADDR_MASK
}

/// Flush a single TLB entry for a virtual address.
fn invlpg(vaddr: u64) {
    unsafe { core::arch::asm!("invlpg [{}]", in(reg) vaddr, options(nostack, preserves_flags)); }
}

/// Extract page table index for a given level from a virtual address.
/// Level 4 = PML4, Level 3 = PDPT, Level 2 = PD, Level 1 = PT
fn table_index(vaddr: u64, level: u8) -> usize {
    let shift = 12 + 9 * (level as u64 - 1);
    ((vaddr >> shift) & 0x1FF) as usize
}

/// Convert a physical page table address to a virtual pointer via HHDM.
fn phys_to_virt(phys: u64) -> *mut u64 {
    (phys + hhdm_offset()) as *mut u64
}

/// Unmap a single 4 KiB page by clearing the Present bit in the PT entry.
///
/// Returns `true` if the page was mapped and is now unmapped.
/// Returns `false` if the page was not mapped or intermediate tables are missing.
///
/// # Safety
/// The caller must ensure that unmapping this page is safe — no code or data
/// should be actively accessed through it.
pub unsafe fn unmap_page(vaddr: u64) -> bool {
    let pml4_phys = read_cr3();
    let pml4 = phys_to_virt(pml4_phys);

    // Walk PML4 → PDPT → PD → PT
    let levels = [4u8, 3, 2];
    let mut table = pml4;

    for &level in &levels {
        let idx = table_index(vaddr, level);
        let entry = table.add(idx).read_volatile();

        if entry & PTE_PRESENT == 0 {
            return false; // Intermediate table not present
        }

        let next_phys = entry & PTE_ADDR_MASK;
        table = phys_to_virt(next_phys);
    }

    // Now `table` points to the PT (level 1 table)
    let pt_idx = table_index(vaddr, 1);
    let pte_ptr = table.add(pt_idx);
    let pte = pte_ptr.read_volatile();

    if pte & PTE_PRESENT == 0 {
        return false; // Already unmapped
    }

    // Clear the present bit
    pte_ptr.write_volatile(pte & !PTE_PRESENT);
    invlpg(vaddr);

    true
}

/// Allocate a kernel stack with a guard page at the bottom.
///
/// Layout (low address first):
///   [guard page] — 1 page, unmapped (not present)
///   [usable stack] — `stack_pages` pages, mapped read/write
///
/// Returns `(guard_vaddr, stack_top_vaddr)` or `None` if allocation fails.
///
/// The stack grows downward, so the stack pointer starts at `stack_top_vaddr`.
///
/// # Safety
/// Must be called after the physical allocator is initialized.
pub unsafe fn alloc_guarded_stack(stack_pages: usize) -> Option<(u64, u64)> {
    // Allocate (1 guard + stack_pages) contiguous pages
    let total_pages = 1 + stack_pages;
    let phys = PHYS_ALLOCATOR.alloc_pages_contiguous(total_pages, 1).ok()?;
    let base_virt = phys.as_u64() + hhdm_offset();

    // The guard page is at the base (lowest address)
    let guard_vaddr = base_virt;
    // Usable stack starts one page above
    let stack_bottom = base_virt + PAGE_SIZE as u64;
    let stack_top = stack_bottom + (stack_pages as u64) * PAGE_SIZE as u64;

    // Unmap the guard page so any access triggers a page fault
    unmap_page(guard_vaddr);

    Some((guard_vaddr, stack_top))
}
