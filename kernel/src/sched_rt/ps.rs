// Split from sched_rt.rs (byte-identical move; see facade).
use crate::process::{Process, State};
use super::ctx::SCHED;

/// Stato del processo `pid` (`None` se non esiste o e' terminato).
pub fn process_state(pid: usize) -> Option<State> {
    let guard = SCHED.lock();
    let sched = guard.as_ref()?;
    if pid < sched.processes.len() {
        let st = sched.processes[pid].state;
        if st == State::Terminated { None } else { Some(st) }
    } else {
        None
    }
}

/// Snapshot dei campi di `ps` per il processo `pid` (Fase 19.1): un solo lock,
/// `None` se lo slot e' vuoto o il processo e' terminato (come `process_state`).
/// Nome come copia owned 16 B (Fase 21: embedded o `spawn_image`, vedi
/// `Process::name_str`): PsSnap esce dal lock, niente borrow.
pub struct PsSnap {
    pub name: [u8; 16],
    pub state: State,
    pub prio: u8,
    pub parent: Option<usize>,
    pub ipc: crate::process::IpcState,
    pub ticks_used: u64,
}

/// Copia il nome display in un buffer locale (i log avvengono dopo il
/// rilascio dei borrow sul PCB).
pub(super) fn name_buf(p: &crate::process::Process) -> ([u8; 16], u8) {
    let s = p.name_str();
    let mut b = [0u8; 16];
    let n = s.len().min(16);
    b[..n].copy_from_slice(&s.as_bytes()[..n]);
    (b, n as u8)
}

/// Nome display come &str da un buffer di `name_buf` (sempre UTF-8 valido).
pub(super) fn name_str_of(buf: &([u8; 16], u8)) -> &str {
    core::str::from_utf8(&buf.0[..buf.1 as usize]).unwrap_or("?")
}

pub fn process_ps(pid: usize) -> Option<PsSnap> {
    let guard = SCHED.lock();
    let sched = guard.as_ref()?;
    if pid >= sched.processes.len() {
        return None;
    }
    let p = &sched.processes[pid];
    if p.state == State::Terminated {
        return None;
    }
    Some(PsSnap {
        name: name_buf(p).0,
        state: p.state,
        prio: p.priority.0,
        parent: p.parent,
        ipc: p.ipc_state,
        ticks_used: p.ticks_used,
    })
}

/// Imposta il nome owned del processo (Fase 21, `spawn_image`): il nome arriva
/// dal chiamante, non dalla tabella statica. No-op su pid invalido.
pub fn set_owned_name(pid: usize, raw: &[u8]) {
    let mut guard = SCHED.lock();
    if let Some(sched) = guard.as_mut() {
        if let Some(p) = sched.processes.get_mut(pid) {
            p.set_owned_name(raw);
        }
    }
}

/// Imposta il canale di nascita di `pid` (creato da sys_spawn, ADR-0008).
pub fn set_parent_chan(pid: usize, chan: Option<usize>) {
    let mut guard = SCHED.lock();
    if let Some(sched) = guard.as_mut() {
        if pid < sched.processes.len() {
            sched.processes[pid].parent_chan = chan;
        }
    }
}

/// Canale di nascita del processo `pid` (ADR-0008). `None` se non esiste.
pub fn parent_channel(pid: usize) -> Option<usize> {
    let guard = SCHED.lock();
    let sched = guard.as_ref()?;
    if pid < sched.processes.len() {
        sched.processes[pid].parent_chan
    } else {
        None
    }
}

pub fn process_of(target: usize) -> Option<*mut Process> {
    let guard = SCHED.lock();
    let sched = guard.as_ref()?;
    if target < sched.processes.len() {
        Some(sched.processes.as_ptr().wrapping_add(target) as *mut Process)
    } else {
        None
    }
}

/// Nome display del processo (copia owned 16 B + len, Fase 21): il nome puo'
/// venire dalla tabella embedded (statico) o da `spawn_image` (owned nel PCB,
/// non 'static). `([0;16], 0)` se il pid non esiste.
pub fn process_name(pid: usize) -> ([u8; 16], u8) {
    let guard = SCHED.lock();
    match guard.as_ref().and_then(|s| s.processes.get(pid)) {
        Some(p) => {
            let s = p.name_str();
            let mut b = [0u8; 16];
            let n = s.len().min(16);
            b[..n].copy_from_slice(&s.as_bytes()[..n]);
            (b, n as u8)
        }
        None => ([0u8; 16], 0),
    }
}

pub fn process_cr3(pid: usize) -> Option<u64> {
    let guard = SCHED.lock();
    let sched = guard.as_ref()?;
    if pid < sched.processes.len() {
        Some(sched.processes[pid].cr3)
    } else {
        None
    }
}

/// Identita' misurata dell'immagine del processo `pid` (Fase 36, Strato 2 di
/// ADR-0026): `None` se lo slot e' vuoto o il processo e' terminato (come
/// `process_ps`). I processi kernel (idle) hanno hash 0 = nessuna immagine.
pub fn process_image_hash(pid: usize) -> Option<u64> {
    let guard = SCHED.lock();
    let sched = guard.as_ref()?;
    if pid >= sched.processes.len() {
        return None;
    }
    let p = &sched.processes[pid];
    if p.state == State::Terminated {
        return None;
    }
    Some(p.image_hash)
}
