//! Driver ATA PIO primario (read-only) in userspace.
//!
//! Accede alle porte 0x1F0-0x1F7 (abilite dal TSS per-processo di userfs).
//! Polling con timeout a contatore: senza disco la porta status resta 0xFF
//! (BSY set) e le operazioni falliscono in tempo finito.

use crate::io;

/// Porta base del controller ATA primario (drive master).
const BASE: u16 = 0x1F0;

/// Timeout di polling in iterazioni (nessuna unita' di tempo: solo un freno).
/// Su QEMU senza disco il controller non clear mai BSY: 2M iterazioni di `inb`
/// emulato costano decine di secondi; con disco la risposta arriva in pochi
/// microsecondi, quindi un timeout contenuto basta ed evita il blocco.
const TIMEOUT: u32 = 200_000;

pub struct AtaDisk;

impl AtaDisk {
    pub fn primary_master() -> AtaDisk {
        AtaDisk
    }

    /// Attende che il controller non sia piu' busy. Ritorna `false` su timeout.
    fn wait_not_busy(&self) -> bool {
        for _ in 0..TIMEOUT {
            let st = unsafe { io::inb(BASE + 7) };
            if st & 0x80 == 0 {
                return true;
            }
            core::hint::spin_loop();
        }
        false
    }

    /// Legge un settore (512 byte) via PIO. Ritorna `false` su errore/timeout.
    pub fn read_sector(&self, lba: u32, buf: &mut [u8; 512]) -> bool {
        if !self.wait_not_busy() {
            return false;
        }

        // 0xE0 = LBA mode + master; i 4 bit alti di LBA28 in 0x1F6.
        unsafe {
            io::outb(BASE + 6, 0xE0 | ((lba >> 24) & 0x0F) as u8);
            io::outb(BASE + 1, 0x00);          // feature
            io::outb(BASE + 2, 0x01);          // sector count = 1
            io::outb(BASE + 3, (lba & 0xFF) as u8);
            io::outb(BASE + 4, ((lba >> 8) & 0xFF) as u8);
            io::outb(BASE + 5, ((lba >> 16) & 0xFF) as u8);
            io::outb(BASE + 7, 0x20);          // READ SECTORS with retry
        }

        if !self.wait_not_busy() {
            return false;
        }

        // ERR bit (0x01) setto subito dopo BSY clear = errore.
        let st = unsafe { io::inb(BASE + 7) };
        if st & 0x01 != 0 {
            return false;
        }

        // Attende DRQ (data request) prima di leggere i dati.
        for _ in 0..TIMEOUT {
            let st = unsafe { io::inb(BASE + 7) };
            if st & 0x08 != 0 {
                break;
            }
            if st & 0x01 != 0 {
                return false; // ERR durante l'attesa dati
            }
            core::hint::spin_loop();
        }

        // Legge 256 word (512 byte) dal data register.
        for i in 0..256 {
            let w = unsafe { io::inw(BASE) };
            buf[i * 2] = (w & 0xFF) as u8;
            buf[i * 2 + 1] = (w >> 8) as u8;
        }
        true
    }
}
