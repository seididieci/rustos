//! userdisk — Driver disco ATA in userspace (Fase 16).
//!
//! Possiede le porte ATA del canale primario (via `io_ranges`, TSS
//! per-processo ADR-0006; il secondario e' probato da `detect.rs` ma non
//! concesso: toccarlo e' #GP — su QEMU non esiste), rileva i dischi presenti
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
//!   OPEN/READ multi-settore (24.2, count≤7 per IPC), CLOSE, RESOLVE
//!   chiave→handle (Fase 16c: userdisk e' l'unico proprietario della mappa;
//!   16d: chiave = nome (`sda`), UUID hex 8 char (seriale volume FAT) o label
//!   (priorità in quest'ordine).
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
mod cache;
mod detect;
mod dma;
mod part;
mod fs_reg;
mod nodes;
mod rings;
mod server;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use libr::println;

// ── IPC tags (DocsD: single source in `syscall-numbers`, via `libr`) ──
use libr::{DEV_CLOSE, DEV_OPEN, DEV_READ, DEV_READDIR, DEV_WRITE};

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
/// Scrive un settore (Fase 20, FAT scrivibile): handle in w0, lba in w1,
/// payload 512 byte nel frame DISK_REQ; reply senza frame.
use libr::DISK_WRITE;

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
// Geometria ring + errore IPC (A1) + frame helpers (A2): single source in `libr`.
use libr::{ERR, RING_DATA_CAP, RING_HEAD, RING_TAIL};
use libr::{req_frame_consume, resp_frame_write};

/// Tag IPC FS (DocsB: single source in `syscall-numbers`, via `libr`).
use libr::{FS_BUF_REG, FS_REGISTER};
/// Tag frame nel request ring (single source in `syscall-numbers`, Fase 17).
use libr::R_REGISTER;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    println!("[userdisk] panic");
    libr::exit(1)
}
