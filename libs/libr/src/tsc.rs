use super::*;

/// P0 (benchmark): legge il Time Stamp Counter (cicli CPU). Disponibile in
/// ring 3: il boot non imposta mai CR4.TSD (solo PAE in `boot.asm`).
#[inline]
pub fn rdtsc() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// P0 (benchmark): calibra il TSC contro il PIT (~100 Hz). Misura i cicli
/// TSC trascorsi su `ticks` tick e ritorna gli Hz stimati (0 se fallisce).
/// Letture `get_ticks` spaziate da spin puri (mai busy-loop su syscall:
/// maschera IF=0 e affama il timer, vedi robustezza scheduler in AGENTS.md).
pub fn tsc_calibrate(ticks: i64) -> u64 {
    if ticks <= 0 {
        return 0;
    }
    let t0 = sys::get_ticks();
    let c0 = rdtsc();
    loop {
        for _ in 0..4096 {
            core::hint::spin_loop();
        }
        let now = sys::get_ticks();
        if now - t0 >= ticks {
            let dt = (now - t0) as u64;
            if dt == 0 {
                return 0;
            }
            return rdtsc().wrapping_sub(c0) * 100 / dt;
        }
    }
}
