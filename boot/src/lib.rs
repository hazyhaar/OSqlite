#![no_std]
// Boot crate — placeholder for UEFI bootloader.
// The actual bootloader will:
// 1. Get memory map from UEFI
// 2. Set up page tables (identity map + higher half)
// 3. Jump to kernel _start with memory map info
