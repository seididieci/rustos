//! Direct-map manager (H1): la direct map statica [0, 64 GiB) a pagine 2M
//! vive nelle tabelle di boot (`boot_tables.rs`: PDPT_DIRECT + 32 PD).
//! Le pagine 2M sono baseline long-mode su OGNI x86-64: nessun check CPUID,
//! nessun prerequisito oltre il long mode (vale per TCG qemu64 come per
//! l'hardware reale piu' vecchio — le pagine 1G avrebbero richiesto PDPE1GB).
//!
//! Tetto statico 64G: oltre, fail-loud (le configurazioni di test usano
//! <= 32G; alzare il tetto = piu' PD statiche in `boot_tables.rs`, meccanico).
//!
//! `mapped_max` = tetto (byte) della direct map attuale.

use crate::addr::DIRECT_MAP_BASE;

const STATIC_DIRECT_MAX: u64 = 64 * 1024 * 1024 * 1024;

static MAPPED_MAX: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

/// Tetto (byte) della direct map attuale. Indirizzi >= questo non sono mappati.
/// Usato solo dai selftest (`#[cfg(feature = "selftest")]` in main.rs).
#[allow(dead_code)]
pub fn mapped_max() -> u64 {
    MAPPED_MAX.load(core::sync::atomic::Ordering::Relaxed)
}

pub fn init(max_addr: u64) {
    if max_addr > STATIC_DIRECT_MAX {
        crate::serial_println!(
            "[vmm] RAM oltre 64G ({:#x}): direct map statica insufficiente",
            max_addr
        );
        loop {
            unsafe { core::arch::asm!("hlt") };
        }
    }

    let mapped = max_addr.min(STATIC_DIRECT_MAX);
    MAPPED_MAX.store(mapped, core::sync::atomic::Ordering::Relaxed);
    let total_mib = mapped / (1024 * 1024);
    crate::serial_println!(
        "[vmm] direct map {:#x} - {:#x}: {} MiB (statica, pagine 2M)",
        DIRECT_MAP_BASE,
        DIRECT_MAP_BASE + mapped,
        total_mib
    );
}
