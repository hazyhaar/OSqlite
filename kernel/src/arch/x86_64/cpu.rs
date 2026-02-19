/// CPU feature detection and management.

/// CPUID wrapper. Saves/restores rbx since LLVM reserves it.
pub fn cpuid(leaf: u32) -> (u32, u32, u32, u32) {
    let (eax, ebx, ecx, edx): (u32, u32, u32, u32);
    unsafe {
        core::arch::asm!(
            "push rbx",
            "cpuid",
            "mov {ebx_out:e}, ebx",
            "pop rbx",
            inout("eax") leaf => eax,
            ebx_out = out(reg) ebx,
            out("ecx") ecx,
            out("edx") edx,
            options(nostack),
        );
    }
    (eax, ebx, ecx, edx)
}

/// Check if RDRAND is supported (CPUID.01H:ECX.RDRAND[bit 30]).
pub fn has_rdrand() -> bool {
    let (_, _, ecx, _) = cpuid(1);
    ecx & (1 << 30) != 0
}

/// Check if CLFLUSHOPT is supported (CPUID.07H.0:EBX.CLFLUSHOPT[bit 23]).
pub fn has_clflushopt() -> bool {
    let (_, ebx, _, _) = cpuid_count(7, 0);
    ebx & (1 << 23) != 0
}

/// Check if TSC is invariant (CPUID.80000007H:EDX.TscInvariant[bit 8]).
pub fn has_invariant_tsc() -> bool {
    let (_, _, _, edx) = cpuid(0x80000007);
    edx & (1 << 8) != 0
}

/// Read the Time Stamp Counter.
#[inline(always)]
pub fn rdtsc() -> u64 {
    let (lo, hi): (u32, u32);
    unsafe {
        core::arch::asm!("rdtsc", out("eax") lo, out("edx") hi, options(nostack, preserves_flags));
    }
    ((hi as u64) << 32) | (lo as u64)
}

/// CPUID with subleaf (ECX input). Saves/restores rbx.
fn cpuid_count(leaf: u32, subleaf: u32) -> (u32, u32, u32, u32) {
    let (eax, ebx, ecx, edx): (u32, u32, u32, u32);
    unsafe {
        core::arch::asm!(
            "push rbx",
            "cpuid",
            "mov {ebx_out:e}, ebx",
            "pop rbx",
            inout("eax") leaf => eax,
            ebx_out = out(reg) ebx,
            inout("ecx") subleaf => ecx,
            out("edx") edx,
            options(nostack),
        );
    }
    (eax, ebx, ecx, edx)
}
