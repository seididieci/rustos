//! userfs — File system server (Fase 9.1 + 9.2 + 9.3 + 10.2 + 16.2).
//!
//! Riceve IPC dai processi client (open/read/write/close/readdir/mkdir) e
//! gestisce:
//!   - ramfs in memoria sul mount point `/` (scrivibile, Fase 9.1)
//!   - FAT32 dal disco via `userdisk` sul mount point `/fat` (Fase 9.2 su
//!     ATA locale; Fase 16 via IPC `DISK_*`; Fase 16c resolve nome→handle
//!     lato driver; **scrivibile dalla Fase 20**: overwrite/crescita/`O_CREAT`,
//!     niente unlink)
//!   - devfs/console remoti via IPC per device `/dev/*` (Fase 9.3)
//!
//! Trasferimento dati (Fase 10.2): ogni client ha DUE pagine ring SPSC
//! (request + response) allocate dalla syscall 26 (`SYS_RING_ALLOC`). Il client
//! scrive un request frame nel request ring, notifica con `FS_NOTIFY`, e userfs
//! legge il frame, processa, e scrive il response frame nel response ring del
//! client. Per i device remoti userfs inietta la response ring del client nel
//! processo driver (`libr::map_in`) cosi' il driver scrive i dati direttamente
//! nella response ring del client — zero copie.

#![no_std]
#![no_main]

extern crate alloc;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use libr;

mod fat32;
mod ipc_disk;

use fat32::{Fat32, FileInfo};
use ipc_disk::IpcDisk;
use libr::println;

mod ftable;
mod handlers;
mod mount;
mod mount_legacy;
mod ramfs;
mod rights;
mod rings;
mod server;

// Geometria ring + errori IPC (A1): single source in `libr`.
use libr::{
    ERR, ERR_NOHANDSHAKE, RING_DATA_CAP, RING_HEAD, RING_TAIL, ring_available,
    ring_positions,
};

// ── Tag delle operazioni (nei frame del ring) ─────────────────────
// Single source in `syscall-numbers` (Fase 17): include R_RIGHTS_DROP/GET.
use libr::{
    R_CLOSE, R_DELETE, R_MKDIR, R_MOUNT, R_OPEN, R_READ, R_READDIR, R_REGISTER, R_UMOUNT,
    R_WRITE, R_RIGHTS_DROP, R_RIGHTS_GET, R_STAT,
};
// Tag DEV_* op + device types (DocsD: single source in `syscall-numbers`).
use libr::{
    DEV_CLOSE, DEV_CONSOLE, DEV_KBD, DEV_KEYBOARD, DEV_NULL, DEV_OPEN, DEV_READ,
    DEV_READDIR, DEV_WRITE, DEV_ZERO,
};
// Tag IPC FS/boot (DocsB): single source in `syscall-numbers`, via `libr`.
use libr::{FS_BUF_REG, FS_NOTIFY, FS_REGISTER};

const MAX_PATH: usize = 256;

// ── IPC tags verso i driver remoti (devfs/console/kbd/tty/disk): op DEV_*
// e device types importati sopra da `syscall-numbers` (DocsD) ──────────

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("[userfs] panic: {}", info.message());
    libr::exit(1)
}
