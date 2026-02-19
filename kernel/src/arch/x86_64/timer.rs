/// Timer subsystem — TSC calibration and monotonic clock.
///
/// Uses PIT Channel 2 (speaker gate) to measure TSC frequency without
/// requiring interrupts. This is the standard "gate calibration" method:
///   1. Program PIT channel 2 for a known delay (~10ms one-shot)
///   2. Read TSC before and after the PIT counts down
///   3. Compute TSC frequency = delta_tsc / known_delay
///
/// After calibration, `monotonic_ms()` converts TSC ticks to milliseconds.
use core::sync::atomic::{AtomicU64, Ordering};
use super::{outb, inb};
use super::cpu::rdtsc;

/// TSC frequency in Hz, set once during calibration.
static TSC_FREQ_HZ: AtomicU64 = AtomicU64::new(0);

/// TSC ticks per millisecond (TSC_FREQ_HZ / 1000), for fast division.
static TSC_PER_MS: AtomicU64 = AtomicU64::new(2_000_000); // default: 2 GHz fallback

/// TSC value at boot (set right after calibration).
static BOOT_TSC: AtomicU64 = AtomicU64::new(0);

// PIT ports
const PIT_CH2_DATA: u16 = 0x42;
const PIT_CMD: u16 = 0x43;
const PIT_GATE: u16 = 0x61;  // NMI Status and Control Register (speaker gate)

/// PIT oscillator frequency: 1,193,182 Hz (standard PC).
const PIT_FREQ: u64 = 1_193_182;

/// Calibrate the TSC using PIT channel 2 in one-shot mode.
///
/// Uses the speaker gate (port 0x61) to control PIT channel 2 without
/// needing interrupts. The gate bit starts the countdown; we spin until
/// the output bit goes high (countdown complete).
///
/// # Safety
/// Must be called during boot, with interrupts disabled.
pub fn calibrate_tsc() {
    // Target: ~10ms calibration window.
    // PIT counter value for 10ms: 1_193_182 * 0.010 = 11_932
    let pit_count: u16 = 11_932;  // ~10.0006 ms
    let expected_us: u64 = (pit_count as u64 * 1_000_000) / PIT_FREQ;

    // 1. Disable speaker and PIT channel 2 gate
    let gate = inb(PIT_GATE);
    outb(PIT_GATE, (gate & !0x02) | 0x01); // gate=0, speaker off, bit0=1 enables gate control

    // 2. Program PIT channel 2 for mode 0 (one-shot), binary, LSB then MSB
    outb(PIT_CMD, 0xB0); // channel 2, mode 0, lobyte/hibyte, binary

    // 3. Load the counter value
    outb(PIT_CH2_DATA, (pit_count & 0xFF) as u8);
    outb(PIT_CH2_DATA, ((pit_count >> 8) & 0xFF) as u8);

    // 4. Start the countdown by enabling the gate (bit 0 of port 0x61)
    let gate = inb(PIT_GATE);
    outb(PIT_GATE, gate & !0x01); // clear gate
    outb(PIT_GATE, gate | 0x01);  // set gate — starts counting

    // 5. Read TSC at start
    let tsc_start = rdtsc();

    // 6. Wait for PIT output to go high (bit 5 of port 0x61)
    loop {
        if inb(PIT_GATE) & 0x20 != 0 {
            break;
        }
        core::hint::spin_loop();
    }

    // 7. Read TSC at end
    let tsc_end = rdtsc();

    // 8. Compute TSC frequency
    let delta = tsc_end - tsc_start;
    let freq_hz = (delta * 1_000_000) / expected_us;
    let per_ms = freq_hz / 1000;

    TSC_FREQ_HZ.store(freq_hz, Ordering::Release);
    TSC_PER_MS.store(per_ms, Ordering::Release);
    BOOT_TSC.store(tsc_end, Ordering::Release);
}

/// Get the calibrated TSC frequency in Hz.
pub fn tsc_freq_hz() -> u64 {
    TSC_FREQ_HZ.load(Ordering::Acquire)
}

/// Get the calibrated TSC ticks per millisecond.
pub fn tsc_per_ms() -> u64 {
    TSC_PER_MS.load(Ordering::Acquire)
}

/// Milliseconds since boot. Uses calibrated TSC.
pub fn monotonic_ms() -> u64 {
    let boot = BOOT_TSC.load(Ordering::Acquire);
    let now = rdtsc();
    let per_ms = TSC_PER_MS.load(Ordering::Acquire);
    if per_ms == 0 {
        return 0;
    }
    (now - boot) / per_ms
}

/// Seconds since boot.
pub fn uptime_secs() -> u64 {
    monotonic_ms() / 1000
}

/// Busy-wait for the specified number of microseconds using calibrated TSC.
pub fn delay_us(us: u64) {
    let per_ms = TSC_PER_MS.load(Ordering::Acquire);
    // per_ms = ticks/ms, so ticks/us = per_ms/1000
    let target_ticks = us * per_ms / 1000;
    let start = rdtsc();
    while rdtsc() - start < target_ticks {
        core::hint::spin_loop();
    }
}
