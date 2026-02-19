/// Kernel heap allocator — slab-based.
///
/// Provides `malloc`/`free`/`realloc` semantics needed by SQLite (via
/// `SQLITE_CONFIG_MALLOC`) and by Rust's `alloc` crate.
///
/// Design:
/// - Fixed-size slab classes: 8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096 bytes
/// - Large allocations (> 4096) go directly to the page allocator
/// - Each allocation has a hidden header storing the slab class (or size for large allocs)
///   so that `free(ptr)` works without a size argument — required by SQLite's xFree.
use core::alloc::{GlobalAlloc, Layout};
use core::ptr;
use spin::Mutex;

use super::phys::{PhysAddr, PAGE_SIZE, PHYS_ALLOCATOR};

/// Allocation header, stored immediately before the returned pointer.
#[repr(C)]
struct AllocHeader {
    /// Actual usable size of this allocation.
    size: usize,
    /// Slab class index (0-9) or LARGE_ALLOC for page-backed allocations.
    class: u8,
}

const HEADER_SIZE: usize = 16; // Aligned to 16 bytes
const LARGE_ALLOC: u8 = 0xFF;

const SLAB_CLASSES: [usize; 10] = [8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096];

/// Per-class free list.
struct FreeList {
    head: *mut FreeNode,
    slab_size: usize,
}

struct FreeNode {
    next: *mut FreeNode,
}

pub struct SlabAllocator {
    inner: Mutex<SlabInner>,
}

struct SlabInner {
    free_lists: [FreeList; 10],
    initialized: bool,
}

unsafe impl Send for SlabInner {}
unsafe impl Sync for SlabAllocator {}

impl SlabAllocator {
    pub const fn new() -> Self {
        const EMPTY_LIST: FreeList = FreeList {
            head: ptr::null_mut(),
            slab_size: 0,
        };

        Self {
            inner: Mutex::new(SlabInner {
                free_lists: [EMPTY_LIST; 10],
                initialized: false,
            }),
        }
    }

    fn ensure_init(inner: &mut SlabInner) {
        if !inner.initialized {
            for (i, &size) in SLAB_CLASSES.iter().enumerate() {
                inner.free_lists[i].slab_size = size;
            }
            inner.initialized = true;
        }
    }

    /// Find the slab class for a given size.
    fn class_for_size(size: usize) -> Option<usize> {
        SLAB_CLASSES.iter().position(|&s| s >= size)
    }

    /// Refill a slab class by allocating a page and splitting it.
    fn refill_class(inner: &mut SlabInner, class: usize) -> bool {
        let entry_size = SLAB_CLASSES[class] + HEADER_SIZE;
        let entries_per_page = PAGE_SIZE / entry_size;

        if entries_per_page == 0 {
            return false;
        }

        let phys = match PHYS_ALLOCATOR.alloc_page() {
            Ok(p) => p,
            Err(_) => return false,
        };

        let base = phys.as_ptr::<u8>();
        let list = &mut inner.free_lists[class];

        for i in 0..entries_per_page {
            let ptr = unsafe { base.add(i * entry_size) };

            // Write header
            let header = ptr as *mut AllocHeader;
            unsafe {
                (*header).size = SLAB_CLASSES[class];
                (*header).class = class as u8;
            }

            // The usable pointer is after the header
            let usable = unsafe { ptr.add(HEADER_SIZE) };
            let node = usable as *mut FreeNode;
            unsafe {
                (*node).next = list.head;
            }
            list.head = node;
        }

        true
    }
}

unsafe impl GlobalAlloc for SlabAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let size = layout.size().max(layout.align());
        let mut inner = self.inner.lock();
        SlabAllocator::ensure_init(&mut inner);

        match SlabAllocator::class_for_size(size) {
            Some(class) => {
                // Slab allocation
                let list = &mut inner.free_lists[class];

                if list.head.is_null() {
                    if !SlabAllocator::refill_class(&mut inner, class) {
                        return ptr::null_mut();
                    }
                }

                let list = &mut inner.free_lists[class];
                let node = list.head;
                if node.is_null() {
                    return ptr::null_mut();
                }

                list.head = unsafe { (*node).next };
                node as *mut u8
            }
            None => {
                // Large allocation: use pages directly
                let total = size + HEADER_SIZE;
                let pages = (total + PAGE_SIZE - 1) / PAGE_SIZE;

                let phys = match PHYS_ALLOCATOR.alloc_pages_contiguous(pages, 1) {
                    Ok(p) => p,
                    Err(_) => return ptr::null_mut(),
                };

                let base = phys.as_ptr::<u8>();
                let header = base as *mut AllocHeader;
                unsafe {
                    (*header).size = pages * PAGE_SIZE - HEADER_SIZE;
                    (*header).class = LARGE_ALLOC;
                }

                unsafe { base.add(HEADER_SIZE) }
            }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        if ptr.is_null() {
            return;
        }

        let header_ptr = unsafe { ptr.sub(HEADER_SIZE) } as *const AllocHeader;
        let header = unsafe { &*header_ptr };

        if header.class == LARGE_ALLOC {
            // Large allocation: free pages
            let total = header.size + HEADER_SIZE;
            let pages = (total + PAGE_SIZE - 1) / PAGE_SIZE;
            let phys = PhysAddr::new(header_ptr as u64);
            PHYS_ALLOCATOR.free_pages(phys, pages);
        } else {
            // Slab: return to free list
            let class = header.class as usize;
            let mut inner = self.inner.lock();

            let node = ptr as *mut FreeNode;
            let list = &mut inner.free_lists[class];
            unsafe {
                (*node).next = list.head;
            }
            list.head = node;
        }
    }
}

/// Global kernel heap allocator.
#[global_allocator]
pub static HEAP: SlabAllocator = SlabAllocator::new();

// --- C-compatible interface for SQLite ---

/// `malloc` for SQLite's SQLITE_CONFIG_MALLOC.
#[no_mangle]
pub extern "C" fn heavenos_malloc(size: usize) -> *mut u8 {
    if size == 0 {
        return ptr::null_mut();
    }
    let layout = match Layout::from_size_align(size, 8) {
        Ok(l) => l,
        Err(_) => return ptr::null_mut(),
    };
    unsafe { HEAP.alloc(layout) }
}

/// `free` for SQLite — no size argument needed (header stores it).
#[no_mangle]
pub extern "C" fn heavenos_free(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }
    // We need the layout for GlobalAlloc::dealloc, but our implementation
    // ignores it (uses the header instead). Pass a dummy layout.
    let layout = Layout::from_size_align(1, 1).unwrap();
    unsafe { HEAP.dealloc(ptr, layout) };
}

/// `realloc` for SQLite.
#[no_mangle]
pub extern "C" fn heavenos_realloc(ptr: *mut u8, new_size: usize) -> *mut u8 {
    if ptr.is_null() {
        return heavenos_malloc(new_size);
    }
    if new_size == 0 {
        heavenos_free(ptr);
        return ptr::null_mut();
    }

    // Read old size from header
    let header_ptr = unsafe { ptr.sub(HEADER_SIZE) } as *const AllocHeader;
    let old_size = unsafe { (*header_ptr).size };

    if new_size <= old_size {
        // Shrink: just return the same pointer (slab class hasn't changed)
        return ptr;
    }

    // Grow: allocate new, copy, free old
    let new_ptr = heavenos_malloc(new_size);
    if new_ptr.is_null() {
        return ptr::null_mut();
    }
    unsafe {
        ptr::copy_nonoverlapping(ptr, new_ptr, old_size.min(new_size));
    }
    heavenos_free(ptr);
    new_ptr
}

/// Return the usable size of an allocation (for sqlite3_msize).
#[no_mangle]
pub extern "C" fn heavenos_malloc_size(ptr: *mut u8) -> usize {
    if ptr.is_null() {
        return 0;
    }
    let header_ptr = unsafe { ptr.sub(HEADER_SIZE) } as *const AllocHeader;
    unsafe { (*header_ptr).size }
}
