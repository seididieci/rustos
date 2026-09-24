// Split from syscall.rs (byte-identical move; see facade).
use core::ptr::{addr_of, addr_of_mut};
use super::entry::{current_id, PERCPU};

/// exit(code): termina il processo corrente. Non ritorna (tipo `!` → i64).
pub(super) fn sys_exit(code: i64) -> i64 {
    crate::sched::exit_current(code)
}

/// Fase 14 — `kill(pid, code)`: termina un processo user per la stessa via di
/// exit (cleanup differito + cascata sulla discendenza + notifica al parent).
/// Fase 35 (hardening): solo il parent (o init, pid 1) puo' killare — uccidere
/// un server supervisionato e' operazione da supervisore (via init, vedi
/// `INIT_BOUNCE`; kill diretto altrui = -1). Ritorna 0 se il processo e'
/// stato terminato, -1 se il pid non esiste / non e' killabile (init,
/// processi kernel, se stesso, non-figlio).
pub(super) fn sys_kill(pid: u64, code: i64) -> i64 {
    let me = current_id() as usize;
    let target = pid as usize;
    if me != 1 {
        match crate::sched::process_ps(target) {
            Some(s) if s.parent == Some(me) => {}
            _ => return -1, // non-figlio (o morto/sconosciuto): rifiutato
        }
    }
    if crate::sched::kill(target, code) {
        0
    } else {
        -1
    }
}

/// getpid(): id del processo corrente.
pub(super) fn sys_getpid() -> i64 {
    current_id() as i64
}

/// Fase 44a (job control) — `suspend(pid)`: congela un processo user
/// (meccanismo neutro, semantica POSIX in shell). Stessi gate di `kill`:
/// solo il parent (o init, pid 1) puo' sospendere. Ritorna 0 se il processo
/// e' sospeso (o gia' sospeso: idempotente), -1 se il pid non esiste / non e'
/// sospendibile (init, processi kernel, se stesso, non-figlio, terminato).
pub(super) fn sys_suspend(pid: u64) -> i64 {
    let me = current_id() as usize;
    let target = pid as usize;
    if me != 1 {
        match crate::sched::process_ps(target) {
            Some(s) if s.parent == Some(me) => {}
            _ => return -1, // non-figlio (o morto/sconosciuto): rifiutato
        }
    }
    if crate::sched::suspend(target) {
        0
    } else {
        -1
    }
}

/// Fase 44a (job control) — `resume(pid)`: rimette in schedulazione un
/// processo sospeso (o no-op ok se gia' running). Stessi gate di `suspend`.
pub(super) fn sys_resume(pid: u64) -> i64 {
    let me = current_id() as usize;
    let target = pid as usize;
    if me != 1 {
        match crate::sched::process_ps(target) {
            Some(s) if s.parent == Some(me) => {}
            _ => return -1, // non-figlio (o morto/sconosciuto): rifiutato
        }
    }
    if crate::sched::resume(target) {
        0
    } else {
        -1
    }
}

/// write(fd, buf, count): stampa su seriale per fd 1/2; ogni altro fd
/// (nessun file implementato in questa fase) → -1.
///
/// Streaming raw a chunk fissi (256 B) via `serial::_write_bytes`: MAI
/// allocazioni, per qualunque `count` (un `from_utf8_lossy` qui allocherebbe
/// `count` byte + free con coalesce O(n²) a OGNI println userspace, oltre a
/// rischiare OOM/panic su `count` enormi). I byte passano tali e quali, senza
/// validazione UTF-8: audit fedele (byte in = byte sul filo). La logica
/// dmesg (timestamp a inizio riga) vive nel writer ed e' trasparente al
/// chunking, anche con `\n` a cavallo tra chunk.
pub(super) fn sys_write(fd: u64, buf: *const u8, count: usize) -> i64 {
    if fd != 1 && fd != 2 {
        return -1;
    }
    if count == 0 {
        return 0;
    }
    // Validazione: il buffer deve stare nel range user mappato (U=1).
    if !crate::vmm_user::is_user_range(buf as u64, count) {
        crate::serial_println!("[syscall] write: puntatore fuori dallo spazio user");
        return -1;
    }
    const CHUNK: usize = 256;
    let slice = unsafe { core::slice::from_raw_parts(buf, count) };
    let mut off = 0;
    while off < count {
        let end = (off + CHUNK).min(count);
        crate::serial::_write_bytes(&slice[off..end]);
        off = end;
    }
    count as i64
}

/// get_ticks(): ritorna il contatore corrente di PIT ticks (100 Hz).
pub(super) fn sys_get_ticks() -> i64 {
    crate::pit::ticks() as i64
}

// ── CBS syscall handlers (Fase 11.4) ────────────────────────────────

/// cbs_create(budget, period): crea un server CBS con i parametri dati.
/// Esegue l'admission control: ritorna l'id del server o -1.
pub(super) fn sys_cbs_create(budget: u64, period: u64) -> i64 {
    match crate::cbs::create(budget as u32, period as u32) {
        Ok(id) => id as i64,
        Err(()) => -1,
    }
}

/// cbs_attach(): lega il server CBS indicato al processo corrente.
/// Il server_id viene passato in arg1, il PID corrente e' da `current_id`.
pub(super) fn sys_cbs_attach() -> i64 {
    let server_id = unsafe { (*(addr_of!(PERCPU))).arg1 as usize };
    let pid = current_id() as usize;
    match crate::cbs::attach(server_id, pid) {
        Ok(()) => {
            if let Some(proc) = crate::sched::process_of(pid) {
                unsafe { (*proc).cbs_server = Some(server_id); }
            }
            0
        }
        Err(()) => -1,
    }
}

/// cbs_get_info(server_id): ritorna le informazioni di un server CBS.
/// Return: rax = budget, rdi = period, rsi = remaining (via ipc_override).
pub(super) fn sys_cbs_get_info(server_id: u64) -> i64 {
    match crate::cbs::get_info(server_id as usize) {
        Some(info) => {
            unsafe {
                let p = addr_of_mut!(PERCPU);
                (*p).ipc_override = 1;
                (*p).ret_rdi = info.period as u64;
                (*p).ret_rsi = info.remaining as u64;
            }
            info.budget as i64
        }
        None => -1,
    }
}

/// Fase 32 — `text_stats()`: contatori shared text per il test (hits/misses/
/// live) + Fase 33 `cow` (fault COW gestiti). Multi-registro come
/// `cbs_get_info`: rax = hits, rdi = misses, rsi = live, rdx = cow.
/// (Il nome resta storico: e' il contatore debug/test della memoria user.)
pub(super) fn sys_text_stats() -> i64 {
    let (hits, misses, live) = crate::text::stats();
    unsafe {
        let p = addr_of_mut!(PERCPU);
        (*p).ipc_override = 1;
        (*p).ret_rdi = misses;
        (*p).ret_rsi = live;
        (*p).ret_rdx = crate::phys_mem::cow_count();
    }
    hits as i64
}

/// Fase 19.1 — `ps_info(pid)`: snapshot del processo per `ps`. 0 se lo slot e'
/// vivo (campi nei registri, layout in `syscall-numbers`), -1 se vuoto o
/// terminato (lo slot si salta, come `ps` salta i PID morti).
pub(super) fn sys_ps_info(pid: usize) -> i64 {
    let snap = match crate::sched::process_ps(pid) {
        Some(s) => s,
        None => return -1,
    };
    // Nome (max 16 B) in rdi+rsi, little-endian (gia' zero-padded in PsSnap).
    let mut lo_b = [0u8; 8];
    let mut hi_b = [0u8; 8];
    lo_b.copy_from_slice(&snap.name[0..8]);
    hi_b.copy_from_slice(&snap.name[8..16]);
    let lo = u64::from_le_bytes(lo_b);
    let hi = u64::from_le_bytes(hi_b);
    // Fase 44a: sospeso = Stopped (2), qualunque sia lo stato sottostante.
    let state = if snap.suspended {
        2u64
    } else {
        match snap.state {
            crate::process::State::Ready => 0u64,
            crate::process::State::Blocked => 1u64,
            crate::process::State::Terminated => return -1, // non dovrebbe accadere
        }
    };
    let ipc = match snap.ipc {
        crate::process::IpcState::None => 0u64,
        crate::process::IpcState::BlockedOnRecv => 1u64,
        crate::process::IpcState::BlockedOnReply => 2u64,
    };
    let parent = snap.parent.map(|p| p as u64 + 1).unwrap_or(0);
    let packed = state | (snap.prio as u64) << 8 | parent << 16 | ipc << 24;
    unsafe {
        let p = addr_of_mut!(PERCPU);
        (*p).ipc_override = 1;
        (*p).ret_rdi = lo;
        (*p).ret_rsi = hi;
        (*p).ret_rdx = packed;
        (*p).ret_r10 = snap.ticks_used;
    }
    0
}
