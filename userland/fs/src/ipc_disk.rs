//! Client disco via IPC verso `userdisk` (Fase 16, resolve Fase 16c).
//!
//! Implementa `BlockSource` per il parser FAT32 sopra il protocollo `DISK_*`
//! su canale diretto (service_lookup(Disk)): `HELLO` per handshake (i fisici
//! viaggiano nella reply: w0 = req_phys, w1 = resp_phys), poi `READ` settoriali
//! con `send` sincrona e frame risposta letto dalla finestra mappata.
//!
//! Risoluzione nomi (16c): userdisk e' la single source of truth della mappa
//! nome→handle. `resolve(name)` scrive un frame `[namelen:8][name]` nel
//! DISK_REQ ring (mappato a `DISK_REQ_VA`, stessa VA di userdisk: page table
//! per-processo, nessun conflitto) e manda `DISK_RESOLVE`; l'handle torna in
//! w0 di reply (ERR = sconosciuto). userfs non indovina piu' nulla dal nome.
//!
//! Riconnessione (init-restart): il canale e' invalidato alla morte di
//! userdisk (`note_peer_death` su EXIT_NOTIFY, o send fallita) e il prossimo
//! read/resolve rifa' lookup + HELLO + remap (bound, mai wedge). Il remap
//! riallinea entrambi i ring: il contenuto appartiene all'epoca morta e l'op
//! e' ritentata dal chiamante. `userdisk` non richiama mai `userfs`: le send
//! sincrone non creano cicli (stesso argomento dei relay verso devfs/console).
//!
//! Single-threaded per costruzione (userfs e' monolitico): `Cell` basta, mai
//! rientranza (la send blocca senza eseguire altro codice).

use core::cell::Cell;

use crate::fat32::BlockSource;
use libr::println;
/// Tag DISK_* (single source in `syscall-numbers`, Fase 16c): handshake,
/// validazione nodo, lettura settoriale, resolve nome→handle di proprieta'
/// del driver.
use libr::{DISK_HELLO, DISK_OPEN, DISK_READ, DISK_RESOLVE, DISK_WRITE};

/// Finestra del request ring di userdisk (stessa VA del server: ogni processo
/// ha le proprie page table, nessun conflitto). userfs e' l'unico writer.
const DISK_REQ_VA: u64 = 0x0000_4000_0024_0000;
/// Finestra del response ring di userdisk (stessa VA del server: ogni processo
/// ha le proprie page table, nessun conflitto).
const DISK_RESP_VA: u64 = 0x0000_4000_0025_0000;
/// Bound nomi di resolve (deve combaciare con `DISK_MAX_NAME` di userdisk).
const DISK_MAX_NAME: usize = 16;
// Geometria ring + errore IPC (A1): single source in `libr`.
use libr::{ERR, RING_DATA_CAP, RING_HEAD, RING_TAIL};
/// Bound attesa userdisk a boot/restart (~5 s, come `wait_ready` di init).
const HELLO_BOUND_TICKS: i64 = 500;
/// P1.2 — settori max per IPC DISK (bound del ring: 8 + 7*512 = 3592 nella
/// request, 16 + 7*512 = 3600 nella response, entrambi < 4087).
const DISK_MAX_SECTORS: usize = 7;

pub struct IpcDisk {
    /// Handle nodo di mount codificato (disco<<16|sub): 0 = sda whole-disk.
    handle: u32,
    /// Canale diretto verso userdisk (None = da riconnettere).
    chan: Cell<Option<u64>>,
    /// Nodo validato con DISK_OPEN sulla connessione corrente (Fase 21: prima
    /// si faceva OPEN a OGNI settore — 2 round-trip per settore invece di 1.
    /// L'OPEN ora e' una tantum per connessione: la tabella nodi del driver e'
    /// statica a driver vivo, e a morte driver il canale cade (qui sotto) e
    /// la riconnessione rivalida. Mai stale silenzioso.)
    open_ok: Cell<bool>,
}

impl IpcDisk {
    pub fn new(handle: u32) -> Self {
        Self { handle, chan: Cell::new(None), open_ok: Cell::new(false) }
    }

    /// Dimentica la connessione (canale caduto o epoca cambiata): il prossimo
    /// uso riconnette e rivalida (OPEN) da zero.
    fn drop_conn(&self) {
        self.chan.set(None);
        self.open_ok.set(false);
    }

    /// Segnala la morte di un peer (EXIT_NOTIFY): se e' userdisk, invalida il
    /// canale — il prossimo read/resolve riconnette. Ritorna true se eravamo
    /// connessi (cambio d'epoca: il chiamante userfs droppa le istanze FAT
    /// attive, gli handle possono cambiare — re-resolve per nome al prossimo
    /// accesso, Fase 16c).
    pub fn note_peer_death(&self, dead_chan: u64) -> bool {
        if self.chan.get() == Some(dead_chan) {
            self.drop_conn();
            true
        } else {
            false
        }
    }

    /// Legge un response frame DISK dalla finestra mappata (consumer SPSC:
    /// si legge a `tail`, il producer avanza `head`) e ne copia `expect` byte
    /// in `out`. P1.2 — l'header porta la lunghezza (`len == expect`, prima
    /// era fissa a 512): resync difensivo come prima a mismatch.
    unsafe fn frame_read(out: &mut [u8], expect: usize) -> bool {
        if out.len() < expect {
            return false;
        }
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
            if res != expect as u64 {
                core::ptr::write_volatile(
                    (DISK_RESP_VA + RING_TAIL as u64) as *mut u32,
                    head,
                );
                return false;
            }
            for i in 0..expect {
                out[i] = core::ptr::read_volatile(base.add((t + 16 + i) % RING_DATA_CAP));
            }
            let new_tail = (t + 16 + expect) % RING_DATA_CAP;
            core::ptr::write_volatile((DISK_RESP_VA + RING_TAIL as u64) as *mut u32, new_tail as u32);
            true
        }
    }

    /// Connessione (lookup + HELLO + map di ENTRAMBI i ring, bound, mai wedge).
    /// Fast path: una Cell-lettura. NON valida il nodo (serve a `resolve`,
    /// che l'handle non ce l'ha ancora). Epoca fresca: azzera il DISK_REQ
    /// (unico writer: niente in volo) e riallinea il DISK_RESP (tail=head).
    fn connect(&self) -> Option<u64> {
        if let Some(c) = self.chan.get() {
            return Some(c);
        }
        let t0 = libr::get_ticks();
        loop {
            if let Ok(c) = libr::service_lookup(libr::Service::Disk) {
                let cu = c as u64;
                let ok = match libr::send(cu, DISK_HELLO, 0, 0) {
                    Ok(rep) if rep.w0 != ERR => {
                        if libr::map_physical(rep.w0, DISK_REQ_VA, 1).is_err()
                            || libr::map_physical(rep.w1, DISK_RESP_VA, 1).is_err()
                        {
                            false
                        } else {
                            // Epoca fresca: scarta l'epoca morta (l'op e' ritentata).
                            unsafe {
                                core::ptr::write_volatile(
                                    (DISK_REQ_VA + RING_HEAD as u64) as *mut u32,
                                    0,
                                );
                                core::ptr::write_volatile(
                                    (DISK_REQ_VA + RING_TAIL as u64) as *mut u32,
                                    0,
                                );
                                let head = core::ptr::read_volatile(
                                    (DISK_RESP_VA + RING_HEAD as u64) as *const u32,
                                );
                                core::ptr::write_volatile(
                                    (DISK_RESP_VA + RING_TAIL as u64) as *mut u32,
                                    head,
                                );
                            }
                            true
                        }
                    }
                    _ => false,
                };
                if ok {
                    println!("[userfs] userdisk connesso (chan {})", cu);
                    self.chan.set(Some(cu));
                    return Some(cu);
                }
                // Trovato ma HELLO fallito (restart in corso?): riprova.
            }
            if libr::get_ticks() - t0 > HELLO_BOUND_TICKS {
                return None;
            }
            for _ in 0..100_000 {
                core::hint::spin_loop();
            }
        }
    }

    /// Assicura connessione + nodo validato (lookup + HELLO + OPEN, bound).
    /// Fast path: canale cachato e nodo gia' validato (una Cell-lettura: niente
    /// IPC). L'OPEN e' deterministico (`locate` su tabelle statiche): un solo
    /// tentativo per connessione — fallisce solo a handle stale (re-resolve
    /// del chiamante) o morte del driver durante la send (una riconnessione e
    /// un retry, come i read).
    fn ensure(&self) -> Option<u64> {
        let cu = self.connect()?;
        if self.open_ok.get() {
            return Some(cu);
        }
        match libr::send(cu, DISK_OPEN, self.handle as u64, 0) {
            Ok(rep) if rep.w0 != ERR => {
                self.open_ok.set(true);
                Some(cu)
            }
            Ok(_) => None, // handle stale: il chiamante re-risolve per nome
            Err(_) => {
                self.drop_conn();
                let cu = self.connect()?;
                match libr::send(cu, DISK_OPEN, self.handle as u64, 0) {
                    Ok(rep) if rep.w0 != ERR => {
                        self.open_ok.set(true);
                        Some(cu)
                    }
                    _ => None,
                }
            }
        }
    }

    /// Un tentativo di lettura multi (nessun retry qui: lo fa il chiamante).
    /// P1.2 — frame di richiesta `[count:8]` (1..=7), risposta con `n*512`
    /// byte: 1 IPC invece di n.
    fn try_read_multi(&self, chan: u64, lba: u64, n: usize, out: &mut [u8]) -> bool {
        if n == 0 || n > DISK_MAX_SECTORS || out.len() < n * 512 {
            return false;
        }
        if !unsafe { Self::req_write_count(n) } {
            return false;
        }
        match libr::send(chan, DISK_READ, self.handle as u64, lba) {
            Ok(rep) => {
                if rep.w0 == ERR {
                    return false;
                }
                let ok = unsafe { Self::frame_read(out, n * 512) };
                if !ok {
                }
                ok
            }
            Err(_) => {
                // userdisk morto durante la send: invalida, il chiamante ritenta.
                self.drop_conn();
                false
            }
        }
    }

    /// Scrive un frame di richiesta read `[count:8]` nel DISK_REQ ring
    /// (P1.2). Ritorna false se non c'e' spazio (disciplina sync + reset a
    /// ogni connessione: non dovrebbe mai accadere).
    unsafe fn req_write_count(n: usize) -> bool {
        unsafe {
            let head = core::ptr::read_volatile((DISK_REQ_VA + RING_HEAD as u64) as *const u32);
            let tail = core::ptr::read_volatile((DISK_REQ_VA + RING_TAIL as u64) as *const u32);
            let used = (head.wrapping_sub(tail)) % RING_DATA_CAP as u32;
            if RING_DATA_CAP as u32 - used < (8 + 1) as u32 {
                return false;
            }
            let dst = DISK_REQ_VA as *mut u8;
            let count_b = (n as u64).to_le_bytes();
            for (i, byte) in count_b.iter().enumerate() {
                core::ptr::write_volatile(dst.add(((head as usize) + i) % RING_DATA_CAP), *byte);
            }
            let new_head = ((head as usize) + 8) % RING_DATA_CAP;
            core::ptr::write_volatile((DISK_REQ_VA + RING_HEAD as u64) as *mut u32, new_head as u32);
            true
        }
    }

    /// Scrive un frame di write `[count:8][settori]` nel DISK_REQ ring
    /// (P1.2, generalizza il vecchio `[512:8][settore]`). Ritorna false se
    /// non c'e' spazio o count fuori bound.
    unsafe fn req_write_sectors(n: usize, data: &[u8]) -> bool {
        if n == 0 || n > DISK_MAX_SECTORS || data.len() < n * 512 {
            return false;
        }
        unsafe {
            let head = core::ptr::read_volatile((DISK_REQ_VA + RING_HEAD as u64) as *const u32);
            let tail = core::ptr::read_volatile((DISK_REQ_VA + RING_TAIL as u64) as *const u32);
            let used = (head.wrapping_sub(tail)) % RING_DATA_CAP as u32;
            if RING_DATA_CAP as u32 - used < (8 + n * 512 + 1) as u32 {
                return false;
            }
            let dst = DISK_REQ_VA as *mut u8;
            let count_b = (n as u64).to_le_bytes();
            for (i, byte) in count_b.iter().enumerate() {
                core::ptr::write_volatile(dst.add(((head as usize) + i) % RING_DATA_CAP), *byte);
            }
            for (i, byte) in data[..n * 512].iter().enumerate() {
                core::ptr::write_volatile(dst.add(((head as usize) + 8 + i) % RING_DATA_CAP), *byte);
            }
            let new_head = ((head as usize) + 8 + n * 512) % RING_DATA_CAP;
            core::ptr::write_volatile((DISK_REQ_VA + RING_HEAD as u64) as *mut u32, new_head as u32);
            true
        }
    }

    /// Un tentativo di scrittura multi (P1.2, nessun retry qui: lo fa il
    /// chiamante). Reply senza frame: w0 = 0 ok, ERR fallito (canale intatto:
    /// niente retry). Send fallita = driver morto: invalida.
    fn try_write_multi(&self, chan: u64, lba: u64, n: usize, data: &[u8]) -> bool {
        if !unsafe { Self::req_write_sectors(n, data) } {
            return false;
        }
        match libr::send(chan, DISK_WRITE, self.handle as u64, lba) {
            Ok(rep) => rep.w0 != ERR,
            Err(_) => {
                self.drop_conn();
                false
            }
        }
    }

    /// Scrive un frame di resolve `[namelen:8][name]` nel DISK_REQ ring.
    /// Ritorna false se non c'e' spazio (disciplina sync + reset a ogni
    /// connessione: non dovrebbe mai accadere; il chiamante fallisce loud).
    unsafe fn req_write_name(name: &str) -> bool {
        let bytes = name.as_bytes();
        let frame_len = 8 + bytes.len();
        unsafe {
            let head = core::ptr::read_volatile((DISK_REQ_VA + RING_HEAD as u64) as *const u32);
            let tail = core::ptr::read_volatile((DISK_REQ_VA + RING_TAIL as u64) as *const u32);
            let used = (head.wrapping_sub(tail)) % RING_DATA_CAP as u32;
            if RING_DATA_CAP as u32 - used < frame_len as u32 + 1 {
                return false;
            }
            let dst = DISK_REQ_VA as *mut u8;
            let len_b = (bytes.len() as u64).to_le_bytes();
            for (i, byte) in len_b.iter().enumerate() {
                core::ptr::write_volatile(dst.add(((head as usize) + i) % RING_DATA_CAP), *byte);
            }
            for (i, byte) in bytes.iter().enumerate() {
                core::ptr::write_volatile(dst.add(((head as usize) + 8 + i) % RING_DATA_CAP), *byte);
            }
            let new_head = ((head as usize) + frame_len) % RING_DATA_CAP;
            core::ptr::write_volatile((DISK_REQ_VA + RING_HEAD as u64) as *mut u32, new_head as u32);
            true
        }
    }

    /// Un tentativo di resolve (nessun retry qui: lo fa il chiamante).
    /// Reply w0 = handle (0 e' valido: sda whole-disk), ERR = nome sconosciuto
    /// (canale intatto: niente retry). Send fallita = driver morto: invalida.
    fn try_resolve(&self, chan: u64, name: &str) -> Option<u32> {
        if !unsafe { Self::req_write_name(name) } {
            return None;
        }
        match libr::send(chan, DISK_RESOLVE, 0, 0) {
            Ok(rep) => {
                if rep.w0 == ERR {
                    None
                } else {
                    Some(rep.w0 as u32)
                }
            }
            Err(_) => {
                self.drop_conn();
                None
            }
        }
    }

    /// Risolve un nome nodo corto ("sda", "sda1") in handle presso userdisk
    /// (Fase 16c: single source of truth nel driver). Bound, mai wedge.
    /// Solo se il canale e' caduto (non su nome sconosciuto): riconnetti e
    /// ritenta UNA volta. Usata dai mount (l'istanza e' usa-e-getta: l'handle
    /// per l'I/O vive poi nel `FsMount` + `IpcDisk` dedicati).
    pub fn resolve(&self, name: &str) -> Option<u32> {
        if name.is_empty() || name.len() > DISK_MAX_NAME {
            return None;
        }
        let chan = self.connect()?;
        if let Some(h) = self.try_resolve(chan, name) {
            return Some(h);
        }
        // Nome sconosciuto (canale intatto): errore legittimo, niente retry.
        if self.chan.get().is_some() {
            return None;
        }
        let chan = self.connect()?;
        self.try_resolve(chan, name)
    }
}

impl BlockSource for IpcDisk {
    fn read_sector(&self, lba: u64, buf: &mut [u8; 512]) -> bool {
        self.read_sectors(lba, 1, buf)
    }

    /// Scrive un settore via DISK_WRITE (Fase 20): stessa disciplina del read
    /// (un retry solo a canale caduto, mai su errore IO vero).
    fn write_sector(&self, lba: u64, data: &[u8; 512]) -> bool {
        self.write_sectors(lba, 1, data)
    }

    /// P1.2 — run di `n` settori in chunk da ≤7 IPC (stessa disciplina del
    /// singolo: un retry solo a canale caduto). `out` lungo almeno `n*512`.
    fn read_sectors(&self, lba: u64, n: usize, out: &mut [u8]) -> bool {
        if out.len() < n * 512 {
            return false;
        }
        let mut done = 0usize;
        while done < n {
            let k = (n - done).min(DISK_MAX_SECTORS);
            let chan = match self.ensure() {
                Some(c) => c,
                None => return false,
            };
            let ok = self.try_read_multi(chan, lba + done as u64, k, &mut out[done * 512..(done + k) * 512]);
            if ok {
                done += k;
                continue;
            }
            // Solo se il canale e' caduto (non su errore IO vero): riconnetti
            // e ritenta UNA volta. La send fallita ha gia' invalidato il canale.
            if self.chan.get().is_some() {
                return false;
            }
            let chan = match self.ensure() {
                Some(c) => c,
                None => return false,
            };
            if !self.try_read_multi(chan, lba + done as u64, k, &mut out[done * 512..(done + k) * 512]) {
                return false;
            }
            done += k;
        }
        true
    }

    /// P1.2 — come `read_sectors` in scrittura (1 comando PIO + 1 flush per
    /// chunk nel driver).
    fn write_sectors(&self, lba: u64, n: usize, data: &[u8]) -> bool {
        if data.len() < n * 512 {
            return false;
        }
        let mut done = 0usize;
        while done < n {
            let k = (n - done).min(DISK_MAX_SECTORS);
            let chan = match self.ensure() {
                Some(c) => c,
                None => return false,
            };
            let ok = self.try_write_multi(chan, lba + done as u64, k, &data[done * 512..(done + k) * 512]);
            if ok {
                done += k;
                continue;
            }
            if self.chan.get().is_some() {
                return false;
            }
            let chan = match self.ensure() {
                Some(c) => c,
                None => return false,
            };
            if !self.try_write_multi(chan, lba + done as u64, k, &data[done * 512..(done + k) * 512]) {
                return false;
            }
            done += k;
        }
        true
    }
}
