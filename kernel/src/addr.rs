//! Conversione indirizzi fisico <-> virtuale (Higher-half, 27.2).
//!
//! 27.2: kernel alto (VMA `KERN_VIRT_BASE`) caricato basso (LMA `KERN_PHYS_BASE`)
//! + direct map di tutta la RAM a `DIRECT_MAP_BASE`. Tutti i siti che
//! dereferenziano memoria fisica passano da questi helper (introdotti in 27.1
//! a offset zero); qui solo le costanti sono cambiate.
//!
//! Domini (da non confondere):
//! - `phys_to_virt` / `virt_to_phys`: RAM generica via direct map
//!   (`DIRECT_MAP_BASE`). Per tabelle di pagina, bitmap frame, stack kernel,
//!   zero-fill, VGA, strutture PVH.
//! - `kern_phys_to_virt` / `kern_virt_to_phys`: immagine del kernel (linker:
//!   VMA `KERN_VIRT_BASE`, LMA `KERN_PHYS_BASE`).
//! - Gli indirizzi USER (spazio per-processo) e i puntatori a `static` del
//!   kernel NON passano di qui: sono gia' virtuali.

/// Indirizzo fisico di caricamento (LMA): base ELF + nota PVH (`linker.ld`).
pub const KERN_PHYS_BASE: u64 = 0x0010_0000;

/// Indirizzo virtuale del kernel (VMA). 27.2: -2 GiB + 1M, cioe'
/// `0xFFFF_FFFF_8010_0000` (come Linux, che parte a `0xFFFFFFFF81000000`):
/// lo scarto VMA-phys di 1M rende le entry PD a 2M allineate (la PD_K mappa
/// VMA [KERN-1M+i*2M) -> phys [i*2M): basi pari). VMA tonda a -2G con LMA a
/// 1M darebbe basi 2M-disallineate (bit 20 riservato -> #PF con RSVD,
/// osservato al primo boot 27.2).
pub const KERN_VIRT_BASE: u64 = 0xFFFF_FFFF_8010_0000;

/// Scarto VMA - LMA dell'immagine kernel. 27.2 = 0xFFFF_FFFF_8000_0000.
/// Gate-0 `readelf` verifica VMA - LMA == questo per ogni PT_LOAD.
pub const KERNEL_OFFSET: u64 = KERN_VIRT_BASE - KERN_PHYS_BASE;

/// Base della direct map di tutta la RAM. 27.2: stile Linux direct mapping.
pub const DIRECT_MAP_BASE: u64 = 0xFFFF_8880_0000_0000;

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

/// Immagine kernel: fisico (LMA) -> virtuale (VMA).
#[allow(dead_code)]
pub const fn kern_phys_to_virt(p: u64) -> u64 {
    p + KERNEL_OFFSET
}

/// La PD_K (27.2) mappa VMA [PDBASE+i*2M) -> phys [i*2M] con PDBASE = base 1G
/// della regione di KERN: l'immagine (VMA = LMA+OFFSET) cade su basi pari
/// sse KERN e LMA sono congrui mod 1G (qui entrambi a 1M). Invariante
/// verificata dal compilatore (basi dispari = bit 20 riservato -> #PF RSVD).
const _: () = assert!(
    (KERN_VIRT_BASE & 0x3FFF_FFFF) == KERN_PHYS_BASE,
    "KERN_VIRT_BASE deve essere congrua a KERN_PHYS_BASE mod 1G"
);

/// Immagine kernel: virtuale (VMA) -> fisico (LMA, per contabilita' frame).
pub const fn kern_virt_to_phys(v: u64) -> u64 {
    v - KERNEL_OFFSET
}
