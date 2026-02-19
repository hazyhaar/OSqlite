/// Cryptographic primitives for bare-metal TLS.
///
/// Provides an RDRAND-based RNG that implements `rand_core::CryptoRng`.
/// RDRAND is a hardware random number generator available on Intel Ivy Bridge+
/// and AMD Zen+. We verified its presence via CPUID during boot.

/// RDRAND-based cryptographically secure RNG.
pub struct RdRandRng;

impl RdRandRng {
    pub fn new() -> Self {
        Self
    }

    /// Read a 64-bit random value via RDRAND.
    /// Retries up to 32 times (Intel recommends 10).
    fn rdrand64() -> Option<u64> {
        for _ in 0..32 {
            let val: u64;
            let ok: u8;
            unsafe {
                core::arch::asm!(
                    "rdrand {val}",
                    "setc {ok}",
                    val = out(reg) val,
                    ok = out(reg_byte) ok,
                    options(nostack, nomem),
                );
            }
            if ok != 0 {
                return Some(val);
            }
        }
        None
    }
}

impl rand_core::RngCore for RdRandRng {
    fn next_u32(&mut self) -> u32 {
        Self::rdrand64().unwrap_or(0) as u32
    }

    fn next_u64(&mut self) -> u64 {
        Self::rdrand64().unwrap_or(0)
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        let mut offset = 0;
        while offset < dest.len() {
            let val = Self::rdrand64().unwrap_or(0);
            let bytes = val.to_le_bytes();
            let remaining = dest.len() - offset;
            let copy_len = remaining.min(8);
            dest[offset..offset + copy_len].copy_from_slice(&bytes[..copy_len]);
            offset += copy_len;
        }
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        let mut offset = 0;
        while offset < dest.len() {
            let val = Self::rdrand64()
                .ok_or(rand_core::Error::from(
                    core::num::NonZeroU32::new(1).unwrap()
                ))?;
            let bytes = val.to_le_bytes();
            let remaining = dest.len() - offset;
            let copy_len = remaining.min(8);
            dest[offset..offset + copy_len].copy_from_slice(&bytes[..copy_len]);
            offset += copy_len;
        }
        Ok(())
    }
}

impl rand_core::CryptoRng for RdRandRng {}
