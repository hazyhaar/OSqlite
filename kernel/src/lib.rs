#![no_std]
#![feature(abi_x86_interrupt)]
#![allow(dead_code)]

extern crate alloc;

// Hardware-dependent modules — only compiled for kernel target, not host-target tests
#[cfg(not(test))]
pub mod api;
#[cfg(not(test))]
pub mod arch;
#[cfg(not(test))]
pub mod crypto;
#[cfg(not(test))]
pub mod drivers;
#[cfg(not(test))]
pub mod fs;
#[cfg(not(test))]
pub mod mem;
#[cfg(not(test))]
pub mod net;
#[cfg(not(test))]
pub mod shell;
#[cfg(not(test))]
pub mod sqlite;
#[cfg(not(test))]
pub mod lua;
#[cfg(not(test))]
pub mod vfs;

// --- Test stubs for types referenced by the storage module ---
// When running `cargo test --target x86_64-unknown-linux-gnu`, we provide
// minimal stubs for NvmeError and DmaBuf so that storage code compiles
// without pulling in the entire NVMe driver or physical memory allocator.

#[cfg(test)]
pub mod drivers {
    pub mod nvme {
        /// Stub NvmeError for host-target tests.
        #[derive(Debug)]
        pub enum NvmeError {
            OutOfMemory,
            MediaError,
        }

        impl core::fmt::Display for NvmeError {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                match self {
                    NvmeError::OutOfMemory => write!(f, "out of memory (test stub)"),
                    NvmeError::MediaError => write!(f, "media error (test stub)"),
                }
            }
        }
    }
}

#[cfg(test)]
pub mod mem {
    use alloc::vec;

    /// Stub DMA buffer backed by a heap Vec for host-target tests.
    pub struct DmaBuf {
        data: alloc::vec::Vec<u8>,
    }

    /// Stub allocation error.
    #[derive(Debug)]
    pub struct AllocError;

    impl DmaBuf {
        pub fn alloc(size: usize) -> Result<Self, AllocError> {
            Ok(Self {
                data: vec![0u8; size],
            })
        }

        #[inline]
        pub fn as_ptr(&self) -> *const u8 {
            self.data.as_ptr()
        }

        #[inline]
        pub fn as_mut_ptr(&self) -> *mut u8 {
            self.data.as_ptr() as *mut u8
        }

        pub fn as_slice(&self) -> &[u8] {
            &self.data
        }

        pub fn as_mut_slice(&mut self) -> &mut [u8] {
            &mut self.data
        }

        pub fn copy_from_slice(&mut self, src: &[u8]) {
            self.data[..src.len()].copy_from_slice(src);
        }
    }
}

pub mod storage;
