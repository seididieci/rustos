//! userdisk — Driver disco ATA in userspace (Fase 16).
//!
//! Possiede le porte ATA primario + secondario (via `io_ranges`, TSS
//! per-processo ADR-0006), rileva i dischi presenti via `IDENTIFY`
//! (`detect.rs`), parsa le partizioni MBR primarie (`part.rs`) ed espone ogni
//! nodo come `/dev/sdX` (`FS_REGISTER` per nodo, solo i presenti) + servizio
//! `Disk` per il data-plane verso userfs.
//!
//! Due protocolli, entrambi con reply implicita (ADR-0008):
//! - `DISK_*` (canale diretto userfs→userdisk, service_lookup(Disk)): HELLO
//!   (handshake ring: il frame riporta i fisici dei ring DISK + i nomi nodi),
//!   OPEN(indice), READ settoriale (w0=handle, w1=lba, 512 B nel frame),
//!   CLOSE. Un settore per chiamata (1:1 con `BlockSource::read_sector`).
//!   userfs emette una op alla volta: niente interleave nei ring DISK.
//! - `DEV_*` (relay userfs per gli open raw `/dev/sdX`): OPEN(w0=indice nodo,
//!   documentato: userfs-16.2 passa l'indice dalla mappa nomi di HELLO),
//!   READ sequenziale con posizione per-fd (solo multipli di 512; resto
//!   scartato), WRITE sempre ERR (read-only come prima), CLOSE, READDIR
//!   (nomi partizioni figlie o vuoto).
//!
//! Boot: detection (solo HW) → `service_register(Disk)` → SVC_READY al parent
//! SUBITO (userdisk parte PRIMA di userfs in 16.3: come console, l'ACK non
//! aspetta il mount) → `ensure_mounted` in loop (attende Fs).

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

/// Handshake data-plane: userfs chiede i fisici dei ring DISK + la lista nodi.
/// Frame risposta: [0:8][n_nodi:8][req_phys:8][resp_phys:8][nomi NUL-joined].
const DISK_HELLO: u64 = 0x50;
/// Apre un nodo (w0 = indice nella tabella nodi). Frame vuoto + reply OK.
const DISK_OPEN: u64 = 0x51;
/// Legge UN settore (w0 = handle = indice nodo, w1 = lba nel nodo).
/// Frame: [512:8][0:8][settore]. Fuori range/errore → reply ERR, niente frame.
const DISK_READ: u64 = 0x52;
/// Chiude (stateless: sempre OK, frame vuoto).
const DISK_CLOSE: u64 = 0x53;

// ── Ring I/O ────────────────────────────────────────────────────────
// Due coppie SEPARATE (lezione CLI_* del fix kbd/tty: mai protocolli diversi
// nello stesso ring):
// - REQ/RESP_RING_VA (libr): traffico FS proprio (FS_REGISTER + relay DEV,
//   con finestre CLI_* mappate da userfs a ogni relay).
// - DISK_REQ_VA/DISK_RESP_VA: data-plane DISK_* con userfs (fisso, noto a
//   userfs via HELLO). Libere nella mappa user (CLI fino a +0x23..., heap da
//   +0x400000).

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

/// Nodo esposto: whole-disk o partizione MBR di un disco rilevato.
struct Node {
    /// Nome breve ("sda", "sda1"): prefix registrato = "/dev/" + nome.
    name: String,
    /// Indice in `disks`.
    disk: usize,
    /// Offset in settori dall'inizio disco (0 = whole-disk).
    base: u64,
    /// Settori del nodo.
    sectors: u64,
}

/// Legge il settore fisico `lba` del disco `di` (bound check sul nodo fuori).
fn node_read(
    disks: &[block::AtaDisk],
    nodes: &[Node],
    idx: usize,
    lba: u64,
    out: &mut [u8; 512],
) -> bool {
    let node = match nodes.get(idx) {
        Some(n) => n,
        None => return false,
    };
    if lba >= node.sectors {
        return false;
    }
    match disks.get(node.disk) {
        Some(d) => d.read_sector(node.base + lba, out),
        None => false,
    }
}

// ── Mount ───────────────────────────────────────────────────────────

/// Registra ogni nodo come `/dev/<nome>` presso userfs (stesso pattern di
/// devfs `ensure_mounted`): attende Fs via soli lookup, poi registra tutti i
/// prefix (idempotenti per replace-on-register in userfs). Unbounded: senza Fs
/// il driver e' comunque inutile. Stessa funzione a boot e su EXIT_NOTIFY.
fn ensure_mounted(nodes: &[Node]) {
    let _ = libr::fs_remap_self();
    loop {
        while libr::service_lookup(libr::Service::Fs).is_err() {
            for _ in 0..1_000_000 {
                core::hint::spin_loop();
            }
        }
        let mut ok = true;
        for node in nodes {
            let prefix = alloc::format!("/dev/{}", node.name);
            if libr::fs_register(prefix.as_bytes()) != 0 {
                ok = false;
                break;
            }
        }
        if ok {
            return;
        }
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
    let mut nodes: Vec<Node> = Vec::new();
    for (i, disk) in disks.iter().enumerate() {
        let letter = (b'a' + i as u8) as char;
        let sectors = infos[i].sectors;
        nodes.push(Node {
            name: alloc::format!("sd{}", letter),
            disk: i,
            base: 0,
            sectors,
        });
        let mut sec0 = [0u8; 512];
        if disk.read_sector(0, &mut sec0) {
            let mut parts = Vec::new();
            part::parse_mbr(&sec0, &mut parts);
            for (p, part) in parts.iter().enumerate() {
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
                    disk: i,
                    base: part.start as u64,
                    sectors: part.sectors as u64,
                });
            }
        }
    }

    // 3. Ring DISK dedicati (data-plane con userfs): allocazione raw, niente
    // handshake libr (i fisici viaggiano nel frame DISK_HELLO). Retry
    // throttled: senza, niente data-plane.
    let (disk_req_phys, disk_resp_phys) = loop {
        if let Some(pair) = libr::ring_alloc_raw() {
            break pair;
        }
        for _ in 0..1_000_000 {
            core::hint::spin_loop();
        }
    };
    if libr::map_physical(disk_req_phys, DISK_REQ_VA, 1).is_err()
        || libr::map_physical(disk_resp_phys, DISK_RESP_VA, 1).is_err()
    {
        println!("[userdisk] map ring DISK fallita, exit");
        libr::exit(1);
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

    // 6. Registra i nodi presso userfs (attende Fs: parte dopo di noi).
    ensure_mounted(&nodes);
    for node in &nodes {
        println!("[userdisk] registered /dev/{} with userfs", node.name);
    }

    // fd DEV_* (raw sequenziale) → (indice nodo, posizione in byte).
    let mut fds: BTreeMap<u32, (usize, u64)> = BTreeMap::new();
    let mut next_fd: u32 = 1;

    loop {
        let msg = match libr::recv() {
            Ok(m) => m,
            Err(_) => continue,
        };

        // userfs morto e rinato: re-mount (come devfs/kbd, t28). I ring DISK
        // persistono (pagine proprie): userfs rifa' HELLO in 16.2. Mai reply.
        if msg.tag == libr::EXIT_NOTIFY {
            println!("[userdisk] peer morto, re-mount nodi");
            ensure_mounted(&nodes);
            continue;
        }

        // ── Data-plane DISK_* (canale diretto userfs) ──
        if msg.tag == DISK_HELLO {
            // Frame: [0:8][n_nodi:8][req_phys:8][resp_phys:8][nomi NUL].
            let mut payload = Vec::new();
            payload.extend_from_slice(&disk_req_phys.to_le_bytes());
            payload.extend_from_slice(&disk_resp_phys.to_le_bytes());
            for node in &nodes {
                payload.extend_from_slice(node.name.as_bytes());
                payload.push(0);
            }
            unsafe { disk_resp_write(0, nodes.len() as u64, &payload) };
            let _ = libr::reply(0, 0, 0);
            continue;
        }
        if msg.tag == DISK_OPEN {
            if (msg.w0 as usize) < nodes.len() {
                unsafe { disk_resp_write(0, 0, &[]) };
                let _ = libr::reply(0, 0, 0);
            } else {
                let _ = libr::reply(0, ERR, 0);
            }
            continue;
        }
        if msg.tag == DISK_READ {
            let idx = msg.w0 as usize;
            let lba = msg.w1;
            let mut sec = [0u8; 512];
            if node_read(&disks, &nodes, idx, lba, &mut sec) {
                unsafe { disk_resp_write(512, 0, &sec) };
                let _ = libr::reply(0, 0, 0);
            } else {
                let _ = libr::reply(0, ERR, 0);
            }
            continue;
        }
        if msg.tag == DISK_CLOSE {
            unsafe { disk_resp_write(0, 0, &[]) };
            let _ = libr::reply(0, 0, 0);
            continue;
        }

        // ── Relay DEV_* (open raw /dev/sdX dai client via userfs) ──
        // w0 di DEV_OPEN = indice nodo (userfs-16.2 lo ricava dalla mappa
        // nomi di HELLO); la posizione avanza a ogni READ (sequenziale).
        let result: Option<u64> = match msg.tag {
            DEV_OPEN => {
                let idx = msg.w0 as usize;
                if idx < nodes.len() {
                    let fd = next_fd;
                    next_fd += 1;
                    fds.insert(fd, (idx, 0));
                    Some(fd as u64)
                } else {
                    None
                }
            }
            DEV_READ => {
                let (idx, pos) = match fds.get(&(msg.w0 as u32)) {
                    Some(&p) => p,
                    None => {
                        let _ = libr::reply(0, ERR, 0);
                        continue;
                    }
                };
                let node_sectors = nodes[idx].sectors;
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
                        if !node_read(&disks, &nodes, idx, lba, &mut sec) {
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
                        fds.insert(msg.w0 as u32, (idx, pos + n as u64));
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
