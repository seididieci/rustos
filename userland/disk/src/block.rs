//! Driver ATA PIO (read-only) in userspace (Fase 16, da `userfs/block.rs`).
//!
//! Generalizzato a qualunque canale/drive (primario + secondario, master +
//! slave) e a LBA48 (`READ SECTORS EXT`): il rilevamento (`detect.rs`)
//! decide capacita' e geometria, qui solo il trasferimento. Polling con
//! timeout a contatore, come prima: senza disco la porta status resta 0xFF
//! (BSY set) e le operazioni falliscono in tempo finito.

use crate::io;

/// Timeout di polling in iterazioni (nessuna unita' di tempo: solo un freno).
/// Su QEMU senza disco il controller non clear mai BSY: 2M iterazioni di `inb`
/// emulato costano decine di secondi; con disco la risposta arriva in pochi
/// microsecondi, quindi un timeout contenuto basta ed evita il blocco.
const TIMEOUT: u32 = 200_000;

pub struct AtaDisk {
    /// Porta base comandi (0x1F0 primario, 0x170 secondario).
    cmd: u16,
    /// 0 = master, 1 = slave (bit 4 del registro drive/head).
    drive: u8,
    /// Vero se IDENTIFY riporta LBA48 (word 83 bit 10): usa `READ SECTORS EXT`.
    pub lba48: bool,
}

impl AtaDisk {
    pub fn open(cmd: u16, drive: u8, lba48: bool) -> AtaDisk {
        AtaDisk { cmd, drive, lba48 }
    }

    /// Attende che il controller non sia piu' busy. Ritorna `false` su timeout.
    fn wait_not_busy(&self) -> bool {
        for _ in 0..TIMEOUT {
            let st = unsafe { io::inb(self.cmd + 7) };
            if st & 0x80 == 0 {
                return true;
            }
            core::hint::spin_loop();
        }
        false
    }

    /// Attende DRQ (data request) o errore. Ritorna `false` su timeout/errore.
    fn wait_drq(&self) -> bool {
        for _ in 0..TIMEOUT {
            let st = unsafe { io::inb(self.cmd + 7) };
            if st & 0x08 != 0 {
                return true;
            }
            if st & 0x01 != 0 {
                return false; // ERR durante l'attesa dati
            }
            core::hint::spin_loop();
        }
        false
    }

    /// Legge le 256 word di dati dopo DRQ nel buffer.
    fn read_data(&self, buf: &mut [u8; 512]) {
        for i in 0..256 {
            let w = unsafe { io::inw(self.cmd) };
            buf[i * 2] = (w & 0xFF) as u8;
            buf[i * 2 + 1] = (w >> 8) as u8;
        }
    }

    /// Legge un settore (512 byte) via PIO LBA28. Ritorna `false` su
    /// errore/timeout o se `lba` non sta in 28 bit.
    fn read_lba28(&self, lba: u64, buf: &mut [u8; 512]) -> bool {
        if lba > 0x0FFF_FFFF {
            return false;
        }
        if !self.wait_not_busy() {
            return false;
        }

        // 0xE0 = LBA mode + drive; i 4 bit alti di LBA28 in drive/head.
        unsafe {
            io::outb(self.cmd + 6, 0xE0 | (self.drive << 4) | ((lba >> 24) & 0x0F) as u8);
            io::outb(self.cmd + 1, 0x00); // feature
            io::outb(self.cmd + 2, 0x01); // sector count = 1
            io::outb(self.cmd + 3, (lba & 0xFF) as u8);
            io::outb(self.cmd + 4, ((lba >> 8) & 0xFF) as u8);
            io::outb(self.cmd + 5, ((lba >> 16) & 0xFF) as u8);
            io::outb(self.cmd + 7, 0x20); // READ SECTORS with retry
        }

        if !self.wait_not_busy() {
            return false;
        }

        // ERR bit (0x01) setto subito dopo BSY clear = errore.
        let st = unsafe { io::inb(self.cmd + 7) };
        if st & 0x01 != 0 {
            return false;
        }
        if !self.wait_drq() {
            return false;
        }
        self.read_data(buf);
        true
    }

    /// Legge un settore (512 byte) via PIO LBA48 (`READ SECTORS EXT 0x24`).
    /// Ritorna `false` su errore/timeout o se `lba` non sta in 48 bit.
    fn read_lba48(&self, lba: u64, buf: &mut [u8; 512]) -> bool {
        if lba > 0xFFFF_FFFF_FFFF {
            return false;
        }
        if !self.wait_not_busy() {
            return false;
        }

        // Ordine 48-bit: prima i byte alti (HOB), poi i bassi. Count = 1.
        // 0x40 = LBA mode + drive (i bit alti di LBA viaggiano nei registri).
        unsafe {
            io::outb(self.cmd + 6, 0x40 | (self.drive << 4));
            io::outb(self.cmd + 1, 0x00); // features high
            io::outb(self.cmd + 2, 0x00); // count high
            io::outb(self.cmd + 3, ((lba >> 24) & 0xFF) as u8); // LBA 3
            io::outb(self.cmd + 4, ((lba >> 32) & 0xFF) as u8); // LBA 4
            io::outb(self.cmd + 5, ((lba >> 40) & 0xFF) as u8); // LBA 5
            io::outb(self.cmd + 1, 0x00); // features low
            io::outb(self.cmd + 2, 0x01); // count low = 1
            io::outb(self.cmd + 3, (lba & 0xFF) as u8); // LBA 0
            io::outb(self.cmd + 4, ((lba >> 8) & 0xFF) as u8); // LBA 1
            io::outb(self.cmd + 5, ((lba >> 16) & 0xFF) as u8); // LBA 2
            io::outb(self.cmd + 7, 0x24); // READ SECTORS EXT
        }

        if !self.wait_not_busy() {
            return false;
        }
        let st = unsafe { io::inb(self.cmd + 7) };
        if st & 0x01 != 0 {
            return false;
        }
        if !self.wait_drq() {
            return false;
        }
        self.read_data(buf);
        true
    }

    /// Legge un settore (512 byte) via PIO. Sceglie LBA28/LBA48 dalla
    /// capacita' rilevata. Ritorna `false` su errore/timeout/fuori range.
    pub fn read_sector(&self, lba: u64, buf: &mut [u8; 512]) -> bool {
        if self.lba48 {
            self.read_lba48(lba, buf)
        } else {
            self.read_lba28(lba, buf)
        }
    }
}
