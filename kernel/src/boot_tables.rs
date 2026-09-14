//! Tabelle di boot: identity map dei primi 8 MiB + GDT temporanea.
//!
//! Sono `static` const-valutate: il compilatore le emette già pronte nell'ELF
//! e il loader PVH le carica in RAM insieme al resto dell'immagine. Lo stub
//! assembly (`boot.asm`) si limita a puntare CR3 e GDTR agli indirizzi fissi.
//!
//! Layout: PD[0] → PT a pagine 4 KiB per [0, 2 MiB) (copre kernel basso a
//! 1 MiB e VGA a 0xB8000); PD[1..3] → large page 2 MiB per [2 MiB, 8 MiB).
//! Il kernel cresce coi binari embedded (`include_bytes!` in `user_binary.rs`):
//! il `.bss` ha superato i 2 MiB in Fase 17 (triple fault silenzioso al primo
//! print con timestamp, che legge `pit::TICKS` oltre il limite) — 8 MiB danno
//! margine, e `rust_main` verifica `_kernel_end < BOOT_MAP_LIMIT` fail-loud
//! prima di qualunque print (mai piu' morte a zero output).
//!
//! NB: in long mode le entry delle page table sono larghe 8 byte — qui è
//! garantito dal tipo (`u64`), l'errore classico dello stride a 4 byte non
//! può nemmeno compilare.

//! I simboli servono a `boot.asm` via indirizzo assoluto: il linker li tiene
//! vivi, ma il compilatore non vede usi Rust — allow mirato, non rimosso.
#![allow(dead_code)]

use core::mem::size_of;

pub const PML4_ADDR: u64 = 0x0009_0000;
pub const PDPT_ADDR: u64 = 0x0009_1000;
pub const PD_ADDR: u64 = 0x0009_2000;
pub const PT_ADDR: u64 = 0x0009_3000;

const PRESENT_WRITABLE: u64 = 0x003;
/// Bit PS (large page 2 MiB) per le entry PD.
const LARGE_PAGE: u64 = 0x080;

/// Tetto (esclusivo) dell'identity map di boot. `rust_main` abortisce fail-loud
/// se `_kernel_end` lo supera (vedi nota in testa).
pub const BOOT_MAP_LIMIT: u64 = 0x800000;

/// Tabella con allineamento garantito a pagina.
#[repr(C, align(4096))]
pub struct PageTable([u64; 512]);

impl PageTable {
    /// Limite per lo pseudo-descrittore LGDT (512 entry × 8 byte − 1).
    pub const LIMIT: u16 = (size_of::<Self>() - 1) as u16;
}

#[unsafe(link_section = ".pagetables.pml4")]
#[unsafe(no_mangle)]
pub static BOOT_PML4: PageTable = {
    let mut t = [0u64; 512];
    t[0] = PDPT_ADDR | PRESENT_WRITABLE;
    PageTable(t)
};

#[unsafe(link_section = ".pagetables.pdpt")]
#[unsafe(no_mangle)]
pub static BOOT_PDPT: PageTable = {
    let mut t = [0u64; 512];
    t[0] = PD_ADDR | PRESENT_WRITABLE;
    PageTable(t)
};

#[unsafe(link_section = ".pagetables.pd")]
#[unsafe(no_mangle)]
pub static BOOT_PD: PageTable = {
    let mut t = [0u64; 512];
    t[0] = PT_ADDR | PRESENT_WRITABLE;
    // Large page 2 MiB: coprono [2 MiB, 8 MiB) per la crescita del kernel
    // (binari embedded). Vedi nota in testa: 2 MiB non bastano piu'.
    t[1] = 0x200000 | PRESENT_WRITABLE | LARGE_PAGE;
    t[2] = 0x400000 | PRESENT_WRITABLE | LARGE_PAGE;
    t[3] = 0x600000 | PRESENT_WRITABLE | LARGE_PAGE;
    PageTable(t)
};

/// Identity map dei primi 8 MiB: PT a 4 KiB per [0, 2 MiB) + large page PD
/// per [2 MiB, 8 MiB). Copre kernel basso (1 MiB), VGA (0xB8000) e crescita.
#[unsafe(link_section = ".pagetables.pt")]
#[unsafe(no_mangle)]
pub static BOOT_PT: PageTable = {
    let mut t = [0u64; 512];
    let mut i = 0usize;
    while i < 512 {
        t[i] = ((i as u64) << 12) | PRESENT_WRITABLE;
        i += 1;
    }
    PageTable(t)
};

/// Selettori della GDT di boot.
pub const SEL_CODE64: u16 = 0x08;
pub const SEL_DATA: u16 = 0x10;

/// null | code64 (L=1 D=0 G=1) | data (RW)
#[repr(C, align(8))]
pub struct Gdt([u64; 3]);

#[unsafe(no_mangle)]
pub static BOOT_GDT: Gdt = Gdt([
    0x0000_0000_0000_0000,
    0x00AF_9A00_0000_FFFF,
    0x00CF_9200_0000_FFFF,
]);
