// Split from vmm_user.rs (byte-identical move; see facade).
use super::layout::{MAX_PROCS, USER_HEAP_BASE};

/// `heap_brk` per processo (PID → indice). 0 = mai cresciuto → USER_HEAP_BASE.
/// Accesso single-core; aggiornato da `sbrk`, letto dal page-fault handler.
pub(super) static mut HEAP_BRK: [u64; MAX_PROCS] = [0; MAX_PROCS];

/// Ritorna il `heap_brk` corrente del processo `pid` (>= USER_HEAP_BASE).
pub fn heap_brk(pid: usize) -> u64 {
    if pid < MAX_PROCS {
        let v = unsafe { *core::ptr::addr_of!(HEAP_BRK[pid]) };
        if v == 0 { USER_HEAP_BASE } else { v }
    } else {
        USER_HEAP_BASE
    }
}

/// Imposta il `heap_brk` del processo `pid`.
pub fn set_heap_brk(pid: usize, val: u64) {
    if pid < MAX_PROCS {
        unsafe { *core::ptr::addr_of_mut!(HEAP_BRK[pid]) = val; }
    }
}
