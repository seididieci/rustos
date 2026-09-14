//! userdisk — Driver disco ATA in userspace (Fase 16).
//!
//! Possiede le porte ATA primario + secondario (via `io_ranges`, TSS
//! per-processo ADR-0006), rileva i dischi presenti via `IDENTIFY`
//! (`detect.rs`), parsa le partizioni MBR primarie (`part.rs`) ed espone ogni
//! nodo come `/dev/sdX` (`FS_REGISTER` per nodo, solo i presenti) + servizio
//! `Disk` per il data-plane verso userfs.
//!
//! REGOLA ANTI-DEADLOCK (lezione Fase 15 + ciclo userfs↔userdisk osservato in
//! Fase 16.2): userdisk non fa MAI `send` sincrona verso userfs — nemmeno
//! l'handshake `fs_init` di libr (sincrono). E' client FS PURAMENTE async:
//! ring propri allocati raw, `FS_BUF_REG` + `R_REGISTER` via `send_async` con
//! collect per req_id nel loop (state machine come tty). userfs fa solo send
//! sincrone verso userdisk, e userdisk drena sempre (mai bloccato su userfs):
//! nessun ciclo possibile, in nessuna direzione, a boot come a restart.
//!
//! Due protocolli serviti, entrambi con reply implicita (ADR-0008):
//! - `DISK_*` (canale diretto userfs→userdisk, service_lookup(Disk)): HELLO
//!   (fisici nelle reply: w0 = req_phys del DISK_REQ ring, w1 = resp_phys),
//!   OPEN/READ settoriali, CLOSE, RESOLVE nome→handle (Fase 16c: userdisk e'
//!   l'unico proprietario della mappa nomi; userfs non indovina piu' nulla).
//!   Un settore per chiamata (1:1 con BlockSource).
//! - `DEV_*` (relay userfs per gli open raw `/dev/sdX`): OPEN(w0=handle
//!   codificato disco<<16|sub), READ sequenziale con posizione per-fd (solo
//!   multipli di 512), WRITE sempre ERR (read-only), CLOSE, READDIR vuota.
//!
//! Boot: detection (solo HW) → ring FS+DISK → `service_register(Disk)` →
//! SVC_READY al parent SUBITO (userdisk parte PRIMA di userfs: come console,
//! l'ACK non aspetta nulla) → loop (la registrazione FS avanza da sola via SM
//! appena userfs esiste).

#![no_std]
#![no_main]

extern crate alloc;

mod block;
mod detect;
mod io;
mod part;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use libr::println;

// ── IPC tags ────────────────────────────────────────────────────────

const DEV_OPEN: u64 = 0x20;
const DEV_READ: u64 = 0x21;
const DEV_WRITE: u64 = 0x22;
const DEV_CLOSE: u64 = 0x23;
const DEV_READDIR: u64 = 0x24;

/// Handshake data-plane: userfs chiede i fisici dei ring DISK.
/// Reply: w0 = req_phys (anello delle richieste di resolve, Fase 16c),
/// w1 = resp_phys (mappato da userfs per leggere i frame). Niente frame:
/// i fisici stanno nei registri.
use libr::DISK_HELLO;
/// Valida un nodo (w0 = handle codificato). Reply OK/ERR, niente frame.
use libr::DISK_OPEN;
/// Legge UN settore (w0 = handle, w1 = lba nel nodo).
/// Frame: [512:8][0:8][settore]. Fuori range/errore → reply ERR, niente frame.
use libr::DISK_READ;
/// Chiude (stateless: sempre OK, frame vuoto).
use libr::DISK_CLOSE;
/// Risolve un nome nodo ("sda", "sda1") in handle (Fase 16c, single source
/// of truth nel driver). Richiesta: frame `[namelen:8][name]` nel DISK_REQ
/// ring; reply w0 = handle o ERR, niente frame.
use libr::DISK_RESOLVE;

// ── Ring I/O ────────────────────────────────────────────────────────
// Due coppie SEPARATE (lezione CLI_* del fix kbd/tty: mai protocolli diversi
// nello stesso ring):
// - FS_REQ_VA/FS_RESP_VA (propri): traffico FS (FS_BUF_REG + FS_REGISTER).
//   Mai iniettati da nessuno: niente remap, mai sovrascritti. Per i relay DEV
//   in ingresso userfs mappa i ring del client nelle finestre CLI_* dedicate.
// - DISK_REQ_VA/DISK_RESP_VA: data-plane DISK_* con userfs (fisso, noto a
//   userfs via HELLO). Libere nella mappa user (CLI fino a +0x23..., heap da
//   +0x400000).

/// Request/response ring FS propri (stesse VA di libr: page table per-processo,
/// nessun conflitto — e userdisk non usa il machinery FS di libr).
const FS_REQ_VA: u64 = 0x0000_4000_0020_0000;
const FS_RESP_VA: u64 = 0x0000_4000_0021_0000;
const CLI_REQ: u64 = libr::CLI_REQ_VA;
const CLI_RESP: u64 = libr::CLI_RESP_VA;
const DISK_REQ_VA: u64 = 0x0000_4000_0024_0000;
const DISK_RESP_VA: u64 = 0x0000_4000_0025_0000;
const RING_DATA_CAP: usize = 4088;
const RING_HEAD: usize = 0xFF8;
const RING_TAIL: usize = 0xFFC;

const ERR: u64 = !0u64;

/// Scrive un response frame `[result:8][w1:8][payload]` nel ring DISK.
unsafe fn disk_resp_write(result: u64, w1: u64, payload: &[u8]) {
    let frame_len = 16 + payload.len();
    unsafe {
        let head = core::ptr::read_volatile((DISK_RESP_VA + RING_HEAD as u64) as *const u32);
        let mut hdr = [0u8; 16];
        hdr[0..8].copy_from_slice(&result.to_le_bytes());
        hdr[8..16].copy_from_slice(&w1.to_le_bytes());
        let dst = DISK_RESP_VA as *mut u8;
        for (i, byte) in hdr.iter().enumerate() {
            let p = ((head as usize) + i) % RING_DATA_CAP;
            core::ptr::write_volatile(dst.add(p), *byte);
        }
        for (i, byte) in payload.iter().enumerate() {
            let p = ((head as usize) + 16 + i) % RING_DATA_CAP;
            core::ptr::write_volatile(dst.add(p), *byte);
        }
        let new_head = ((head as usize) + frame_len) % RING_DATA_CAP;
        core::ptr::write_volatile((DISK_RESP_VA + RING_HEAD as u64) as *mut u32, new_head as u32);
    }
}

/// Scrive dati nella response ring del client relay (a CLI_RESP, come kbd).
unsafe fn resp_ring_write_client(data: &[u8]) {
    let frame_len = 16 + data.len();
    unsafe {
        let head = core::ptr::read_volatile((CLI_RESP + RING_HEAD as u64) as *const u32);
        let mut hdr = [0u8; 16];
        hdr[0..8].copy_from_slice(&(data.len() as u64).to_le_bytes());
        hdr[8..16].copy_from_slice(&0u64.to_le_bytes());
        let dst = CLI_RESP as *mut u8;
        for (i, byte) in hdr.iter().enumerate() {
            let p = ((head as usize) + i) % RING_DATA_CAP;
            core::ptr::write_volatile(dst.add(p), *byte);
        }
        for (i, byte) in data.iter().enumerate() {
            let p = ((head as usize) + 16 + i) % RING_DATA_CAP;
            core::ptr::write_volatile(dst.add(p), *byte);
        }
        let new_head = ((head as usize) + frame_len) % RING_DATA_CAP;
        core::ptr::write_volatile((CLI_RESP + RING_HEAD as u64) as *mut u32, new_head as u32);
    }
}

/// Lunghezza massima del nome nodo in un frame di resolve ("sda1" = 4;
/// bound difensivo: oltre e' spazzatura di un'epoca morta).
const DISK_MAX_NAME: usize = 16;

/// Legge un frame di resolve `[namelen:8][name]` dal DISK_REQ ring e lo
/// consuma (SPSC: si legge a `tail`, il producer userfs avanza `head`).
/// Ritorna il nome o None a ring vuoto/frame malformato (resync tail=head:
/// il mittente scrive il frame intero prima di notificare, quindi un frame
/// incompleto appartiene a un'epoca morta — stessa invariante dei ring FS).
fn disk_req_read_name() -> Option<String> {
    unsafe {
        let head = core::ptr::read_volatile((DISK_REQ_VA + RING_HEAD as u64) as *const u32);
        let tail = core::ptr::read_volatile((DISK_REQ_VA + RING_TAIL as u64) as *const u32);
        let avail = (head.wrapping_sub(tail)) % RING_DATA_CAP as u32;
        if avail < 8 {
            return None;
        }
        let src = DISK_REQ_VA as *const u8;
        let t = tail as usize;
        let mut len_b = [0u8; 8];
        for i in 0..8 {
            len_b[i] = core::ptr::read_volatile(src.add((t + i) % RING_DATA_CAP));
        }
        let len = u64::from_le_bytes(len_b) as usize;
        if len == 0 || len > DISK_MAX_NAME || avail < (8 + len) as u32 {
            core::ptr::write_volatile((DISK_REQ_VA + RING_TAIL as u64) as *mut u32, head);
            return None;
        }
        let mut name_b = [0u8; DISK_MAX_NAME];
        for i in 0..len {
            name_b[i] = core::ptr::read_volatile(src.add((t + 8 + i) % RING_DATA_CAP));
        }
        let new_tail = (t + 8 + len) % RING_DATA_CAP;
        core::ptr::write_volatile((DISK_REQ_VA + RING_TAIL as u64) as *mut u32, new_tail as u32);
        core::str::from_utf8(&name_b[..len]).ok().map(String::from)
    }
}

/// Consuma `count` byte di payload WRITE dal request ring del client relay
/// (avanza la tail di 20 + count): anche rifiutando la scrittura la tail va
/// avanzata o il prossimo request del client e' male (come devfs).
unsafe fn req_ring_consume_client(count: usize) {
    unsafe {
        let tail = core::ptr::read_volatile((CLI_REQ + RING_TAIL as u64) as *const u32);
        let new_tail = ((tail as usize) + 20 + count) % RING_DATA_CAP;
        core::ptr::write_volatile((CLI_REQ + RING_TAIL as u64) as *mut u32, new_tail as u32);
    }
}

// ── Nodi ────────────────────────────────────────────────────────────

/// Nodo esposto: whole-disk o partizione MBR (Fase 16c: la tabella e' la
/// single source of truth della mappa nome→handle; userfs risolve via
/// DISK_RESOLVE invece di ricalcolare l'handle dal nome).
struct Node {
    /// Nome breve ("sda", "sda1"): prefix registrato = "/dev/" + nome.
    name: String,
    /// Handle codificato (disco<<16|sub, 0 = whole-disk): allocato qui,
    /// mai indovinato altrove.
    handle: u32,
}

/// Partizione MBR (coordinate fisiche, dal parse del settore 0).
struct PartLoc {
    start: u32,
    sectors: u32,
}

/// Risolve un handle codificato (disco<<16|sub, 0 = whole-disk) in
/// (indice disco, base settori, settori nodo). userfs parsa i nomi Linux
/// ("sda"→(0,0), "sda1"→(0,1)) senza lista nodi; la validita' (quante
/// partizioni ha davvero il disco) e' qui. Ritorna None se inesistente.
fn locate(handle: u32, disk_sectors: &[u64], parts: &[Vec<PartLoc>]) -> Option<(usize, u64, u64)> {
    let disk = (handle >> 16) as usize;
    let sub = (handle & 0xFFFF) as usize;
    if disk >= disk_sectors.len() {
        return None;
    }
    if sub == 0 {
        return Some((disk, 0, disk_sectors[disk]));
    }
    let p = parts[disk].get(sub - 1)?;
    Some((disk, p.start as u64, p.sectors as u64))
}

/// Legge il settore `lba` del nodo `handle` (bound check sul nodo).
fn node_read(
    disks: &[block::AtaDisk],
    disk_sectors: &[u64],
    parts: &[Vec<PartLoc>],
    handle: u32,
    lba: u64,
    out: &mut [u8; 512],
) -> bool {
    let (disk, base, sectors) = match locate(handle, disk_sectors, parts) {
        Some(r) => r,
        None => return false,
    };
    if lba >= sectors {
        return false;
    }
    match disks.get(disk) {
        Some(d) => d.read_sector(base + lba, out),
        None => false,
    }
}

// ── Mount (registrazione FS puramente async) ────────────────────────
// Vedi doc in testa: MAI send sincrone verso userfs. Ring FS propri (allocati
// raw, mappati qui, mai iniettati da nessuno) + FS_BUF_REG / R_REGISTER via
// send_async + collect per req_id. Una sola op FS in volo (come libr).

/// Tag IPC FS (devono combaciare con userfs).
const FS_BUF_REG: u64 = 0x31;
const FS_REGISTER: u64 = 0x30;
/// Tag frame nel request ring (come libr).
const R_REGISTER: u32 = 0x30;

/// Scrive un request frame `[tag:4][w0:8][w1:8][payload]` nel ring FS proprio
/// (a FS_REQ_VA). Ritorna false se non c'e' spazio (il chiamante riprova).
fn fs_req_write(tag: u32, w0: u64, w1: u64, payload: &[u8]) -> bool {
    let frame_len = 20 + payload.len();
    unsafe {
        let head = core::ptr::read_volatile((FS_REQ_VA + RING_HEAD as u64) as *const u32);
        let tail = core::ptr::read_volatile((FS_REQ_VA + RING_TAIL as u64) as *const u32);
        let used = (head.wrapping_sub(tail)) % RING_DATA_CAP as u32;
        if (RING_DATA_CAP as u32) - used < frame_len as u32 + 1 {
            return false;
        }
        let dst = FS_REQ_VA as *mut u8;
        let mut hdr = [0u8; 20];
        hdr[0..4].copy_from_slice(&tag.to_le_bytes());
        hdr[4..12].copy_from_slice(&w0.to_le_bytes());
        hdr[12..20].copy_from_slice(&w1.to_le_bytes());
        for (i, byte) in hdr.iter().enumerate() {
            let p = ((head as usize) + i) % RING_DATA_CAP;
            core::ptr::write_volatile(dst.add(p), *byte);
        }
        for (i, byte) in payload.iter().enumerate() {
            let p = ((head as usize) + 20 + i) % RING_DATA_CAP;
            core::ptr::write_volatile(dst.add(p), *byte);
        }
        let new_head = ((head as usize) + frame_len) % RING_DATA_CAP;
        core::ptr::write_volatile((FS_REQ_VA + RING_HEAD as u64) as *mut u32, new_head as u32);
        true
    }
}

/// Toglie l'ultimo frame scritto (rollback su send_async fallita, come libr).
fn fs_req_rollback(frame_len: usize) {
    unsafe {
        let head = core::ptr::read_volatile((FS_REQ_VA + RING_HEAD as u64) as *const u32);
        let new_head = (head as usize + RING_DATA_CAP - frame_len % RING_DATA_CAP) % RING_DATA_CAP;
        core::ptr::write_volatile((FS_REQ_VA + RING_HEAD as u64) as *mut u32, new_head as u32);
    }
}

/// Legge il result di un response frame FS proprio (a FS_RESP_VA) e lo
/// consuma. Ritorna None a ring vuoto.
fn fs_resp_read() -> Option<u64> {
    unsafe {
        let head = core::ptr::read_volatile((FS_RESP_VA + RING_HEAD as u64) as *const u32);
        let tail = core::ptr::read_volatile((FS_RESP_VA + RING_TAIL as u64) as *const u32);
        if head == tail {
            return None;
        }
        let src = FS_RESP_VA as *const u8;
        let mut hdr = [0u8; 16];
        for i in 0..16 {
            hdr[i] = core::ptr::read_volatile(src.add(((head as usize) + i) % RING_DATA_CAP));
        }
        let result = u64::from_le_bytes(hdr[0..8].try_into().unwrap_or([0xFF; 8]));
        let new_tail = ((head as usize) + 16) % RING_DATA_CAP;
        core::ptr::write_volatile((FS_RESP_VA + RING_TAIL as u64) as *mut u32, new_tail as u32);
        Some(result)
    }
}

/// Azzera entrambi i ring FS propri (epoca morta dopo EXIT_NOTIFY di userfs).
fn fs_rings_reset() {
    unsafe {
        core::ptr::write_volatile((FS_REQ_VA + RING_HEAD as u64) as *mut u32, 0);
        core::ptr::write_volatile((FS_REQ_VA + RING_TAIL as u64) as *mut u32, 0);
        core::ptr::write_volatile((FS_RESP_VA + RING_HEAD as u64) as *mut u32, 0);
        core::ptr::write_volatile((FS_RESP_VA + RING_TAIL as u64) as *mut u32, 0);
    }
}

/// Stato della registrazione FS: handshake poi un prefix alla volta.
/// SENZA throttle: ogni tentativo fallito si riprova al prossimo wakeup (i
/// tentativi sono solo lookup/send cheap e `recv` blocca sempre dopo — mai
/// spin). Lo sleep in `recv` senza waker congelerebbe i retry (osservato:
/// registrazione ferma per sempre dopo un lookup fallito a boot).
struct FsReg {
    /// Fisici dei ring FS propri (per FS_BUF_REG).
    fs_req_phys: u64,
    fs_resp_phys: u64,
    /// Canale verso userfs (None = da risolvere).
    chan: Option<u64>,
    /// Handshake FS_BUF_REG completato sul canale corrente.
    bufreg_done: bool,
    /// req_id dell'op FS in volo (None = libero).
    pending: Option<i64>,
    /// Prossimo nodo da registrare.
    idx: usize,
}

impl FsReg {
    fn new(fs_req_phys: u64, fs_resp_phys: u64) -> Self {
        Self {
            fs_req_phys,
            fs_resp_phys,
            chan: None,
            bufreg_done: false,
            pending: None,
            idx: 0,
        }
    }

    /// Reset dopo morte di userfs (EXIT_NOTIFY): mounts purgati di la', i ring
    /// resettati di qua', si ricomincia da handshake + primo nodo.
    fn reset(&mut self) {
        libr::println!("[userdisk] reset registrazione FS (userfs morto)");
        self.chan = None;
        self.bufreg_done = false;
        self.pending = None;
        self.idx = 0;
        fs_rings_reset();
    }

    /// Completa se tutti i nodi registrati.
    fn done(&self, total: usize) -> bool {
        self.idx >= total
    }

    /// Avanza di UN passo (mai bloccante): risolve, handshake, registra.
    /// INVARIANTE (lezione tty): l'invio avviene NELLA STESSA chiamata che
    /// entra nella fase — un giro chiuso in recv senza aver inviato dorme.
    /// Ritenta a OGNI wakeup senza throttle: i tentativi sono solo lookup e
    /// send cheap, e `recv` blocca sempre dopo (mai spin). Uno sleep con
    /// throttle e senza waker congelerebbe i retry per sempre.
    fn step(&mut self, nodes: &[Node]) {
        if self.done(nodes.len()) || self.pending.is_some() {
            return;
        }
        // Canale (re-lookup se assente/stale: la send_async fallita lo azzera).
        let chan = match self.chan {
            Some(c) => c,
            None => match libr::service_lookup(libr::Service::Fs) {
                Ok(c) => {
                    self.chan = Some(c as u64);
                    self.bufreg_done = false;
                    c as u64
                }
                Err(_) => {
                    return;
                }
            },
        };
        if !self.bufreg_done {
            match libr::send_async(chan, FS_BUF_REG, self.fs_req_phys, self.fs_resp_phys) {
                Ok(req) => {
                    self.pending = Some(req);
                }
                Err(_) => {
                    self.chan = None;
                }
            }
            return;
        }
        // Un prefix alla volta (frame + notify async).
        let prefix = alloc::format!("/dev/{}", nodes[self.idx].name);
        let bytes = prefix.as_bytes();
        if !fs_req_write(R_REGISTER, bytes.len() as u64, 0, bytes) {
            return;
        }
        match libr::send_async(chan, FS_REGISTER, 0, 0) {
            Ok(req) => {
                self.pending = Some(req);
            }
            Err(_) => {
                fs_req_rollback(20 + bytes.len());
                self.chan = None;
            }
        }
    }

    /// Raccoglie una reply async che matcha il pending. Ritorna true se era
    /// nostra (consumata), con avanzamento di stato.
    fn collect_if_mine(&mut self, req_id: i64, nodes: &[Node]) -> bool {
        let pending = match self.pending {
            Some(p) if req_id > 0 && req_id == p => p,
            _ => return false,
        };
        let _ = pending;
        // BUF_REG non ha frame (register-only): basta il match.
        if !self.bufreg_done {
            self.bufreg_done = true;
            self.pending = None;
            return true;
        }
        // REGISTER: result dal response frame (0 = registrato).
        match fs_resp_read() {
            Some(0) => {
                self.pending = None;
                libr::println!("[userdisk] registered /dev/{} with userfs", nodes[self.idx].name);
                self.idx += 1;
            }
            _ => {
                // userfs ha scartato il frame (resync) o ring vuoto: pending
                // libero, si riprova al prossimo wakeup (mai throttle senza
                // waker: vedi `step`).
                self.pending = None;
            }
        }
        true
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    println!("[userdisk] starting, pid={}", libr::getpid());

    // 1. Rilevamento (solo HW, niente FS coinvolto).
    let mut infos = Vec::new();
    let atapi = detect::detect(&mut infos);
    let mut disks = Vec::new();
    for (i, info) in infos.iter().enumerate() {
        let letter = (b'a' + i as u8) as char;
        let model = core::str::from_utf8(&info.model[..info.model_len]).unwrap_or("?");
        println!(
            "[userdisk] sd{}: {} settori, {} {} ({})",
            letter,
            info.sectors,
            if info.lba48 { "LBA48" } else { "LBA28" },
            if info.cmd == 0x1F0 { "primary" } else { "secondary" },
            if info.drive == 0 { "master" } else { "slave" }
        );
        println!("[userdisk] sd{}: modello '{}'", letter, model);
        disks.push(block::AtaDisk::open(info.cmd, info.drive, info.lba48));
    }
    if atapi > 0 {
        println!("[userdisk] {} device ATAPI skippati (PACKET futuro)", atapi);
    }
    if disks.is_empty() {
        println!("[userdisk] nessun disco ATA: solo registrazione servizio");
    }

    // 2. Nodi: whole-disk + partizioni MBR primarie (graceful se assenti).
    // Handle = disco<<16|sub, allocato QUI (Fase 16c): la tabella `nodes' e'
    // la single source of truth nome→handle; userfs lo chiede con DISK_RESOLVE.
    let mut nodes: Vec<Node> = Vec::new();
    let mut disk_sectors: Vec<u64> = Vec::new();
    let mut parts: Vec<Vec<PartLoc>> = Vec::new();
    for (i, disk) in disks.iter().enumerate() {
        let letter = (b'a' + i as u8) as char;
        disk_sectors.push(infos[i].sectors);
        nodes.push(Node {
            name: alloc::format!("sd{}", letter),
            handle: (i as u32) << 16,
        });
        let mut disk_parts: Vec<PartLoc> = Vec::new();
        let mut sec0 = [0u8; 512];
        if disk.read_sector(0, &mut sec0) {
            let mut parsed = Vec::new();
            part::parse_mbr(&sec0, &mut parsed);
            for (p, part) in parsed.iter().enumerate() {
                println!(
                    "[userdisk] sd{}{}: tipo {:#04x}, start {}, settori {}",
                    letter,
                    p + 1,
                    part.ptype,
                    part.start,
                    part.sectors
                );
                nodes.push(Node {
                    name: alloc::format!("sd{}{}", letter, p + 1),
                    handle: ((i as u32) << 16) | (p as u32 + 1),
                });
                disk_parts.push(PartLoc { start: part.start, sectors: part.sectors });
            }
        }
        parts.push(disk_parts);
    }

    // 3. Ring FS + DISK dedicati (allocazione raw, MAI via libr::fs_init che e'
    // sincrono): FS per BUF_REG/REGISTER async, DISK per il data-plane con
    // userfs. Retry throttled: senza, niente registrazione ne' data-plane.
    // Reset head=tail: le pagine devono partire allineate.
    let (fs_req_phys, fs_resp_phys) = loop {
        if let Some(pair) = libr::ring_alloc_raw() {
            break pair;
        }
        for _ in 0..1_000_000 {
            core::hint::spin_loop();
        }
    };
    let (disk_req_phys, disk_resp_phys) = loop {
        if let Some(pair) = libr::ring_alloc_raw() {
            break pair;
        }
        for _ in 0..1_000_000 {
            core::hint::spin_loop();
        }
    };
    if libr::map_physical(fs_req_phys, FS_REQ_VA, 1).is_err()
        || libr::map_physical(fs_resp_phys, FS_RESP_VA, 1).is_err()
        || libr::map_physical(disk_req_phys, DISK_REQ_VA, 1).is_err()
        || libr::map_physical(disk_resp_phys, DISK_RESP_VA, 1).is_err()
    {
        println!("[userdisk] map ring fallita, exit");
        libr::exit(1);
    }
    fs_rings_reset();
    unsafe {
        core::ptr::write_volatile((DISK_REQ_VA + RING_HEAD as u64) as *mut u32, 0);
        core::ptr::write_volatile((DISK_REQ_VA + RING_TAIL as u64) as *mut u32, 0);
        core::ptr::write_volatile((DISK_RESP_VA + RING_HEAD as u64) as *mut u32, 0);
        core::ptr::write_volatile((DISK_RESP_VA + RING_TAIL as u64) as *mut u32, 0);
    }

    // 4. Servizio Disk per nome (ADR-0008): userfs lo risolve per il
    // data-plane, init per la supervisione, il kernel non instrada IRQ.
    if libr::service_register(libr::Service::Disk).is_ok() {
        println!("[userdisk] registered as service Disk");
    }

    // 5. READY al parent SUBITO (come console): userdisk parte PRIMA di userfs
    // (16.3) e l'ACK non puo' aspettare il mount (deadlock: il mount aspetta
    // Fs che parte dopo). Fire-and-forget, retry bounded, mai hang.
    for _ in 0..100 {
        if libr::send_async(libr::CHANNEL_PARENT, 0x7D, 1, 0).is_ok() {
            break;
        }
        for _ in 0..10_000 {
            core::hint::spin_loop();
        }
    }

    // 6. Registrazione FS via SM async (mai sync: vedi doc in testa). DISK e
    // DEV funzionano anche a registrazione incompleta: userfs monta appena
    // HELLO risponde, senza aspettare i prefix.
    let mut fsreg = FsReg::new(fs_req_phys, fs_resp_phys);

    // fd DEV_* (raw sequenziale) → (handle nodo, posizione in byte).
    let mut fds: BTreeMap<u32, (u32, u64)> = BTreeMap::new();
    let mut next_fd: u32 = 1;

    loop {
        // Invio nella stessa chiamata (lezione tty): prima di dormire in recv
        // bisogna aver notificato, altrimenti nessuno ci sveglia.
        fsreg.step(&nodes);
        let msg = match libr::recv() {
            Ok(m) => m,
            Err(_) => continue,
        };

        // Reply async FS (BUF_REG/REGISTER): consuma per primo, prima di ogni
        // dispatch (req_id > 0 solo per le risposte, mai per le richieste).
        if fsreg.collect_if_mine(msg.req_id, &nodes) {
            continue;
        }

        // userfs morto e rinato: reset SM (re-handshake + re-register). I ring
        // DISK persistono (pagine proprie): userfs rifa' HELLO da solo. Niente
        // send sincrone qui: solo reset di stato. Mai reply (peer morto).
        if msg.tag == libr::EXIT_NOTIFY {
            fsreg.reset();
            continue;
        }

        // ── Data-plane DISK_* (canale diretto userfs) ──
        if msg.tag == DISK_HELLO {
            // Fisici nei registri di reply (tag 0, mai !0 = ERR): niente frame.
            let _ = libr::reply(0, disk_req_phys, disk_resp_phys);
            continue;
        }
        if msg.tag == DISK_OPEN {
            if locate(msg.w0 as u32, &disk_sectors, &parts).is_some() {
                let _ = libr::reply(0, 0, 0);
            } else {
                let _ = libr::reply(0, ERR, 0);
            }
            continue;
        }
        if msg.tag == DISK_READ {
            let handle = msg.w0 as u32;
            let lba = msg.w1;
            let mut sec = [0u8; 512];
            if node_read(&disks, &disk_sectors, &parts, handle, lba, &mut sec) {
                unsafe { disk_resp_write(512, 0, &sec) };
                let _ = libr::reply(0, 0, 0);
            } else {
                let _ = libr::reply(0, ERR, 0);
            }
            continue;
        }
        if msg.tag == DISK_CLOSE {
            let _ = libr::reply(0, 0, 0);
            continue;
        }
        if msg.tag == DISK_RESOLVE {
            // Single source of truth nome→handle (Fase 16c): il nome corto
            // ("sda", "sda1") arriva nel frame DISK_REQ, l'handle torna in w0.
            // Sconosciuto/malformato → ERR, mai frame, mai wedge.
            let result = match disk_req_read_name() {
                Some(name) => nodes.iter().find(|n| n.name == name).map(|n| n.handle as u64),
                None => None,
            };
            let _ = libr::reply(0, result.unwrap_or(ERR), 0);
            continue;
        }

        // ── Relay DEV_* (open raw /dev/sdX dai client via userfs) ──
        // w0 di DEV_OPEN = handle codificato (disco<<16|sub, 0 = whole):
        // userfs-16.2 lo ricava parsando il nome Linux ("sda"→0, "sda1"→1),
        // senza bisogno della lista nodi. La posizione avanza a ogni READ.
        let result: Option<u64> = match msg.tag {
            DEV_OPEN => {
                let handle = msg.w0 as u32;
                if locate(handle, &disk_sectors, &parts).is_some() {
                    let fd = next_fd;
                    next_fd += 1;
                    fds.insert(fd, (handle, 0));
                    Some(fd as u64)
                } else {
                    None
                }
            }
            DEV_READ => {
                let (handle, pos) = match fds.get(&(msg.w0 as u32)) {
                    Some(&p) => p,
                    None => {
                        let _ = libr::reply(0, ERR, 0);
                        continue;
                    }
                };
                let node_sectors = match locate(handle, &disk_sectors, &parts) {
                    Some((_, _, s)) => s,
                    None => {
                        let _ = libr::reply(0, ERR, 0);
                        continue;
                    }
                };
                let avail = node_sectors * 512 - pos.min(node_sectors * 512);
                let want = (msg.w1 as u64).min(avail).min(4096);
                let nsec = (want / 512) as usize;
                if nsec == 0 {
                    // EOF o count < 512: frame vuoto + 0 (come /dev/null), mai
                    // wedge il client (lezione fix kbd/tty).
                    unsafe { resp_ring_write_client(&[]) };
                    Some(0)
                } else {
                    let mut buf = [0u8; 4096];
                    let mut ok = 0usize;
                    for s in 0..nsec {
                        let lba = pos / 512 + s as u64;
                        let mut sec = [0u8; 512];
                        if !node_read(&disks, &disk_sectors, &parts, handle, lba, &mut sec) {
                            break;
                        }
                        buf[s * 512..(s + 1) * 512].copy_from_slice(&sec);
                        ok += 1;
                    }
                    if ok == 0 {
                        None
                    } else {
                        let n = ok * 512;
                        unsafe { resp_ring_write_client(&buf[..n]) };
                        fds.insert(msg.w0 as u32, (handle, pos + n as u64));
                        Some(n as u64)
                    }
                }
            }
            DEV_WRITE => {
                // Read-only: consuma comunque il payload (tail!) e rifiuta.
                let count = msg.w1 as usize;
                unsafe { req_ring_consume_client(count) };
                None
            }
            DEV_CLOSE => {
                if fds.remove(&(msg.w0 as u32)).is_some() {
                    Some(0)
                } else {
                    None
                }
            }
            DEV_READDIR => {
                // Nomi delle partizioni figlie del nodo (o vuoto). Il chiamante
                // e' userfs su relay del prefix stesso: rel non disponibile qui,
                // quindi si elencano i figli di TUTTI i dischi? No: senza rel,
                // risposta vuota conservativa (t32 usa open/read diretti).
                unsafe { resp_ring_write_client(&[]) };
                Some(0)
            }
            _ => None,
        };
        // Idempotente: reply ERR senza frame (convenzione driver), come
        // devfs/kbd — il client vede -1, mai wedge.
        let _ = libr::reply(0, result.unwrap_or(ERR), 0);
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    println!("[userdisk] panic");
    libr::exit(1)
}
