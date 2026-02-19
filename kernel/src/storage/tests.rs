/// Unit tests for storage subsystem — BlockAllocator bitmap logic, FileTable.
///
/// These tests exercise pure in-memory logic without any hardware I/O.
/// Run with: cargo test --target x86_64-unknown-linux-gnu --lib
use super::*;

// ---- BlockAllocator: pure bitmap logic ----

#[test]
fn alloc_single_block() {
    let mut alloc = BlockAllocator::new();
    alloc.init_for_test(1024, 4096, 10);

    let block = alloc.alloc(1).unwrap();
    assert_eq!(block, 0);
    assert_eq!(alloc.free_count(), 1023);
}

#[test]
fn alloc_multiple_blocks() {
    let mut alloc = BlockAllocator::new();
    alloc.init_for_test(1024, 4096, 10);

    let b1 = alloc.alloc(10).unwrap();
    assert_eq!(b1, 0);
    let b2 = alloc.alloc(5).unwrap();
    assert_eq!(b2, 10);
    assert_eq!(alloc.free_count(), 1024 - 15);
}

#[test]
fn alloc_free_reuse() {
    let mut alloc = BlockAllocator::new();
    alloc.init_for_test(100, 4096, 10);

    let b1 = alloc.alloc(10).unwrap();
    assert_eq!(b1, 0);
    alloc.free(0, 10);
    assert_eq!(alloc.free_count(), 100);

    // Should reuse the freed blocks
    let b2 = alloc.alloc(10).unwrap();
    assert_eq!(b2, 0);
}

#[test]
fn alloc_out_of_space() {
    let mut alloc = BlockAllocator::new();
    alloc.init_for_test(10, 4096, 10);

    let _ = alloc.alloc(10).unwrap();
    let err = alloc.alloc(1);
    assert!(err.is_err());
}

#[test]
fn alloc_contiguous_with_gap() {
    let mut alloc = BlockAllocator::new();
    alloc.init_for_test(20, 4096, 10);

    // Allocate blocks 0-4
    let _ = alloc.alloc(5).unwrap();
    // Allocate blocks 5-9
    let _ = alloc.alloc(5).unwrap();
    // Free blocks 0-4
    alloc.free(0, 5);
    // Need 10 contiguous: blocks 0-4 free (5), 5-9 used, 10-19 free (10).
    // Should find the run at 10-19.
    let b = alloc.alloc(10).unwrap();
    assert_eq!(b, 10);
}

#[test]
fn free_idempotent() {
    let mut alloc = BlockAllocator::new();
    alloc.init_for_test(10, 4096, 10);

    let _ = alloc.alloc(5).unwrap();
    assert_eq!(alloc.free_count(), 5);

    // Free same blocks twice — should not double-count
    alloc.free(0, 5);
    assert_eq!(alloc.free_count(), 10);
    alloc.free(0, 5);
    assert_eq!(alloc.free_count(), 10); // Still 10, not 15
}

#[test]
fn to_lba_conversion() {
    let mut alloc = BlockAllocator::new();
    alloc.init_for_test(1024, 4096, 10);

    assert_eq!(alloc.to_lba(0), 10);
    assert_eq!(alloc.to_lba(100), 110);
    assert_eq!(alloc.to_lba(1023), 1033);
}

#[test]
fn alloc_zero_blocks_error() {
    let mut alloc = BlockAllocator::new();
    alloc.init_for_test(100, 4096, 10);

    assert!(alloc.alloc(0).is_err());
}

#[test]
fn alloc_more_than_available() {
    let mut alloc = BlockAllocator::new();
    alloc.init_for_test(10, 4096, 10);

    assert!(alloc.alloc(11).is_err());
}

#[test]
fn alloc_exactly_all() {
    let mut alloc = BlockAllocator::new();
    alloc.init_for_test(64, 4096, 10);

    let b = alloc.alloc(64).unwrap();
    assert_eq!(b, 0);
    assert_eq!(alloc.free_count(), 0);
}

#[test]
fn alloc_across_word_boundary() {
    // Bitmap words are 64 bits. Test allocation that spans two words.
    let mut alloc = BlockAllocator::new();
    alloc.init_for_test(128, 4096, 10);

    // Fill first 60 blocks
    let _ = alloc.alloc(60).unwrap();
    // Allocate 10 blocks — spans from bit 60 of word 0 to bit 5 of word 1
    let b = alloc.alloc(10).unwrap();
    assert_eq!(b, 60);
    assert_eq!(alloc.free_count(), 128 - 70);
}

#[test]
fn alloc_after_fragmentation() {
    let mut alloc = BlockAllocator::new();
    alloc.init_for_test(100, 4096, 10);

    // Create a checkerboard pattern: alloc 2, free 1, alloc 2, free 1...
    let _ = alloc.alloc(2).unwrap(); // 0-1 used
    let _ = alloc.alloc(1).unwrap(); // 2 used
    alloc.free(1, 1);                // 1 free, 0+2 used

    // Now: 0 used, 1 free, 2 used, 3-99 free
    // Requesting 3 contiguous should start at block 3
    let b = alloc.alloc(3).unwrap();
    assert_eq!(b, 3);
}

// ---- FileEntry ----

#[test]
fn file_entry_name_handling() {
    let mut entry = FileEntry::empty();
    assert!(!entry.is_in_use());
    assert_eq!(entry.name_bytes(), b"");

    entry.set_name(b"test.db");
    assert_eq!(entry.name_bytes(), b"test.db");

    // Test truncation for long names
    let long_name = [b'x'; 100];
    entry.set_name(&long_name);
    assert_eq!(entry.name_bytes().len(), 63); // MAX_NAME_LEN - 1
}

#[test]
fn file_entry_flags() {
    let mut entry = FileEntry::empty();
    assert!(!entry.is_in_use());

    entry.set_in_use(true);
    assert!(entry.is_in_use());

    entry.set_in_use(false);
    assert!(!entry.is_in_use());
}

#[test]
fn file_entry_size() {
    // FileEntry must be exactly 96 bytes for on-disk compatibility
    assert_eq!(core::mem::size_of::<FileEntry>(), 96);
}

// ---- FileTable ----

#[test]
fn file_table_create_lookup() {
    let mut ft = FileTable::new(5, 4096);

    let idx = ft.create(b"main.db", 100, 16).unwrap();
    assert_eq!(idx, 0);

    let (found_idx, entry) = ft.lookup(b"main.db").unwrap();
    assert_eq!(found_idx, 0);
    assert_eq!(entry.start_block, 100);
    assert_eq!(entry.block_count, 16);
    assert_eq!(entry.byte_length, 0);
}

#[test]
fn file_table_create_multiple() {
    let mut ft = FileTable::new(5, 4096);

    ft.create(b"main.db", 0, 10).unwrap();
    ft.create(b"main.db-wal", 10, 10).unwrap();
    ft.create(b"main.db-shm", 20, 5).unwrap();

    assert!(ft.lookup(b"main.db").is_some());
    assert!(ft.lookup(b"main.db-wal").is_some());
    assert!(ft.lookup(b"main.db-shm").is_some());
    assert!(ft.lookup(b"nonexistent").is_none());
}

#[test]
fn file_table_delete() {
    let mut ft = FileTable::new(5, 4096);

    let idx = ft.create(b"temp.db", 50, 8).unwrap();
    assert!(ft.lookup(b"temp.db").is_some());

    let deleted = ft.delete(idx).unwrap();
    assert_eq!(deleted.start_block, 50);
    assert!(ft.lookup(b"temp.db").is_none());

    // Slot should be reusable
    let idx2 = ft.create(b"new.db", 60, 4).unwrap();
    assert_eq!(idx2, idx);
}

#[test]
fn file_table_get_mut() {
    let mut ft = FileTable::new(5, 4096);
    ft.create(b"main.db", 0, 10).unwrap();

    {
        let entry = ft.get_mut(0).unwrap();
        entry.byte_length = 8192;
    }

    let (_, entry) = ft.lookup(b"main.db").unwrap();
    assert_eq!(entry.byte_length, 8192);
}

#[test]
fn file_table_full() {
    let mut ft = FileTable::new(5, 4096);

    for i in 0..42u64 {
        let name = alloc::format!("file_{}", i);
        ft.create(name.as_bytes(), i * 10, 10).unwrap();
    }

    // 43rd should fail
    assert!(ft.create(b"overflow", 420, 10).is_none());
}

#[test]
fn file_table_delete_nonexistent() {
    let mut ft = FileTable::new(5, 4096);

    // Delete from empty table
    assert!(ft.delete(0).is_none());
    assert!(ft.delete(99).is_none());
}

#[test]
fn file_table_lookup_after_delete() {
    let mut ft = FileTable::new(5, 4096);

    ft.create(b"a.db", 0, 5).unwrap();
    ft.create(b"b.db", 5, 5).unwrap();
    ft.create(b"c.db", 10, 5).unwrap();

    ft.delete(1); // delete b.db

    assert!(ft.lookup(b"a.db").is_some());
    assert!(ft.lookup(b"b.db").is_none());
    assert!(ft.lookup(b"c.db").is_some());

    // New file should take slot 1
    let idx = ft.create(b"d.db", 15, 3).unwrap();
    assert_eq!(idx, 1);
}
