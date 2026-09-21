//! Scheduler unico di Velordor: RT a 32 priorita' + CBS (Fase 11).
//!
//! 32 livelli di priorita' (0 = idle, 31 = massima) con run queue per-priorita'
//! O(1) tramite bitmask `u32` + `leading_zeros()`, piu' Constant Bandwidth
//! Server (`cbs.rs`) per la bandwidth reservation. Esposto come `crate::sched`
//! (vedi main.rs): i chiamanti (main/syscall/user_binary/process) usano
//! `crate::sched::*` senza conoscere i dettagli RT.

#[path = "sched_rt/queue.rs"]
mod queue;
#[path = "sched_rt/spawn.rs"]
mod spawn;
#[path = "sched_rt/tick.rs"]
mod tick;
#[path = "sched_rt/ipc.rs"]
mod ipc;
#[path = "sched_rt/ps.rs"]
mod ps;
#[path = "sched_rt/lifecycle.rs"]
mod lifecycle;
#[path = "sched_rt/ctx.rs"]
mod ctx;
#[path = "sched_rt/fork.rs"]
mod fork;

pub use spawn::{init, spawn, create_user};
pub use fork::fork_current;
pub use tick::{on_tick, notify_irq};
pub use ipc::{IpcResult, ipc_send, ipc_send_async, ipc_recv, ipc_recv_nonblock, ipc_reply};
// Compat: PsSnap era `pub` prima dello split (nessun uso interno attuale).
#[allow(unused_imports)]
pub use ps::PsSnap;
pub use ps::{process_state, process_ps, set_owned_name, set_parent_chan, parent_channel, process_of, process_name, process_cr3, process_image_hash};
pub use lifecycle::{exit_current, kill};

/// Quanto dura il timeslice in tick di PIT (100 Hz) → 2 tick = 20 ms.
const QUANTUM_TICKS: u64 = 2;

/// Massimo numero di PID / processi CONCORRENTI: `ready_by_prio` usa `u32`
/// (bit i = PID i pronto) → 32 PID totali. Dal Fase 14 (ADR-0010) i PID dei
/// processi reclamati vengono RIUSATI: il limite e' di concorrenza, non piu'
/// il numero totale di processi creati dal boot.
const MAX_PIDS: usize = 32;

/// Priorita' a 32 livelli (0 = idle, 31 = massima). Newtype struct con
/// costanti alias (`Priority::High`/`Normal`/`Low`) per leggibilita'.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[allow(non_upper_case_globals)]
pub struct Priority(pub u8);

#[allow(non_upper_case_globals)]
impl Priority {
    pub const High: Priority = Priority(31);
    pub const Normal: Priority = Priority(16);
    pub const Low: Priority = Priority(1);
    pub const Idle: Priority = Priority(0);
}
