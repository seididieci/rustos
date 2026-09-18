//! Meccanismo syscall/sysret (Fase 6.3).
//!
//! Configura i MSR `STAR`/`LSTAR`/`SFMASK` + `EFER.SCE` e `KernelGsBase`, e
//! fornisce l'entry assembly che trasferisce da ring 3 a ring 0 via `syscall`.
//!
//! A differenza di un interrupt, la CPU su `syscall` **non** cambia stack e
//! non fa push di un frame: salva solo RIP→RCX e RFLAGS→R11, poi salta a
//! `LSTAR` con `CS` dal `STAR`. L'entry deve quindi, manualmente:
//!   1. fare `swapgs` per portare `GS.base` verso l'area `PERCPU` del kernel;
//!   2. `mov rsp` verso uno stack kernel per-processo (campo `rsp0`);
//!   3. salvare RSP user (`user_rsp`) e gli argomenti su `PERCPU`;
//!   4. chiamare `syscall_handler` e, al ritorno, ripristinare RSP user,
//!      `swapgs` di nuovo e `sysretq`.
//!
//! `PERCPU` e' un'area per-core: oggi ce n'e' una sola (single core), ma il
//! layout `#[repr(C)]` con offset fissi e `KernelGsBase` per-core e' pensato
//! per passare al multicore senza ristrutturare l'entry.

mod entry;
mod dispatch;
mod ipc;
mod service;
mod spawn;
mod mem;
mod misc;

pub use entry::{init, set_current, current_id};
// Fase 34 (fork): offset dei registri salvati + trampoline di ritorno figlio.
pub(crate) use entry::{
    fork_child_exit, SAVED_R8, SAVED_R9, SAVED_R10, SAVED_R11, SAVED_R13,
    SAVED_R14, SAVED_R15, SAVED_RBX, SAVED_RBP, SAVED_RCX, SAVED_RDI, SAVED_RDX,
    SAVED_RSI, SAVED_USER_R12, SAVED_USER_RSP,
};
// Compat: era `pub` prima dello split (nessun uso interno attuale).
#[allow(unused_imports)]
pub use entry::syscall_entry;
