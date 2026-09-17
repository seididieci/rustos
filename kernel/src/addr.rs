//! Conversione indirizzi fisico <-> virtuale (Higher-half, H0).
//!
//! H0: offset ZERO (identity map) — nessun cambio di comportamento. Tutti i
//! siti che dereferenziano memoria fisica passano gia' da questi helper, cosi'
//! H1 (kernel in alto + direct map) cambia solo le costanti qui sotto.
//!
//! Domini (da non confondere):
//! - `phys_to_virt` / `virt_to_phys`: RAM generica via direct map (oggi
//!   identity; H1: `DIRECT_MAP_BASE = 0xFFFF_8880_0000_0000`). Per tabelle di
//!   pagina, bitmap frame, stack kernel, zero-fill, VGA, strutture PVH.
//! - `kern_phys_to_virt` / `kern_virt_to_phys`: immagine del kernel (linker).
//!   Oggi VMA == LMA a 1 MiB; H1: VMA `0xFFFF_FFFF_8000_0000`, LMA 1 MiB.
//! - Gli indirizzi USER (spazio per-processo) e i puntatori a `static` del
//!   kernel NON passano di qui: sono gia' virtuali.

/// Indirizzo fisico di caricamento (LMA): base ELF + nota PVH (`linker.ld`).
pub const KERN_PHYS_BASE: u64 = 0x0010_0000;

/// Indirizzo virtuale del kernel (VMA). H0: uguale alla LMA (identity).
/// H1: `0xFFFF_FFFF_8000_0000` (convenzione -2 GiB, stile Linux).
pub const KERN_VIRT_BASE: u64 = 0x0010_0000;

/// Scarto VMA - LMA dell'immagine kernel. H0 = 0.
pub const KERNEL_OFFSET: u64 = KERN_VIRT_BASE - KERN_PHYS_BASE;

/// Base della direct map di tutta la RAM. H0 = 0 (identity).
/// H1: `0xFFFF_8880_0000_0000` (stile Linux direct mapping).
pub const DIRECT_MAP_BASE: u64 = 0x0000_0000_0000_0000;

/// Frame fisico del buffer testo VGA.
pub const VGA_PHYS: u64 = 0x0000_B8000;

/// RAM generica: fisico -> virtuale (via direct map).
pub const fn phys_to_virt(p: u64) -> u64 {
    p + DIRECT_MAP_BASE
}

/// RAM generica: virtuale (direct map) -> fisico.
pub const fn virt_to_phys(v: u64) -> u64 {
    v - DIRECT_MAP_BASE
}

/// Immagine kernel: fisico (LMA) -> virtuale (VMA). H0: identita'.
/// Oggi inutilizzato (VMA == LMA): servira' in H1 per i simboli linkati alti.
#[allow(dead_code)]
pub const fn kern_phys_to_virt(p: u64) -> u64 {
    p + KERNEL_OFFSET
}

/// Immagine kernel: virtuale (VMA) -> fisico (LMA, per contabilita' frame).
pub const fn kern_virt_to_phys(v: u64) -> u64 {
    v - KERNEL_OFFSET
}
