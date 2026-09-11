//! Scheduler unico di rustOS: RT a 32 priorita' + CBS (Fase 11).
//!
//! 32 livelli di priorita' (0 = idle, 31 = massima) con run queue per-priorita'
//! O(1) tramite bitmask `u32` + `leading_zeros()`, piu' Constant Bandwidth
//! Server (`cbs.rs`) per la bandwidth reservation. Esposto come `crate::sched`
//! (vedi main.rs): i chiamanti (main/syscall/user_binary/process) usano
//! `crate::sched::*` senza conoscere i dettagli RT.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

use crate::context::CpuContext;
use crate::process::{Process, State};
use x86_64::instructions::hlt;

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

struct Scheduler {
    processes: Vec<Process>,
    current: Option<usize>,
    ticks_current: u64,
    round_robin: usize,
    next_id: usize,
    /// free_pids: bit i = PID i libero (processo reclamato, riusabile).
    free_pids: u32,
    /// Coda di reclaim (Fase 14): PID Terminated in attesa di teardown
    /// differito. Fixed-size (max `MAX_PIDS`): nessuna allocazione nel
    /// percorso di exit/kill.
    reclaim_q: [usize; MAX_PIDS],
    reclaim_head: usize,
    reclaim_len: usize,
    /// ready_by_prio[p] : bit i = processo PID i pronto al livello p.
    ready_by_prio: [u32; 32],
    /// ready_prio_mask : bit p = almeno un processo pronto al livello p.
    ready_prio_mask: u32,
}

static INITIALIZED: AtomicBool = AtomicBool::new(false);
static SCHED: Mutex<Option<Scheduler>> = Mutex::new(None);



/// Contesto del "processo" di boot (main). Non e' un vero PCB: serve solo a
/// salvare lo stato di main al primo switch, quando `current` e' ancora None.
/// Main non viene mai rischedulato (resta in `hlt` nel suo loop).
static mut BOOT_CONTEXT: CpuContext = CpuContext::ZERO;

impl Scheduler {
    fn new() -> Self {
        Self {
            processes: Vec::new(),
            current: None,
            ticks_current: 0,
            round_robin: 0,
            next_id: 0,
            free_pids: 0,
            reclaim_q: [0; MAX_PIDS],
            reclaim_head: 0,
            reclaim_len: 0,
            ready_by_prio: [0u32; 32],
            ready_prio_mask: 0,
        }
    }

    /// Alloca un PID: riusa prima i PID liberati dal reclaim, poi cresce
    /// fino a `MAX_PIDS`. `None` se tutti i 32 sono occupati.
    fn alloc_pid(&mut self) -> Option<usize> {
        if self.free_pids != 0 {
            let pid = self.free_pids.trailing_zeros() as usize;
            self.free_pids &= !(1u32 << pid);
            return Some(pid);
        }
        if self.next_id < MAX_PIDS {
            let pid = self.next_id;
            self.next_id += 1;
            return Some(pid);
        }
        None
    }

    /// Inserisce (o sovrascrive, se il PID era riusato) il processo.
    fn place_process(&mut self, id: usize, process: Process) {
        if id < self.processes.len() {
            self.processes[id] = process;
        } else {
            self.processes.push(process);
        }
    }

    /// Rimette a disposizione un PID allocato ma non utilizzato (spawn
    /// fallito dopo `alloc_pid`).
    fn release_pid(&mut self, pid: usize) {
        self.free_pids |= 1u32 << pid;
    }

    fn push_reclaim(&mut self, pid: usize) {
        if self.reclaim_len >= MAX_PIDS {
            // Coda satura (cascata massiccia): libera subito i gia' pronti.
            self.drain_reclaim();
        }
        if self.reclaim_len >= MAX_PIDS {
            crate::serial_println!("[sched] reclaim queue piena (pid {})", pid);
            return;
        }
        let tail = (self.reclaim_head + self.reclaim_len) % MAX_PIDS;
        self.reclaim_q[tail] = pid;
        self.reclaim_len += 1;
    }

    fn set_ready(&mut self, pid: usize) {
        if pid < 32 && pid < self.processes.len() {
            let p = self.processes[pid].priority.0 as usize;
            self.ready_by_prio[p] |= 1u32 << pid;
            self.ready_prio_mask |= 1u32 << p;
        }
    }

    fn clear_ready(&mut self, pid: usize) {
        if pid < 32 {
            if let Some(proc) = self.processes.get(pid) {
                let p = proc.priority.0 as usize;
                self.ready_by_prio[p] &= !(1u32 << pid);
                if self.ready_by_prio[p] == 0 {
                    self.ready_prio_mask &= !(1u32 << p);
                }
            }
        }
    }

    fn pick_next(&mut self) -> Option<usize> {
        if self.ready_prio_mask == 0 {
            return None;
        }
        // Livello di priorita' piu' alto con almeno un processo pronto.
        let p = 31 - self.ready_prio_mask.leading_zeros() as usize;
        let mask = self.ready_by_prio[p];
        let total = mask.count_ones() as usize;
        let bit_idx = (self.round_robin as usize) % total;
        // Trova il bit_idx-esimo bit impostato (round-robin).
        let mut m = mask;
        for _ in 0..bit_idx {
            m &= m - 1;
        }
        let bit = (m & (!m + 1)).trailing_zeros() as usize;
        // Unico avanzamento di round_robin (una volta per selezione): NON
        // incrementarlo anche in switch_to, altrimenti con un numero pari di
        // pronti meta' morirebbe di fame per sempre (parita' bloccata).
        self.round_robin += 1;
        Some(bit)
    }
}

pub fn init() {
    let mut guard = SCHED.lock();
    *guard = Some(Scheduler::new());
    INITIALIZED.store(true, Ordering::Release);
    crate::serial_println!("[sched_rt] init (preemptive, 32-prio + RR, quantum {} tick)", QUANTUM_TICKS);
}

pub fn spawn(name: &'static str, priority: Priority, entry: crate::process::ProcessFn, parent: Option<usize>, parent_chan: Option<usize>) -> Option<usize> {
    let mut guard = SCHED.lock();
    let sched = guard.as_mut().expect("scheduler non inizializzato");

    let id = sched.alloc_pid()?;
    let process = match Process::create(id, name, priority, entry, parent, parent_chan, &[]) {
        Some(p) => p,
        None => {
            sched.release_pid(id);
            return None;
        }
    };
    sched.place_process(id, process);
    sched.set_ready(id);
    let p = &sched.processes[id];
    crate::serial_println!(
        "[sched_rt] process '{}' (id {}), {:?} | cr3={:#x} rsp0={:#x}",
        name, id, priority, p.cr3, p.kernel_stack_top
    );
    Some(id)
}

pub unsafe fn create_user(
    name: &'static str,
    priority: Priority,
    code_phys: u64,
    code_frames: usize,
    entry: u64,
    parent: Option<usize>,
    parent_chan: Option<usize>,
    io_ranges: &'static [(u16, u16)],
) -> Option<usize> {
    let mut guard = SCHED.lock();
    let sched = guard.as_mut().expect("scheduler non inizializzato");

    let id = sched.alloc_pid()?;
    let process = match unsafe {
        Process::create_user(id, name, priority, code_phys, code_frames, entry, parent, parent_chan, io_ranges)
    } {
        Some(p) => p,
        None => {
            sched.release_pid(id);
            return None;
        }
    };
    sched.place_process(id, process);
    sched.set_ready(id);
    let p = &sched.processes[id];
    crate::serial_println!(
        "[sched_rt] USER process '{}' (id {}), {:?} | cr3={:#x} rsp0={:#x}",
        name, id, priority, p.cr3, p.kernel_stack_top
    );
    Some(id)
}

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

pub fn on_tick() {
    crate::pit::tick();

    if !INITIALIZED.load(Ordering::Acquire) {
        return;
    }

    // Diagnostica scheduler (feature `sched_debug`, vedi Cargo.toml): snapshot
    // ogni 100 tick. Tenuta nel tree perche' ha gia' diagnosticato uno stallo
    // apparente (era la coda vuota, non lo scheduler).
    #[cfg(feature = "sched_debug")]
    static TICKDBG: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
    #[cfg(feature = "sched_debug")]
    let tn = TICKDBG.fetch_add(1, Ordering::Relaxed);

    let mut guard = SCHED.lock();
    let sched = guard.as_mut().expect("scheduler non inizializzato");

    // Fase 14: teardown differito dei processi Terminated (in coda di
    // reclaim). Gira qui perche' `current` e' un processo vivo: non si libera
    // mai lo stack di un processo mentre si gira ancora su di esso. Prima del
    // replenish CBS cosi' i frame vengono riusati subito.
    sched.drain_reclaim();

    // CBS: replenishment — se la deadline di un server e' scaduta, resetta
    // il budget e rimetti il processo in ready queue. Prima del decrement
    // cosi' un processo throttled puo' tornare schedulabile nello stesso tick.
    // Rimetto in ready SOLO processi che sono effettivamente Ready o che
    // erano stati throttle-ati: mai risvegliare un processo Terminated (il suo
    // server CBS deve essere stato rilasciato a exit) ne' uno Blocked (in
    // attesa IPC: lo sblocca la reply, non il CBS).
    let replenished = crate::cbs::tick_replenish();
    for pid in replenished {
        if pid < sched.processes.len() {
            let st = sched.processes[pid].state;
            if st == State::Ready || st == State::Blocked {
                if st == State::Ready {
                    sched.set_ready(pid);
                }
            }
        }
    }

    let mut need_switch = match sched.current {
        Some(_) => {
            sched.ticks_current += 1;
            sched.ticks_current >= QUANTUM_TICKS
        }
        None => true,
    };

    // CBS: decrement budget del processo corrente. Se esaurito, toglilo
    // dalla ready queue e forza lo switch.
    if let Some(cur) = sched.current {
        if crate::cbs::tick_budget(cur) {
            sched.clear_ready(cur);
            need_switch = true;
        }
    }

    if !need_switch {
        #[cfg(feature = "sched_debug")]
        if tn % 100 == 0 {
            crate::serial_println!("[sched] tick={} cur={:?} mask={:#x} l16={:#x} rr={}",
                tn, sched.current, sched.ready_prio_mask, sched.ready_by_prio[16], sched.round_robin);
        }
        return;
    }

    let prev = sched.current;
    let next = match sched.pick_next() {
        Some(n) if prev != Some(n) => n,
        _ => {
            #[cfg(feature = "sched_debug")]
            if tn % 100 == 0 {
                crate::serial_println!("[sched] tick={} cur={:?} mask={:#x} l16={:#x} rr={} (no-switch)",
                    tn, sched.current, sched.ready_prio_mask, sched.ready_by_prio[16], sched.round_robin);
            }
            return;
        }
    };

    switch_to(prev, next, guard);
}

pub fn block_current() {
    if !INITIALIZED.load(Ordering::Acquire) {
        return;
    }

    let mut guard = SCHED.lock();
    let action: Option<(Option<usize>, usize)> = {
        let sched = guard.as_mut().expect("scheduler non inizializzato");
        let Some(prev) = sched.current else {
            return;
        };

        if sched.processes[prev].pending_wake {
            sched.processes[prev].pending_wake = false;
            None
        } else {
            sched.processes[prev].state = State::Blocked;
            sched.clear_ready(prev);
            match sched.pick_next() {
                Some(n) if n != prev => Some((Some(prev), n)),
                _ => {
                    sched.processes[prev].state = State::Ready;
                    sched.set_ready(prev);
                    None
                }
            }
        }
    };
    if let Some((prev, next)) = action {
        switch_to(prev, next, guard);
    }
}

pub fn wake(id: usize) {
    if !INITIALIZED.load(Ordering::Acquire) {
        return;
    }
    let mut guard = SCHED.lock();
    if let Some(sched) = guard.as_mut() {
        if id < sched.processes.len() {
            let p = &mut sched.processes[id];
            if p.state == State::Blocked {
                p.state = State::Ready;
                sched.set_ready(id);
            } else {
                p.pending_wake = true;
            }
        }
    }
}

/// Sveglia un driver su IRQ accodandogli una notify (Fase 15, bridge
/// interrupt→IPC): un `wake()` da solo non basta — se il processo dorme in
/// `recv()` con coda vuota, il wake lo rende Ready ma al primo giro, non
/// trovando messaggi, si ri-blocca senza mai tornare in userspace (il dato
/// hardware resterebbe unread). Con un messaggio in coda, `recv()` ritorna e
/// il driver drena l'hardware. Fire-and-forget: se la coda e' piena la notify
/// si perde (il drain successivo recupera comunque — il driver drena SEMPRE
/// l'hardware a ogni giro, anche su wake spurio). Sicuro da IRQ (solo lock
/// SCHED, come `wake`).
pub fn notify_irq(id: usize, tag: u64) {
    if !INITIALIZED.load(Ordering::Acquire) {
        return;
    }
    let mut guard = SCHED.lock();
    if let Some(sched) = guard.as_mut() {
        if id < sched.processes.len() {
            let p = &mut sched.processes[id];
            let _ = p.msg_queue.try_push(crate::process::PendingMsg {
                channel: 0,
                req_id: 0,
                tag,
                w0: 0,
                w1: 0,
            });
            if p.state == State::Blocked {
                p.state = State::Ready;
                sched.set_ready(id);
            } else {
                p.pending_wake = true;
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct IpcResult {
    pub rax: i64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub r10: u64,
}

fn ok_result() -> IpcResult {
    IpcResult { rax: 0, rdi: 0, rsi: 0, rdx: 0, r10: 0 }
}

fn err_result() -> IpcResult {
    IpcResult { rax: -1, rdi: 0, rsi: 0, rdx: 0, r10: 0 }
}

/// Canale 0 = canale di nascita verso il parent (ADR-0008).
fn resolve_chan(cur: usize, chan: usize, sched: &Scheduler) -> Option<usize> {
    if chan == syscall_numbers::CHANNEL_PARENT as usize {
        sched.processes.get(cur)?.parent_chan
    } else {
        Some(chan)
    }
}

/// Assegna il prossimo request-id del processo `pid` (Fase 13).
fn next_req_id(sched: &mut Scheduler, pid: usize) -> i64 {
    let p = &mut sched.processes[pid];
    let id = p.req_next as i64;
    p.req_next += 1;
    id
}

pub fn ipc_send(channel: usize, tag: u64, w0: u64, w1: u64) -> IpcResult {
    loop {
        if !INITIALIZED.load(Ordering::Acquire) {
            return err_result();
        }
        let mut guard = SCHED.lock();
        let sched = guard.as_mut().expect("scheduler non inizializzato");

        let cur = match sched.current {
            Some(c) => c,
            None => return err_result(),
        };
        let chan = match resolve_chan(cur, channel, sched) {
            Some(c) => c,
            None => return err_result(),
        };
        let dest = match crate::channels::peer(chan, cur) {
            Some(p) => p,
            None => return err_result(),
        };

        let req_id = next_req_id(sched, cur);

        {
            let d = &mut sched.processes[dest];
            d.msg_queue.push(crate::process::PendingMsg { channel: chan, req_id, tag, w0, w1 });
            if d.ipc_state == crate::process::IpcState::BlockedOnRecv {
                d.ipc_state = crate::process::IpcState::None;
                d.state = State::Ready;
                sched.set_ready(dest);
            }
        }

        {
            let c = &mut sched.processes[cur];
            c.ipc_state = crate::process::IpcState::BlockedOnReply;
            // Fase 14: ricorda su chi siamo bloccati, cosi' la morte del
            // destinatario ci sblocca con un errore (niente deadlock).
            c.waiting_pid = Some(dest);
            c.state = State::Blocked;
            sched.clear_ready(cur);
        }

        let next = match sched.pick_next() {
            Some(n) if n != cur => n,
            _ => {
                sched.processes[cur].state = State::Ready;
                sched.set_ready(cur);
                sched.processes[cur].ipc_state = crate::process::IpcState::None;
                sched.processes[cur].waiting_pid = None;
                return err_result();
            }
        };
        switch_to(Some(cur), next, guard);

        let reply = {
            let sched = SCHED.lock();
            let s = sched.as_ref().expect("scheduler non inizializzato");
            s.processes[cur].reply_slot
        };
        {
            // Risvegliati (reply arrivata o mittente morto): non aspettiamo
            // piu' nessuno.
            let mut sched = SCHED.lock();
            let s = sched.as_mut().expect("scheduler non inizializzato");
            s.processes[cur].waiting_pid = None;
        }
        return match reply {
            Some(r) => IpcResult { rax: 0, rdi: 0, rsi: r.tag, rdx: r.w0, r10: r.w1 },
            None => err_result(),
        };
    }
}

pub fn ipc_send_async(channel: usize, tag: u64, w0: u64, w1: u64) -> IpcResult {
    if !INITIALIZED.load(Ordering::Acquire) {
        return err_result();
    }
    let mut guard = SCHED.lock();
    let sched = guard.as_mut().expect("scheduler non inizializzato");

    let cur = match sched.current {
        Some(c) => c,
        None => return err_result(),
    };
    let chan = match resolve_chan(cur, channel, sched) {
        Some(c) => c,
        None => return err_result(),
    };
    let dest = match crate::channels::peer(chan, cur) {
        Some(p) => p,
        None => return err_result(),
    };

    let req_id = next_req_id(sched, cur);

    let ok = {
        let d = &mut sched.processes[dest];
        let pushed = d.msg_queue.try_push(crate::process::PendingMsg { channel: chan, req_id, tag, w0, w1 });
        if pushed && d.ipc_state == crate::process::IpcState::BlockedOnRecv {
            d.ipc_state = crate::process::IpcState::None;
            d.state = State::Ready;
            sched.set_ready(dest);
        }
        pushed
    };

    if ok {
        IpcResult { rax: req_id, rdi: 0, rsi: 0, rdx: 0, r10: 0 }
    } else {
        err_result()
    }
}

/// Estrae il prossimo messaggio dalla coda del processo `cur` e prepara il
/// risultato IPC, oppure ritorna `None` se la coda e' vuota (Fase 13).
fn pop_msg(sched: &mut Scheduler, cur: usize) -> Option<IpcResult> {
    if sched.processes[cur].msg_queue.is_empty() {
        return None;
    }
    let m = sched.processes[cur].msg_queue.pop().expect("coda non vuota");
    if m.req_id >= 0 {
        sched.processes[cur].reply_chan = Some(m.channel);
        sched.processes[cur].reply_req = m.req_id;
        Some(IpcResult { rax: 0, rdi: m.channel as u64, rsi: m.tag, rdx: m.w0, r10: m.w1 })
    } else {
        Some(IpcResult { rax: 0, rdi: m.req_id as u64, rsi: m.tag, rdx: m.w0, r10: m.w1 })
    }
}

pub fn ipc_recv() -> IpcResult {
    loop {
        if !INITIALIZED.load(Ordering::Acquire) {
            return err_result();
        }
        let mut guard = SCHED.lock();
        let action: Option<(Option<usize>, usize)> = {
            let sched = guard.as_mut().expect("scheduler non inizializzato");
            let cur = match sched.current {
                Some(c) => c,
                None => return err_result(),
            };
            if let Some(res) = pop_msg(sched, cur) {
                return res;
            }
            sched.processes[cur].ipc_state = crate::process::IpcState::BlockedOnRecv;
            sched.processes[cur].state = State::Blocked;
            sched.clear_ready(cur);
            match sched.pick_next() {
                Some(n) if n != cur => Some((Some(cur), n)),
                _ => {
                    sched.processes[cur].state = State::Ready;
                    sched.set_ready(cur);
                    sched.processes[cur].ipc_state = crate::process::IpcState::None;
                    None
                }
            }
        };
        match action {
            Some((prev, next)) => switch_to(prev, next, guard),
            None => return err_result(),
        }
    }
}

pub fn ipc_recv_nonblock() -> IpcResult {
    if !INITIALIZED.load(Ordering::Acquire) {
        return err_result();
    }
    let mut guard = SCHED.lock();
    let sched = guard.as_mut().expect("scheduler non inizializzato");
    let cur = match sched.current {
        Some(c) => c,
        None => return err_result(),
    };
    match pop_msg(sched, cur) {
        Some(res) => res,
        None => err_result(),
    }
}

pub fn ipc_reply(tag: u64, w0: u64, w1: u64) -> IpcResult {
    if !INITIALIZED.load(Ordering::Acquire) {
        return err_result();
    }
    let mut guard = SCHED.lock();
    let sched = guard.as_mut().expect("scheduler non inizializzato");
    let cur = match sched.current {
        Some(c) => c,
        None => return err_result(),
    };

    let target_chan = match sched.processes[cur].reply_chan {
        Some(c) => c,
        None => return err_result(),
    };
    let reply_req = sched.processes[cur].reply_req;
    let target = match crate::channels::peer(target_chan, cur) {
        Some(t) => t,
        None => return err_result(),
    };

    sched.processes[cur].reply_chan = None;
    sched.processes[cur].reply_req = 0;

    let sync = sched.processes[target].ipc_state == crate::process::IpcState::BlockedOnReply;

    if sync {
        {
            let t = &mut sched.processes[target];
            t.reply_slot = Some(crate::process::PendingReply { tag, w0, w1 });
            t.ipc_state = crate::process::IpcState::None;
            t.state = State::Ready;
        }
        sched.set_ready(target);
    } else {
        let delivered = {
            let t = &mut sched.processes[target];
            let pushed = t.msg_queue.try_push(crate::process::PendingMsg {
                channel: target_chan,
                req_id: -reply_req,
                tag,
                w0,
                w1,
            });
            if pushed && t.ipc_state == crate::process::IpcState::BlockedOnRecv {
                t.ipc_state = crate::process::IpcState::None;
                t.state = State::Ready;
                sched.set_ready(target);
            }
            pushed
        };
        if !delivered {
            crate::serial_println!(
                "[ipc] reply async a pid {} persa (msg_queue piena), req_id={}",
                target, reply_req
            );
        }
    }

    ok_result()
}

#[allow(dead_code)]
pub fn process_of(target: usize) -> Option<*mut Process> {
    let guard = SCHED.lock();
    let sched = guard.as_ref()?;
    if target < sched.processes.len() {
        Some(sched.processes.as_ptr().wrapping_add(target) as *mut Process)
    } else {
        None
    }
}

pub fn process_name(pid: usize) -> Option<&'static str> {
    let guard = SCHED.lock();
    let sched = guard.as_ref()?;
    if pid < sched.processes.len() {
        Some(sched.processes[pid].name)
    } else {
        None
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

pub fn exit_current(code: i64) -> ! {
    if !INITIALIZED.load(Ordering::Acquire) {
        loop {
            hlt();
        }
    }
    let mut guard = SCHED.lock();

    let action: Option<(usize, usize)> = {
        let sched = guard.as_mut().expect("scheduler non inizializzato");
        match sched.current {
            Some(prev) => {
                // Fase 14: morte logica (terminate) + switch via. Il teardown
                // fisico e' differito (`drain_reclaim` in `on_tick`).
                sched.terminate(prev, code);
                match sched.pick_next() {
                    Some(n) if n != prev => Some((prev, n)),
                    _ => None,
                }
            }
            None => None,
        }
    };

    match action {
        Some((prev, next)) => {
            switch_to(Some(prev), next, guard);
            loop {
                hlt();
            }
        }
        None => {
            drop(guard);
            loop {
                hlt();
            }
        }
    }
}

/// `kill(pid, code)`: termina un processo user per la stessa via di `exit`
/// (cleanup differito + cascata sulla discendenza + notifica al parent).
/// Killabile: qualunque processo user tranne init (pid 1), i processi kernel
/// (cr3 = kernel_cr3) e se stesso (per se' usare `exit`). Ritorna `true` se
/// il processo e' stato terminato.
pub fn kill(pid: usize, code: i64) -> bool {
    if !INITIALIZED.load(Ordering::Acquire) {
        return false;
    }
    let mut guard = SCHED.lock();
    let sched = guard.as_mut().expect("scheduler non inizializzato");

    if pid >= sched.processes.len() || pid == 1 || sched.current == Some(pid) {
        return false;
    }
    let p = &sched.processes[pid];
    if p.state == State::Terminated {
        return false;
    }
    if p.cr3 == crate::vmm_user::kernel_cr3() {
        return false; // processo kernel (idle, keyboard)
    }
    let name = p.name;
    sched.terminate(pid, code);
    crate::serial_println!("[kill ] pid {} '{}' ucciso (code {})", pid, name, code);
    true
}

impl Scheduler {
    /// Morte logica del processo `pid` (Fase 14, ADR-0010): marca
    /// `Terminated`, sblocca i mittenti sincroni che attendevano una reply da
    /// `pid`, enumera i peer da notificare (coppie peer/channel salvate nel
    /// PCB per il reclaim), termina TUTTA la discendenza (cascata), libera
    /// canali/servizi/CBS e accoda il processo al reclaim. Il teardown fisico
    /// (stack/TSS/address space) e la notifica EXIT ai peer sono differiti a
    /// `drain_reclaim`. Se `pid` e' il processo corrente, il chiamante deve
    /// poi fare lo switch (vedi `exit_current`).
    fn terminate(&mut self, pid: usize, code: i64) {
        if pid >= self.processes.len() || self.processes[pid].state == State::Terminated {
            return;
        }
        // init e' la radice della process tree: non deve mai morire.
        if pid == 1 {
            panic!("init terminato (pid 1)");
        }

        let name = self.processes[pid].name;

        {
            let p = &mut self.processes[pid];
            p.state = State::Terminated;
            p.exit_code = code;
            p.ipc_state = crate::process::IpcState::None;
            p.reply_chan = None;
            p.reply_req = 0;
            p.reply_slot = None;
            p.waiting_pid = None;
            p.pending_wake = false;
        }
        self.clear_ready(pid);
        crate::cbs::release_pid(pid);

        // Niente notifica QUI: sblocca solo i mittenti bloccati su `pid` e
        // accoda il reclaim. Le notifiche EXIT ai peer avvengono in
        // `reclaim_one`, DOPO il teardown: cosi' quando un peer si sveglia
        // (es. il parent che spawa di nuovo nel churn) le risorse
        // (PID/TSS/frame) sono gia' libere e il pool non si esaurisce.
        self.wake_senders(pid);

        // Cascata: tutta la discendenza muore con il capostipite.
        for child in 0..self.processes.len() {
            if child == pid {
                continue;
            }
            let is_child = self.processes[child].state != State::Terminated
                && self.processes[child].parent == Some(pid);
            if is_child {
                self.terminate(child, code);
            }
        }

        // Notifica unificata: enumera le coppie (peer, channel) PRIMA di
        // `release_pid` (che rimuove i canali) e salvale nel PCB del morente:
        // `reclaim_one` le consuma DOPO il teardown. Il parent e' uno dei peer
        // (la sua coppia porta il birth channel): nessun caso speciale.
        let (peers, npeer) = crate::channels::enumerate_peers(pid);
        {
            let p = &mut self.processes[pid];
            p.die_peers = peers;
            p.die_peer_count = npeer;
        }

        // Canali e slot servizi del morto (prima che il pid torni nel free-set
        // a reclaim).
        crate::channels::release_pid(pid);

        self.push_reclaim(pid);
        crate::serial_println!(
            "[proc ] '{}' pid {} terminato (code {}), reclaim accodato",
            name, pid, code
        );
    }

    /// Sblocca i processi bloccati in `send` sincrono in attesa di una reply
    /// dal morente `pid`: tornano da `ipc_send` con errore e possono rifare
    /// un `service_lookup` (niente deadlock client-su-servizio-morto).
    fn wake_senders(&mut self, pid: usize) {
        for i in 0..self.processes.len() {
            if i == pid {
                continue;
            }
            let blocked_on_dead = self.processes[i].state == State::Blocked
                && self.processes[i].ipc_state == crate::process::IpcState::BlockedOnReply
                && self.processes[i].waiting_pid == Some(pid);
            if blocked_on_dead {
                let p = &mut self.processes[i];
                p.ipc_state = crate::process::IpcState::None;
                p.waiting_pid = None;
                p.reply_slot = None;
                p.state = State::Ready;
                self.set_ready(i);
            }
        }
    }

    /// Teardown differito dei processi in coda di reclaim: libera lo stack
    /// kernel, lo slot TSS e (per i processi user) l'address space (foglie
    /// `owned` + page table), poi rimette il PID nel free-set per il riuso.
    /// Gira da `on_tick`: `current` e' sempre un processo vivo, quindi non si
    /// libera mai lo stack del processo su cui si sta eseguendo.
    fn drain_reclaim(&mut self) {
        while self.reclaim_len > 0 {
            let pid = self.reclaim_q[self.reclaim_head];
            self.reclaim_head = (self.reclaim_head + 1) % MAX_PIDS;
            self.reclaim_len -= 1;
            self.reclaim_one(pid);
        }
    }

    /// Teardown di un singolo PID Terminated (skip se non reclamabile).
    fn reclaim_one(&mut self, pid: usize) {
        if pid >= self.processes.len() {
            return;
        }
        if self.current == Some(pid) {
            return; // mai liberare il processo in esecuzione
        }
        if self.processes[pid].state != State::Terminated {
            return;
        }

        let (die_peers, npeer, name, stack_base, cr3, exit_code, tss_slot) = {
            let p = &self.processes[pid];
            (p.die_peers, p.die_peer_count, p.name, p.stack_base, p.cr3, p.exit_code, p.tss_slot)
        };

        crate::phys_mem::free_contiguous(stack_base, crate::process::STACK_FRAMES);
        crate::gdt::free_tss_slot(tss_slot);

        let is_user = cr3 != crate::vmm_user::kernel_cr3();
        if is_user {
            unsafe { crate::vmm_user::teardown_user_space(cr3, pid) };
        }

        // Notifica EXIT UNIFICATA a tutti i peer DOPO il teardown: parent,
        // client e server ricevono tutti lo stesso messaggio sul canale che
        // li collegava al morente (single path). Quando un peer si sveglia le
        // risorse del morto sono gia' liberate (pool non esauribile nei loop
        // spawn/exit). Peer Terminated (cascata) skippati.
        for i in 0..npeer {
            let (peer_u32, chan_u32) = die_peers[i];
            let peer = peer_u32 as usize;
            if peer >= self.processes.len()
                || self.processes[peer].state == State::Terminated
            {
                continue;
            }
            let msg = crate::process::PendingMsg {
                channel: chan_u32 as usize,
                req_id: 0,
                tag: syscall_numbers::EXIT_NOTIFY,
                w0: exit_code as u64,
                w1: pid as u64,
            };
            if self.processes[peer].msg_queue.try_push(msg) {
                if self.processes[peer].ipc_state
                    == crate::process::IpcState::BlockedOnRecv
                {
                    self.processes[peer].ipc_state = crate::process::IpcState::None;
                    self.processes[peer].state = State::Ready;
                    self.set_ready(peer);
                }
            } else {
                crate::serial_println!(
                    "[reap ] exit-notify per peer {} persa (coda piena), morto {}",
                    peer, pid
                );
            }
        }

        // Il PID torna disponibile SOLO ora: canali/servizi/CBS del morto sono
        // gia' stati rilasciati da `terminate`.
        self.free_pids |= 1u32 << pid;

        crate::serial_println!(
            "[reap ] '{}' pid {} reclamato{}: frame liberi = {}",
            name,
            pid,
            if is_user { " (addr space)" } else { "" },
            crate::phys_mem::free_frames()
        );
    }
}

fn switch_to(
    prev: Option<usize>,
    next: usize,
    mut guard: spin::MutexGuard<Option<Scheduler>>,
) {
    {
        let sched = guard.as_mut().expect("scheduler non inizializzato");
        sched.current = Some(next);
        sched.ticks_current = 0;
    }

    let (cur_ptr, next_ptr): (*mut CpuContext, *mut CpuContext) = {
        let sched = guard.as_mut().expect("scheduler non inizializzato");
        let base = sched.processes.as_mut_ptr();
        let cur = match prev {
            Some(pi) => unsafe { &mut (*base.add(pi)).saved as *mut CpuContext },
            None => core::ptr::addr_of_mut!(BOOT_CONTEXT),
        };
        let next = unsafe { &mut (*base.add(next)).saved as *mut CpuContext };
        (cur, next)
    };

    let (next_cr3, next_kstack_top, next_tss_sel) = unsafe {
        let sched = guard.as_mut().expect("scheduler non inizializzato");
        let n = &*sched.processes.as_ptr().add(next);
        (n.cr3, n.kernel_stack_top, n.tss_sel)
    };

    crate::syscall::set_current(next, next_kstack_top, next_cr3);

    drop(guard);

    crate::gdt::load_process_tss(next_tss_sel);
    unsafe {
        core::arch::asm!("mov cr3, {}", in(reg) next_cr3, options(nostack, preserves_flags));
    }

    unsafe { crate::context::switch_to(cur_ptr, next_ptr) };
}
