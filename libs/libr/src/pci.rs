//! Spazio di configurazione PCI in ring 3 (Fase 38.0d, ATA DMA).
//!
//! Modulo condiviso e traslocabile (il servizio `userland/pci` nascerà solo al
//! SECONDO consumer — audio — per YAGNI): oggi lo usa solo userdisk per trovare
//! il PIIX3-IDE di QEMU, programmarne la BAR4 Bus-Master e abilitarne il DMA.
//!
//! Contesto: col boot diretto PVH nessun BIOS programma le BAR — BAR4 all'avvio
//! vale 0 e il Bus-Master e' disabilitato. Siamo noi a scegliere la finestra
//! (`BM_BASE`, QEMU-scoped: sopra il legacy non c'e' nulla di programmato) e a
//! verificarla in lettura. Fuori finestra o senza PIIX3 → `None` e il chiamante
//! resta in PIO (fallback dichiarato, codice PIO intatto).
//!
//! Serve `io_ranges` con `0xCF8-0xCFC` (conf) + la finestra BM (TSS
//! per-processo, ADR-0006): senza, ogni accesso e' #GP. Nota onesta (ADR-0026):
//! chi ha il conf PCI puo' riprogrammare qualunque device — il contenimento
//! vale poco finche' DMA compromesso = game over (no IOMMU).

use crate::pio;

/// Porte dello spazio di configurazione PCI (mecanismo 1).
pub const PCI_CONF_ADDR: u16 = 0xCF8;
pub const PCI_CONF_DATA: u16 = 0xCFC;

/// Finestra I/O scelta per la BMIBA (38.0d): 16 byte (`PIIX_BAR4_SIZE`),
/// sopra il legacy (libero col boot diretto: nessuna BAR programmata).
/// QEMU-scoped per dichiarazione — fuori QEMU va rivalutata.
pub const BM_BASE: u16 = 0xC000;
/// Dimensione in byte della regione Bus-Master PIIX (2 canali × 8).
pub const PIIX_BAR4_SIZE: u16 = 16;

/// PIIX3 IDE di QEMU (`-drive if=ide`).
pub const PIIX3_VENDOR: u16 = 0x8086;
pub const PIIX3_IDE_DEVICE: u16 = 0x7010;

/// Funzione PCI (bus/device/function).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PciDev {
    pub bus: u8,
    pub dev: u8,
    pub func: u8,
}

/// Legge 32 bit del conf di `dev` a `offset` (deve essere allineato a 4).
pub fn config_read(dev: PciDev, offset: u8) -> u32 {
    let addr: u32 = 0x8000_0000
        | ((dev.bus as u32) << 16)
        | ((dev.dev as u32) << 11)
        | ((dev.func as u32) << 8)
        | ((offset & 0xFC) as u32);
    unsafe {
        pio::outl(PCI_CONF_ADDR, addr);
        pio::inl(PCI_CONF_DATA)
    }
}

/// Scrive 32 bit del conf di `dev` a `offset` (deve essere allineato a 4).
pub fn config_write(dev: PciDev, offset: u8, val: u32) {
    let addr: u32 = 0x8000_0000
        | ((dev.bus as u32) << 16)
        | ((dev.dev as u32) << 11)
        | ((dev.func as u32) << 8)
        | ((offset & 0xFC) as u32);
    unsafe {
        pio::outl(PCI_CONF_ADDR, addr);
        pio::outl(PCI_CONF_DATA, val);
    }
}

/// Cerca il PIIX3-IDE sul bus 0 (tutte le function: su QEMU e' 00:01.1).
/// `None` = assente o diverso (fallback PIO, mai hang: solo letture bound).
pub fn find_piix3_ide() -> Option<PciDev> {
    for dev in 0..32u8 {
        for func in 0..8u8 {
            let d = PciDev { bus: 0, dev, func };
            let id = config_read(d, 0x00);
            if id == ((PIIX3_IDE_DEVICE as u32) << 16) | (PIIX3_VENDOR as u32) {
                return Some(d);
            }
        }
    }
    None
}

/// Programma la BAR4 (offset 0x20) a `BM_BASE` e abilita I/O Space + Bus Master
/// nel command (offset 0x04, bit 0 e 2, resto preservato). Ritorna la BMIBA se
/// la readback conferma, altrimenti `None` (PIO).
///
/// Se la BAR vale gia' `BM_BASE` (restart userdisk senza reboot: la BAR
/// sopravvive) la si tiene; altrimenti la si riprogramma — QEMU di default la
/// pre-programma a `0xC040`, fuori dal nostro grant statico. Col boot diretto
/// nessun driver e' live sul Bus-Master (il PIO usa le porte legacy, non la
/// BAR4): la readback e' la verifica, se non tiene si resta in PIO.
pub fn enable_bus_master(dev: PciDev) -> Option<u16> {
    let bar = config_read(dev, 0x20);
    if bar != ((BM_BASE as u32) | 0x1) {
        config_write(dev, 0x20, (BM_BASE as u32) | 0x1);
        let back = config_read(dev, 0x20);
        if back != ((BM_BASE as u32) | 0x1) {
            return None;
        }
    }
    // Command: preserva, accendi I/O Space (bit 0) + Bus Master (bit 2).
    let cmd = (config_read(dev, 0x04) & 0xFFFF) as u16;
    let cmd_addr_val = (config_read(dev, 0x04) & 0xFFFF_0000) | ((cmd | 0x0005) as u32);
    config_write(dev, 0x04, cmd_addr_val);
    Some(BM_BASE)
}

/// Linea IRQ PCI del device (offset 0x3C, byte basso): solo diagnostica — il
/// routing resta via PIC fisso (IRQ14/15, Fase 38.0c), mai via questa.
pub fn irq_line(dev: PciDev) -> u8 {
    (config_read(dev, 0x3C) & 0xFF) as u8
}
