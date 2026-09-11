//! Parser della struttura di avvio PVH (`hvm_start_info`) lasciata dal
//! loader di QEMU in memoria bassa, con puntatore in EBX all'entry.
//!
//! Layout ESATTO da Xen (`xen/include/public/arch-x86/hvm/start_info.h`).
//! I campi della memory map esistono solo da versione >= 1, e nelle entry
//! l'ordine e' addr/size/type (diverso dall'e820 classico!).

#![allow(dead_code)]

use core::slice;

/// Valore che deve avere [`HvmStartInfo::magic`] ("xEn3" con bit alto su E).
pub const HVM_START_MAGIC: u32 = 0x336E_C578;

pub const MEM_RAM: u32 = 1;
pub const MEM_RESERVED: u32 = 2;
pub const MEM_ACPI: u32 = 3;
pub const MEM_NVS: u32 = 4;
pub const MEM_UNUSABLE: u32 = 5;
pub const MEM_DISABLED: u32 = 6;
pub const MEM_PMEM: u32 = 7;

/// Struttura passata dal loader: `EBX` ne contiene l'indirizzo fisico.
#[repr(C)]
pub struct HvmStartInfo {
    pub magic: u32,
    /// Versione del formato: i campi memmap esistono solo se >= 1.
    pub version: u32,
    pub flags: u32,
    pub nr_modules: u32,
    pub modlist_paddr: u64,
    /// Indirizzo fisico della command line NUL-terminata (0 se assente).
    pub cmdline_paddr: u64,
    /// Indirizzo fisico dello RSDP ACPI (0 se assente) — utile per Fasi future.
    pub rsdp_paddr: u64,
    // --- campi presenti solo da versione 1 ---
    pub memmap_paddr: u64,
    pub memmap_entries: u32,
    pub _reserved: u32,
}

/// Un'entry della memory map (formato Xen, NON e820 classico).
#[repr(C)]
pub struct HvmMemmapEntry {
    pub addr: u64,
    pub size: u64,
    pub kind: u32,
    pub _reserved: u32,
}

impl HvmMemmapEntry {
    pub const fn end(&self) -> u64 {
        self.addr + self.size
    }

    fn kind_str(&self) -> &'static str {
        match self.kind {
            MEM_RAM => "usable",
            MEM_RESERVED => "reserved",
            MEM_ACPI => "acpi",
            MEM_NVS => "nvs",
            MEM_UNUSABLE => "unusable",
            MEM_DISABLED => "disabled",
            MEM_PMEM => "pmem",
            _ => "?",
        }
    }
}

impl core::fmt::Display for HvmMemmapEntry {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{:<9} {:#018x} - {:#018x}  ({} KiB)",
            self.kind_str(),
            self.addr,
            self.end(),
            self.size / 1024
        )
    }
}

/// Interpreta un indirizzo fisico come riferimento alla start-info.
///
/// Safety: l'indirizzo deve essere quello passato dal loader PVH in EBX e la
/// zona deve essere mappata identity (i primi 2 MiB lo sono).
pub unsafe fn at(phys: u64) -> &'static HvmStartInfo {
    unsafe { &*(phys as usize as *const HvmStartInfo) }
}

/// Tabella della memoria come slice tipizzata.
///
/// Ritorna slice vuota se il loader non ha fornito una mappa.
pub fn memmap(info: &HvmStartInfo) -> &[HvmMemmapEntry] {
    if info.version < 1 || info.memmap_entries == 0 || info.memmap_paddr == 0 {
        return &[];
    }
    assert!(info.memmap_entries < 256, "memory map irrealistica");
    unsafe {
        slice::from_raw_parts(
            info.memmap_paddr as usize as *const HvmMemmapEntry,
            info.memmap_entries as usize,
        )
    }
}

/// Stampa info di boot e memory map su seriale.
pub fn dump(info: &HvmStartInfo) {
    crate::serial_println!(
        "[boot] hvm_start_info @ {:#x} (magic {:#010x}, versione {})",
        info as *const _ as usize,
        info.magic,
        info.version
    );

    let cmd = cmdline(info);
    crate::serial_println!(
        "[boot] cmdline: {}",
        cmd.unwrap_or("<assente>")
    );
    crate::serial_println!("[boot] rsdp_paddr: {:#x}", info.rsdp_paddr);

    let map = memmap(info);
    crate::serial_println!("[mmap] {} zone:", map.len());
    for e in map {
        crate::serial_println!("[mmap] {}", e);
    }
}

/// Command line come &str, se presente e UTF-8 valido.
pub fn cmdline(info: &HvmStartInfo) -> Option<&'static str> {
    if info.cmdline_paddr == 0 {
        return None;
    }
    unsafe {
        let mut len = 0usize;
        let mut p = info.cmdline_paddr as *const u8;
        while *p != 0 && len < 512 {
            len += 1;
            p = p.add(1);
        }
        let bytes = slice::from_raw_parts(info.cmdline_paddr as *const u8, len);
        core::str::from_utf8(bytes).ok()
    }
}
