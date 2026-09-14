//! Client disco via IPC verso `userdisk` (Fase 16).
//!
//! Implementa `BlockSource` per il parser FAT32 sopra il protocollo `DISK_*`
//! su canale diretto (service_lookup(Disk)): `HELLO` per handshake (i fisici
//! viaggiano nella reply: w0 = req_phys inutilizzato qui, w1 = resp_phys che
//! mappiamo a `DISK_RESP_VA`), poi `READ` settoriali con `send` sincrona e
//! frame risposta letto dalla finestra mappata.
//!
//! Riconnessione (init-restart): il canale e' invalidato alla morte di
//! userdisk (`note_peer_death` su EXIT_NOTIFY, o send fallita) e il prossimo
//! read rifa' lookup + HELLO + remap (bound, mai wedge). Il remap riallinea
//! tail=head: il contenuto appartiene all'epoca morta e l'op e' ritentata dal
//! chiamante. `userdisk` non richiama mai `userfs`: la send sincrona non crea
//! cicli (stesso argomento dei relay verso devfs/console).
//!
//! Single-threaded per costruzione (userfs e' monolitico): `Cell` basta, mai
//! rientranza (la send blocca senza eseguire altro codice).

use core::cell::Cell;

use crate::fat32::BlockSource;
use libr::println;

/// Handshake: chiede i fisici dei ring DISK (reply w0=req_phys, w1=resp_phys).
const DISK_HELLO: u64 = 0x50;
/// Valida il nodo di mount (w0 = handle codificato).
const DISK_OPEN: u64 = 0x51;
/// Legge un settore (w0 = handle, w1 = lba): frame [512:8][0:8][settore].
const DISK_READ: u64 = 0x52;

/// Finestra del response ring di userdisk (stessa VA del server: ogni processo
/// ha le proprie page table, nessun conflitto).
const DISK_RESP_VA: u64 = 0x0000_4000_0025_0000;
const RING_DATA_CAP: usize = 4088;
const RING_HEAD: usize = 0xFF8;
const RING_TAIL: usize = 0xFFC;

const ERR: u64 = !0u64;
/// Bound attesa userdisk a boot/restart (~5 s, come `wait_ready` di init).
const HELLO_BOUND_TICKS: i64 = 500;

pub struct IpcDisk {
    /// Handle nodo di mount codificato (disco<<16|sub): 0 = sda whole-disk.
    handle: u32,
    /// Canale diretto verso userdisk (None = da riconnettere).
    chan: Cell<Option<u64>>,
}

impl IpcDisk {
    pub fn new(handle: u32) -> Self {
        Self { handle, chan: Cell::new(None) }
    }

    /// Segnala la morte di un peer (EXIT_NOTIFY): se e' userdisk, invalida il
    /// canale — il prossimo read riconnette. Solo un compare, mai reply.
    pub fn note_peer_death(&self, dead_chan: u64) {
        if self.chan.get() == Some(dead_chan) {
            self.chan.set(None);
        }
    }

    /// Legge un response frame DISK dalla finestra mappata (consumer SPSC:
    /// si legge a `tail`, il producer avanza `head`) e ne copia il settore in
    /// `out`. Ritorna false se il ring e' vuoto o il frame non e' un settore
    /// valido (resync difensivo: single-writer sequenziale, non dovrebbe mai
    /// accadere).
    unsafe fn frame_read(out: &mut [u8; 512]) -> bool {
        unsafe {
            let head = core::ptr::read_volatile((DISK_RESP_VA + RING_HEAD as u64) as *const u32);
            let tail = core::ptr::read_volatile((DISK_RESP_VA + RING_TAIL as u64) as *const u32);
            if head == tail {
                return false;
            }
            let base = DISK_RESP_VA as *const u8;
            let t = tail as usize;
            let mut hdr = [0u8; 16];
            for i in 0..16 {
                hdr[i] = core::ptr::read_volatile(base.add((t + i) % RING_DATA_CAP));
            }
            let res = u64::from_le_bytes(hdr[0..8].try_into().unwrap_or([0xFF; 8]));
            if res != 512 {
                core::ptr::write_volatile(
                    (DISK_RESP_VA + RING_TAIL as u64) as *mut u32,
                    head,
                );
                return false;
            }
            for i in 0..512 {
                out[i] = core::ptr::read_volatile(base.add((t + 16 + i) % RING_DATA_CAP));
            }
            let new_tail = (t + 16 + 512) % RING_DATA_CAP;
            core::ptr::write_volatile((DISK_RESP_VA + RING_TAIL as u64) as *mut u32, new_tail as u32);
            true
        }
    }

    /// Assicura la connessione (lookup + HELLO + OPEN + map). Fast path: una
    /// Cell-letura. Ritorna il canale o None oltre il bound.
    fn ensure(&self) -> Option<u64> {
        if let Some(c) = self.chan.get() {
            return Some(c);
        }
        let t0 = libr::get_ticks();
        loop {
            if let Ok(c) = libr::service_lookup(libr::Service::Disk) {
                let cu = c as u64;
                let ok = match libr::send(cu, DISK_HELLO, 0, 0) {
                    Ok(rep) if rep.w0 != ERR => {
                        if libr::map_physical(rep.w1, DISK_RESP_VA, 1).is_err() {
                            false
                        } else {
                            // Riallinea: scarta l'epoca morta (l'op e' ritentata).
                            unsafe {
                                let head = core::ptr::read_volatile(
                                    (DISK_RESP_VA + RING_HEAD as u64) as *const u32,
                                );
                                core::ptr::write_volatile(
                                    (DISK_RESP_VA + RING_TAIL as u64) as *mut u32,
                                    head,
                                );
                            }
                            // Valida il nodo di mount (es. sda presente?).
                            matches!(
                                libr::send(cu, DISK_OPEN, self.handle as u64, 0),
                                Ok(rep) if rep.w0 != ERR
                            )
                        }
                    }
                    _ => false,
                };
                if ok {
                    println!("[userfs] userdisk connesso (chan {})", cu);
                    self.chan.set(Some(cu));
                    return Some(cu);
                }
                // Trovato ma HELLO/OPEN falliti (restart in corso?): riprova.
            }
            if libr::get_ticks() - t0 > HELLO_BOUND_TICKS {
                return None;
            }
            for _ in 0..100_000 {
                core::hint::spin_loop();
            }
        }
    }

    /// Un tentativo di lettura (nessun retry qui: lo fa il chiamante).
    fn try_read(&self, chan: u64, lba: u64, buf: &mut [u8; 512]) -> bool {
        match libr::send(chan, DISK_READ, self.handle as u64, lba) {
            Ok(rep) => {
                if rep.w0 == ERR {
                    return false;
                }
                let ok = unsafe { Self::frame_read(buf) };
                if !ok {
                }
                ok
            }
            Err(_) => {
                // userdisk morto durante la send: invalida, il chiamante ritenta.
                self.chan.set(None);
                false
            }
        }
    }
}

impl BlockSource for IpcDisk {
    fn read_sector(&self, lba: u64, buf: &mut [u8; 512]) -> bool {
        let chan = match self.ensure() {
            Some(c) => c,
            None => return false,
        };
        if self.try_read(chan, lba, buf) {
            return true;
        }
        // Solo se il canale e' caduto (non su errore IO vero): riconnetti e
        // ritenta UNA volta. La send fallita ha gia' invalidato il canale.
        if self.chan.get().is_some() {
            return false;
        }
        let chan = match self.ensure() {
            Some(c) => c,
            None => return false,
        };
        self.try_read(chan, lba, buf)
    }
}
