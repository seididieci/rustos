//! Tabelle di boot: identity map dei primi 2 MiB + GDT temporanea.
//!
//! Sono `static` const-valutate: il compilatore le emette già pronte nell'ELF
//! e il loader PVH le carica in RAM insieme al resto dell'immagine. Lo stub
//! assembly (`boot.asm`) si limita a puntare CR3 e GDTR agli indirizzi fissi.
//!
//! NB: in long mode le entry delle page table sono larghe 8 byte — qui è
//! garantito dal tipo (`u64`), l'errore classico dello stride a 4 byte non
//! può nemmeno compilare.

#![allow(dead_code)]

use core::mem::size_of;

pub const PML4_ADDR: u64 = 0x0009_0000;
pub const PDPT_ADDR: u64 = 0x0009_1000;
pub const PD_ADDR: u64 = 0x0009_2000;
pub const PT_ADDR: u64 = 0x0009_3000;

const PRESENT_WRITABLE: u64 = 0x003;

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
    PageTable(t)
};

/// Identity map dei primi 2 MiB: copre kernel (1 MiB) e VGA (0xB8000).
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
