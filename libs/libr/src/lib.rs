//! libr — libreria di sistema per processi user (equivalente minimale di una
//! libc per Velordor). Fornisce i wrapper alle syscall del kernel.
//!
//! ABI syscall (vedi docs/src/06-syscalls.md — numerazione arbitraria del
//! progetto, NON standard):
//!   - `rax` : numero di syscall
//!   - `rdi` : arg1
//!   - `rsi` : arg2
//!   - `rdx` : arg3
//!   - `r10` : arg4
//!   - ritorno in `rax`
//!
//! Syscall implementate dal kernel (Fase 6):
//!   - 0 = exit(code)
//!   - 2 = write(fd, buf, count)
//!   - 8 = getpid()
//!
//! Nota: `syscall`/`sysret` salvano `RCX` (RIP) e `R11` (RFLAGS) senza toccarli,
//! quindi lo shim li marca come `lateout` per non affermare di conservarli.

#![no_std]

extern crate alloc;

pub extern crate syscall_numbers;
use syscall_numbers::*;
use core::fmt;
use core::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};

/// Pagina fisica scratch riservata dal kernel per i test `map_physical`.
pub use syscall_numbers::MAP_TEST_PHYS;
/// Finestra staging DMA (Fase 38.1): `dma_alloc` mappa qui i frame contigui.
pub use syscall_numbers::USER_DMA_VA;
/// Servizi di sistema raggiungibili per nome (ADR-0008).
pub use syscall_numbers::Service;
/// Tag della notifica kernel→parent della morte di un figlio (Fase 14).
pub use syscall_numbers::EXIT_NOTIFY;
/// Tag della notify kernel→userkbd su IRQ1 (Fase 15, bridge interrupt→IPC).
pub use syscall_numbers::IRQ_NOTIFY_KBD;
/// Tag della notify kernel→userdisk su IRQ14/15 (Fase 38, ATA DMA: stesso
/// bridge interrupt→IPC — userdisk drena lo status Bus-Master ad ogni giro).
pub use syscall_numbers::IRQ_NOTIFY_DISK;
/// Bound di scansione PID per `ps` (Fase 19.1, = MAX_PIDS del kernel).
pub use syscall_numbers::PS_SCAN_MAX;
/// Identita' misurata di un'immagine ELF (Fase 36, Strato 2 di ADR-0026):
/// FNV-1a sui byte dell'ELF — stesso valore che il kernel misura allo spawn.
/// init/userfs la ricalcolano sui byte caricati (manifest, policy FS_REGISTER).
pub use syscall_numbers::image_hash;
/// Protocollo DISK_* userfs→userdisk (Fase 16, single source in
/// `syscall-numbers`, Fase 16c): handshake/open/read/close + resolve
/// nome→handle di proprieta' del driver.
pub use syscall_numbers::{DISK_CLOSE, DISK_HELLO, DISK_OPEN, DISK_READ, DISK_RESOLVE, DISK_WRITE};

// ── Tag delle operazioni (nel frame del ring, non nell'IPC) ────────
// Single source in `syscall-numbers` (Fase 17): prima duplicati qui, in
// userfs e (R_REGISTER) userdisk.
pub use syscall_numbers::{
    R_CLOSE, R_DELETE, R_MKDIR, R_MOUNT, R_OPEN, R_READ, R_READDIR, R_REGISTER, R_UMOUNT,
    R_WRITE, R_RIGHTS_DROP, R_RIGHTS_GET, R_STAT, R_LSEEK, R_DUP_GRANT, R_DUP_CLAIM,
    R_DUP_CANCEL,
};
/// Bit dei diritti per-canale (Fase 17, self-restriction; DELETE in 18.2;
/// SEEK in 40): mask per `rights_drop`, valore di ritorno di `rights_get`.
pub use syscall_numbers::{
    RIGHTS_ALL, RIGHTS_DELETE, RIGHTS_MKDIR, RIGHTS_MOUNT, RIGHTS_OPEN, RIGHTS_READ,
    RIGHTS_READDIR, RIGHTS_SEEK, RIGHTS_UMOUNT, RIGHTS_WRITE,
};
/// `kind` per R_STAT (Fase 19.2): bit 0-1 tipo + bit 7 readonly.
pub use syscall_numbers::{STAT_DEVICE, STAT_DIR, STAT_FILE, STAT_READONLY};

/// Flag `open` (Fase 18.2: O_CREAT; Fase 40: O_TRUNC/O_APPEND) + origini
/// `SEEK_*` per R_LSEEK + sentinelle di errore FS (Fase 40: il server
/// distingue i rifiuti, il client li mappa in `posix::Error`).
pub use syscall_numbers::{
    O_APPEND, O_CREAT, O_TRUNC, SEEK_CUR, SEEK_END, SEEK_SET, ERR_BUSY, ERR_EXISTS,
    ERR_INVALID, ERR_ISDIR, ERR_NOTDIR, ERR_NOTFOUND, ERR_READONLY,
};

/// Fase 29 (mmap/mprotect): protezioni + codice di uscita per fault di
/// memoria, e layout stack condiviso (guard page) per i test.
pub use syscall_numbers::{
    FAULT_EXIT_CODE, MAP_COW, MMAP_FIXED, PROT_NONE, PROT_READ, PROT_WRITE, USER_CODE,
    USER_STACK_FRAMES, USER_STACK_GUARD, USER_STACK_TOP,
};

/// Tag IPC FS/boot/kbd (DocsB): single source in `syscall-numbers` (prima
/// duplicati qui, in userfs/userdisk/init/tty/kbd e come letterali nei test).
/// `libr` li riesporta: i server/test usano i path `libr::`, mai i valori.
pub use syscall_numbers::{
    FS_BUF_REG, FS_NOTIFY, FS_REGISTER, INIT_BOUNCE, KBD_NOTIFY, SVC_READY, TEST_DONE,
};
/// Tag DEV_* op + device type (DocsD): stesso pattern, prima duplicati in
/// userfs/userdisk/devfs/console/kbd/tty.
pub use syscall_numbers::{
    DEV_CLOSE, DEV_KBD, DEV_KEYBOARD, DEV_CONSOLE, DEV_NULL, DEV_OPEN, DEV_READ,
    DEV_READDIR, DEV_WRITE, DEV_ZERO,
};

// ── Stratificazione meccanismo / personalita' (ADR-0025, ADR-0015) ───
// `libr` ospita strati che non si mescolano verso il basso:
// - MECCANISMO (neutro, usabile da chiunque): sys, ipc, heap/scratch, task
//   (async-first e' nativo), pio/pci, tsc, test, error (vocabolario condiviso).
// - PERSONALITA' POSIX (traduzione al bordo, solo userspace): posix
//   (`to_errno` + `E*`), stdio (tabelle vfd + redirect), RedirEntry/
//   serialize_argv_redir/stdio_restore (handoff redirect).
// - MISTI dichiarati (nomi POSIX sopra meccanismo nativo — non forzati in una
//   scatola sola): fs, spawn (`spawn_image`/`exec` meccanismo, `fork`
//   confinato POSIX), print, args (convenzione dati neutra al servizio della
//   personalita').
// REGOLA: i moduli meccanismo non usano `posix::` (verifica con
// `rg "posix::" libs/libr/src`: solo posix.rs + usi al bordo). La traduzione
// vive solo al bordo; una seconda personalita' riusa il meccanismo invariato.

/// Allocatore globale on-demand (free-list + `sbrk`): unico per tutto il
/// userland. Vive qui cosi' ogni binario che linka `libr` lo usa senza
/// duplicare codice.
pub mod heap;

/// Scratch arena per-op (bump + `reset()`, backing `sbrk` dedicato fuori
/// free-list): per i temporanei con lifetime = una richiesta. Mai heap
/// globale nei percorsi per-op (regola 24.2 aggiornata).
pub mod scratch;

/// Executor async minimale sopra l'IPC asincrona (ADR-0019): `Future`
/// (`WaitReply`, `RecvMsg`), tratto `Receivable` per l'instradamento,
/// `block_on` single-task e `run` multi-task a router centrale.
/// Kernel invariato; vincoli Fase 13 invariati (vedi modulo).
pub mod task;

/// Port I/O x86 in ring 3 (A4: prima duplicato in userdisk/userkbd).
pub mod pio;

/// Spazio di configurazione PCI in ring 3 (Fase 38.0d, ATA DMA): modulo
/// condiviso e traslocabile (servizio `userland/pci` solo al secondo consumer).
pub mod pci;

/// Harness condiviso per la test suite (A4: traversal readdir).
pub mod test;

/// Vocabolario condiviso di tutti i wrapper (Fase 39, ADR-0030): vive in un
/// modulo neutro perche' meccanismo e personalita' lo usano senza distinzione.
/// La traduzione in errno sta SOLO in `posix::to_errno` (bordo).
pub mod error;

/// Errore nativo del sistema + traduzione errno al bordo POSIX (Fase 39,
/// fondamenta posix, ADR-0030): i numeri POSIX non entrano mai nel kernel/wire.
pub mod posix;

pub use error::Error;

pub mod fs;
pub mod ipc;
pub mod print;
pub mod spawn;

/// Tabelle stdio vfd + routing stdout/stdin (Fase 40.2, redirect P1):
/// `set_stdio`/`clear_stdio`/`stdio_active`/`stdout_fd`/`stdin_byte` (il sink
/// seriale `print_string`/`sys::write` resta sempre seriale per disegno).
pub mod stdio;
pub use stdio::{clear_stdio, set_stdio, stdio_active, stdin_byte, stdin_fd};
pub use stdio::{stderr_fd, stdout_fd};
/// Voce di spec redirect per `serialize_argv_redir` (Fase 40.4c/d): grant da
/// riscuotere o alias di slot (`2>&1`). Costruita dalla shell, consumata dallo
/// startup — mai visibile ai programmi.
pub use stdio::RedirEntry;
/// Ripristino redirect dallo stack iniziale (Fase 40.4c): CRT-internal, pub
/// perche' l'espansione `entry!` vive nei crate utenti. Chiamata dal wrapper
/// `__velordor_entry` prima di `real_main`; mai direttamente dai programmi.
pub use stdio::stdio_restore;
pub mod sys;
pub mod tsc;

/// Convenzione argv sullo stack iniziale (Fase 37.1): macro `entry!` (CRT
/// minimale) + parser `args_from_stack`. Layout stile Linux come convenzione
/// di dati neutra (ADR-0025 §Neutral).
pub mod args;

/// Esegue una syscall a 4 argomenti e ne restituisce il risultato in `rax`.
///
/// # Safety
/// Il numero e gli argomenti devono essere validi per il kernel del target.
#[inline]
pub unsafe fn syscall4(
    number: u64,
    arg1: u64,
    arg2: u64,
    arg3: u64,
    arg4: u64,
) -> i64 {
    let ret: i64;
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") number => ret,
            in("rdi") arg1,
            in("rsi") arg2,
            in("rdx") arg3,
            in("r10") arg4,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret
}

/// Come `syscall4`, ma cattura anche i registri di ritorno `rdi/rsi/rdx/r10`:
/// usato dalle syscall IPC (ADR-0008) che restituiscono piu' parole (channel,
/// tag, w0, w1) oltre allo stato in `rax`.
///
/// # Safety
/// Il numero e gli argomenti devono essere validi per il kernel del target.
#[inline(always)]
pub unsafe fn syscall4_out(
    number: u64,
    arg1: u64,
    arg2: u64,
    arg3: u64,
    arg4: u64,
) -> (i64, u64, u64, u64, u64) {
    let rax: i64;
    let rdi: u64;
    let rsi: u64;
    let rdx: u64;
    let r10: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") number => rax,
            inlateout("rdi") arg1 => rdi,
            inlateout("rsi") arg2 => rsi,
            inlateout("rdx") arg3 => rdx,
            inlateout("r10") arg4 => r10,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    (rax, rdi, rsi, rdx, r10)
}

pub use fs::{ops_async::*, ring::*, session::*, sync::*};
pub use ipc::*;
pub use print::*;
pub use spawn::*;
pub use sys::*;
pub use tsc::*;
pub use args::{Args, args_from_stack, ARGS_MAX};
// `CHANNEL_PARENT` e' anche in `syscall-numbers` (glob privato sopra):
// il single-item esplicito vince sui glob e preserva `libr::CHANNEL_PARENT`.
pub use ipc::CHANNEL_PARENT;
