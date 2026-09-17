//! Virtual Memory Manager: identity map dinamica con large pages (2 MiB).
//!
//! Layout page table boot (fisso):
//!   PML4 @ 0x90000  →  PDPT @ 0x91000  →  PD @ 0x92000  →  PT @ 0x93000
//!
//! Estensione dinamica (a runtime):
//!   PDPT[0] → PD @ 0x92000 (già esistente):
//!     entry 0 → PT (primi 2 MiB, 4 KiB pages)
//!     entry 1-511 → large pages (2 MiB – 1 GiB)
//!   PDPT[1..N] → PD @ 0x94000 + (i-1)×0x1000 (allocate in loco):
//!     tutti i 512 entry → large pages
//!
//! Spazio disponibile per le PD: 0x94000 – 0xFFFFF = 432 KiB = 108 PD.
//! 108 PD × 512 entry × 2 MiB = 108 GiB aggiuntivi.
//! Totale: 1 GiB (PD[0]) + 108 GiB (PD[1..108]) ≈ 109 GiB.

const PDPT_ADDR: u64 = 0x91000;
const PD_ADDR: u64 = 0x92000;
const PD_AREA_START: u64 = 0x94000;
const PD_AREA_END: u64 = 0x100000;
const PAGE_SIZE: u64 = 0x1000;
const PRESENT_WRITABLE: u64 = 0x003;
const PAGE_SIZE_2M: u64 = 0x080;

const MAX_EXTRA_PDS: usize = ((PD_AREA_END - PD_AREA_START) / PAGE_SIZE) as usize;

static MAPPED_MAX: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

/// Tetto (byte) dell'identity map attuale. Indirizzi >= questo non sono mappati.
/// Usato solo dai selftest (`#[cfg(feature = "selftest")]` in main.rs).
#[allow(dead_code)]
pub fn mapped_max() -> u64 {
    MAPPED_MAX.load(core::sync::atomic::Ordering::Relaxed)
}

pub fn init(max_addr: u64) {
    use crate::addr::phys_to_virt;
    let pdpt = unsafe { &mut *(phys_to_virt(PDPT_ADDR) as *mut [u64; 512]) };
    let pd0 = unsafe { &mut *(phys_to_virt(PD_ADDR) as *mut [u64; 512]) };

    // ── PD[0]: entries 1-511 → large pages (2 MiB – 1 GiB) ────────
    for i in 1..512usize {
        let phys = (i as u64) << 21;
        if phys >= max_addr {
            break;
        }
        pd0[i] = phys | PRESENT_WRITABLE | PAGE_SIZE_2M;
    }

    // ── PD aggiuntive: copertura beyond 1 GiB ──────────────────────
    let mut covered: u64 = 1024 * 1024 * 1024; // 1 GiB già coperto da PD[0]
    let mut extra_pds: usize = 0;

    'outer: for pdpt_idx in 1..512usize {
        if covered >= max_addr || extra_pds >= MAX_EXTRA_PDS {
            break;
        }

        let pd_phys = PD_AREA_START + (extra_pds as u64) * PAGE_SIZE;
        pdpt[pdpt_idx] = pd_phys | PRESENT_WRITABLE;

        let new_pd = unsafe { &mut *(phys_to_virt(pd_phys) as *mut [u64; 512]) };

        for j in 0..512usize {
            let phys = covered + (j as u64) * 2 * 1024 * 1024;
            if phys >= max_addr {
                break 'outer;
            }
            new_pd[j] = phys | PRESENT_WRITABLE | PAGE_SIZE_2M;
        }

        covered += 1024 * 1024 * 1024; // +1 GiB per PD
        extra_pds += 1;
    }

    // Flush TLB.
    unsafe {
        let cr3: u64;
        core::arch::asm!("mov {cr3}, cr3", cr3 = out(reg) cr3);
        core::arch::asm!("mov cr3, {cr3}", cr3 = in(reg) cr3);
    }

    let mapped = covered.min(max_addr);
    MAPPED_MAX.store(mapped, core::sync::atomic::Ordering::Relaxed);
    let total_mib = mapped / (1024 * 1024);
    crate::serial_println!(
        "[vmm] identity map: {} MiB ({} PD, large pages)",
        total_mib,
        1 + extra_pds
    );
}
