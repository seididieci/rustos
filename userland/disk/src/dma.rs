//! Motore DMA Bus-Master PIIX (Fase 38.1c).
//!
//! Trasferimenti `READ/WRITE DMA EXT` su staging contigua (1 pagina da
//! `SYS_DMA_ALLOC`: PRD a offset 0, dati a offset 64). Protocollo `DISK_*`
//! INVARIATO e userfs intoccato: il motore e' un'alternativa interna al PIO
//! per le stesse `(handle, lba, n)` — ogni errore degrada a PIO per-op.
//!
//! Attesa completamento = **poll boundato del BM status** (come il PIO, ma su
//! UNA porta invece che per-word; timeout = fallback PIO, mai wedge oltre il
//! bound). NON blocking-recv, per due semantiche kernel provate sul campo:
//! (1) `pop_msg` riscrive la reply implicita (`reply_chan`) a OGNI `recv`
//! con `req_id >= 0` — e le notify IRQ hanno `req_id == 0`: un `recv` tra
//! richiesta e reply perde la reply a userfs (hang);
//! (2) la `send` sincrona fa `push` (scarta a coda piena!) ma blocca comunque
//! il mittente: messaggi accumulati riempiono la coda e la send di userfs
//! viene scartata in silenzio (hang permanente a code vuote).
//! Quindi: MAI `recv` tra richiesta e reply (poll hardware, niente syscall),
//! e drain delle notify IRQ a testa-loop (sicuro: `reply_chan` e' None dopo
//! ogni reply, e il prossimo `recv` lo riscrive prima di qualunque reply).
//! L'IRQ resta osservabilita' (contatore `irq_drained`) + base del 38.2, dove
//! il multiplexing vero (serve-while-pending) richiedera' una reply esplicita.
//! NOTA 38.1c: su QEMU 10.2 la notify IRQ14 non arriva mai in userspace
//! (device alza INTR, PIC lo conta — `info irq` 317 — ma la CPU non vettora
//! 0x2E: IRR slave resta pending; causa ignota, da sciogliere in 38.2 che ne
//! dipende). Il completamento NON dipende dalle notify: solo dal poll.
//!
//! Chiusura (sempre, anche a errore): STOP → lettura ATA status (spegne l'IRQ
//! del drive — PRIMA del clear BM, ordine del level-triggered) → clear
//! INTR/ERROR → check. Lo STOP+clear a init serve anche dopo kill+restart
//! (t32): la BAR/BM sopravvivono al processo.
//!
//! Il timeout su device impallato degrada a PIO (che fallisce a sua volta →
//! ERR all'utente, mai wedge): QEMU non impalla.

use super::*;
use libr::pio as io;

/// Offset registri Bus-Master dentro il canale (primario +0, secondario +8).
const BM_CMD_OFF: u16 = 0;
const BM_STATUS_OFF: u16 = 2;
const BM_PRD_OFF: u16 = 4;
/// Bit registro command: START/STOP + direzione (1 = read disco→mem).
const BM_CMD_START: u8 = 0x01;
const BM_CMD_RW: u8 = 0x08;
/// Bit registro status: ACTIVE (ro) + ERROR/INTR (write-1-to-clear).
const BM_ST_ACTIVE: u8 = 0x01;
const BM_ST_ERROR: u8 = 0x02;
const BM_ST_INTR: u8 = 0x04;
/// Bound poll pre-wait (stesso del PIO in `block.rs`: solo un freno).
const DMA_TIMEOUT: u32 = 200_000;
/// Staging: PRD (8 entry × 8 B) a offset 0, dati a offset 64 (allineato).
pub const PRD_MAX_ENTRIES: usize = 8;
const DATA_OFF: u64 = 64;
/// Log contatori ogni N trasferimenti (come la cache: throttled, mai spam).
const STAT_EVERY: u64 = 512;

/// Scrive le entry PRD per `(phys, len)` in `staging[0..64]`, splittando ai
/// confini 64K (il device non attraversa il confine in una entry). Layout
/// PIIX: `[base:4 LE][count:2 LE][0x00][EOT]`, EOT = 0x80 sull'ultima.
/// Ritorna il numero di entry o `None` (len 0 / oltre 7 settori / oltre il
/// 4G (PIIX a 32 bit) / oltre cap 8 — tutti fallback PIO dal chiamante).
fn build_prd(staging: *mut u8, phys: u64, len: usize) -> Option<usize> {
    if len == 0 || len > 7 * 512 {
        return None;
    }
    if phys + len as u64 > 0x1_0000_0000 {
        return None;
    }
    let mut entries = 0usize;
    let mut off = 0u64;
    let total = len as u64;
    while off < total {
        if entries >= PRD_MAX_ENTRIES {
            return None;
        }
        let base = phys + off;
        let to_boundary = 0x10000 - (base & 0xFFFF);
        let mut run = total - off;
        if run > to_boundary {
            run = to_boundary;
        }
        unsafe {
            let e = staging.add(entries * 8);
            core::ptr::write_unaligned(e as *mut u32, base as u32);
            core::ptr::write_unaligned(e.add(4) as *mut u16, run as u16);
            *e.add(6) = 0;
            *e.add(7) = 0; // EOT sotto, alzato sull'ultima
        }
        entries += 1;
        off += run;
    }
    unsafe {
        *staging.add((entries - 1) * 8 + 7) = 0x80;
    }
    Some(entries)
}

/// Motore DMA: BMIBA negoziata (38.0d) + staging contigua (38.1a) + modi per
/// disco (38.1b, parallelo a `disks` in `server.rs`: `None` = PIO).
pub struct DmaEngine {
    bmiba: u16,
    staging_va: u64,
    staging_phys: u64,
    dma_ok: u64,
    dma_fb: u64,
    /// Notify IRQ stale drenate dal loop (prova che il routing 38.0c funziona;
    /// il completamento NON dipende da loro: vedi `wait_done`).
    irq_drained: u64,
}

impl DmaEngine {
    /// Inizializza il motore: `bmiba` da 38.0d (`None` = niente DMA),
    /// `modes` = modi negoziati per disco. Alloca 1 pagina staging e AZZERA
    /// il BM (STOP + clear su entrambi i canali: dopo kill+restart (t32) lo
    /// stato sopravvive al processo). `None` = PIO puro (data-plane intatto).
    pub fn init(bmiba: Option<u16>, modes: &[Option<u8>]) -> Option<DmaEngine> {
        let bmiba = bmiba?;
        if !modes.iter().any(|m| m.is_some()) {
            println!("[userdisk] DMA: nessun disco con modo — resto in PIO");
            return None;
        }
        let phys = match libr::dma_alloc(1) {
            Ok(p) => p,
            Err(()) => {
                println!("[userdisk] DMA: staging alloc fallita — resto in PIO");
                return None;
            }
        };
        for chan in [0u16, 8u16] {
            unsafe {
                let c = io::inb(bmiba + chan + BM_CMD_OFF);
                io::outb(bmiba + chan + BM_CMD_OFF, c & !BM_CMD_START);
                io::outb(bmiba + chan + BM_STATUS_OFF, BM_ST_ERROR | BM_ST_INTR);
            }
        }
        println!(
            "[userdisk] DMA engine: bmiba={:#x} staging phys={:#x} (PRD+dati, 1 pagina)",
            bmiba, phys
        );
        Some(DmaEngine {
            bmiba,
            staging_va: libr::USER_DMA_VA,
            staging_phys: phys,
            dma_ok: 0,
            dma_fb: 0,
            irq_drained: 0,
        })
    }

    /// Contatori (prova d'uso reale in 38.3: `dma_ok > 0`, `dma_fb == 0`).
    fn note(&mut self, ok: bool) {
        if ok {
            self.dma_ok += 1;
        } else {
            self.dma_fb += 1;
        }
        let t = self.dma_ok + self.dma_fb;
        if t % STAT_EVERY == 0 {
            println!(
                "[userdisk] DMA xfers: ok={} fb={} irq_drained={} (fb = fallback PIO per-op)",
                self.dma_ok, self.dma_fb, self.irq_drained
            );
        }
    }

    /// Conta una notify IRQ drenata dal loop (mai osservate su QEMU 10.2, ma
    /// il drain resta necessario per tenere la coda pulita in ogni caso).
    pub fn note_irq_drained(&mut self) {
        self.irq_drained += 1;
    }

    fn bm_cmd(&self, chan: u16) -> u16 {
        self.bmiba + chan + BM_CMD_OFF
    }

    fn bm_status(&self, chan: u16) -> u16 {
        self.bmiba + chan + BM_STATUS_OFF
    }

    /// True se il BM segnala fine (INTR) o errore (ERROR). ACTIVE da solo =
    /// ancora in corsa (non basta: a fine op il drive alza INTR).
    fn is_done(&self, chan: u16) -> bool {
        let st = unsafe { io::inb(self.bm_status(chan)) };
        st & (BM_ST_INTR | BM_ST_ERROR) != 0
    }

    /// Attende che il BM sia fermo (ACTIVE spento), bound come il PIO.
    fn wait_idle(&self, chan: u16) -> bool {
        for _ in 0..DMA_TIMEOUT {
            let st = unsafe { io::inb(self.bm_status(chan)) };
            if st & BM_ST_ACTIVE == 0 {
                return true;
            }
            core::hint::spin_loop();
        }
        false
    }

    /// Ferma il BM sul canale (STOP; non tocca INTR/ERROR: li chiude `finish`).
    fn stop(&self, chan: u16) {
        unsafe {
            let c = io::inb(self.bm_cmd(chan));
            io::outb(self.bm_cmd(chan), c & !BM_CMD_START);
        }
    }

    /// Chiusura post-wait (sempre): STOP → lettura ATA status (spegne l'IRQ
    /// del drive — PRIMA del clear BM, ordine del level-triggered) → clear
    /// INTR/ERROR → check. `false` = errore ATA o BM (fallback PIO).
    fn finish(&self, disk: &super::block::AtaDisk, chan: u16) -> bool {
        // Cattura ERROR prima del clear (dopo leggerebbe 0).
        let bs = unsafe { io::inb(self.bm_status(chan)) };
        self.stop(chan);
        let ata = disk.task_status();
        unsafe {
            io::outb(self.bm_status(chan), BM_ST_ERROR | BM_ST_INTR);
        }
        if bs & BM_ST_ERROR != 0 {
            return false;
        }
        ata & 0x01 == 0
    }

    /// Attesa completamento a poll boundato del BM status (NON blocking-recv:
    /// un `recv` tra richiesta e reply clobbererebbe la reply implicita del
    /// kernel (`pop_msg` riscrive `reply_chan` anche per le notify IRQ con
    /// `req_id == 0`), perdendo la reply a userfs. Le notify IRQ restano solo
    /// osservabilita' (drenate a testa-loop) + futuro 38.2. Poll come il PIO
    /// (`wait_not_busy`), ma su UNA porta invece che per-word: il timeout
    /// degrada a fallback PIO (mai wedge oltre il bound).
    fn wait_done(&self, chan: u16) -> bool {
        for _ in 0..DMA_TIMEOUT {
            if self.is_done(chan) {
                return true;
            }
            core::hint::spin_loop();
        }
        false
    }

    /// Trasferisce `n` settori (1..=7) a `lba` fisico via DMA: `write` = mem→
    /// disco (`buf` sorgente) o disco→mem (`buf` destinazione, n*512 byte).
    /// `disk` = taskfile, `chan` = offset BM del canale. Ritorna false =
    /// fallback PIO dal chiamante (stesso contratto del PIO).
    pub fn transfer(
        &mut self,
        disk: &super::block::AtaDisk,
        chan: u16,
        lba: u64,
        n: usize,
        buf: &mut [u8],
        write: bool,
    ) -> bool {
        if n == 0 || n > 7 || buf.len() < n * 512 || !disk.lba48 {
            self.note(false);
            return false;
        }
        let data_phys = self.staging_phys + DATA_OFF;
        let staging = self.staging_va as *mut u8;
        if build_prd(staging, data_phys, n * 512).is_none() {
            self.note(false);
            return false;
        }
        if write {
            unsafe {
                core::ptr::copy_nonoverlapping(buf.as_ptr(), staging.add(DATA_OFF as usize), n * 512);
            }
        }
        if !self.wait_idle(chan) {
            self.note(false);
            return false;
        }
        unsafe {
            io::outl(self.bmiba + chan + BM_PRD_OFF, self.staging_phys as u32);
            io::outb(self.bm_status(chan), BM_ST_ERROR | BM_ST_INTR);
        }
        if !disk.start_dma_ext(lba, n as u8, write) {
            self.note(false);
            return false;
        }
        unsafe {
            let c = io::inb(self.bm_cmd(chan));
            io::outb(
                self.bm_cmd(chan),
                (c & !BM_CMD_START) | BM_CMD_START | if write { 0 } else { BM_CMD_RW },
            );
        }
        if !self.wait_done(chan) {
            self.note(false);
            return false;
        }
        let ok = self.finish(disk, chan);
        if ok && write && !disk.flush_write_cache() {
            self.note(false);
            return false;
        }
        if ok && !write {
            unsafe {
                core::ptr::copy_nonoverlapping(
                    staging.add(DATA_OFF as usize),
                    buf.as_mut_ptr(),
                    n * 512,
                );
            }
        }
        self.note(ok);
        ok
    }
}
