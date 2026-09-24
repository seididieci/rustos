// Split from sched_rt.rs (byte-identical move; see facade).
use super::*;
use crate::process::State;
use core::sync::atomic::Ordering;
use x86_64::instructions::hlt;
use super::ctx::{SCHED, INITIALIZED, switch_to};
use super::ps::{name_buf, name_str_of};
use super::queue::Scheduler;

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
        return false; // processo kernel (solo idle oltre init)
    }
    let name = name_buf(p);
    sched.terminate(pid, code);
    crate::serial_println!("[kill ] pid {} '{}' ucciso (code {})", pid, name_str_of(&name), code);
    true
}

/// `suspend(pid)` (Fase 44a, job control): congela un processo user (non piu'
/// schedulato) finche' `resume`. Meccanismo neutro (ADR-0025): niente segnali
/// numerati, solo fuori/dentro le ready queue. Gate come `kill`: qualunque
/// processo user tranne init (pid 1), i processi kernel e se stesso.
/// Idempotente (doppio suspend = ok). Ritorna `true` se il processo e'
/// sospeso (o gia' sospeso).
pub fn suspend(pid: usize) -> bool {
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
        return false; // processo kernel (solo idle oltre init)
    }
    let name = name_buf(p);
    {
        let p = &mut sched.processes[pid];
        p.suspended = true;
        if p.state == State::Ready {
            // Fuori dalle ready queue; se Blocked non c'e' da togliere nulla
            // (i wake via `set_ready` lo saltano, i messaggi restano in coda).
            sched.clear_ready(pid);
        }
    }
    crate::serial_println!("[job  ] pid {} '{}' sospeso", pid, name_str_of(&name));
    true
}

/// `resume(pid)` (Fase 44a, job control): rimette in schedulazione un processo
/// sospeso. Stessi gate di `suspend` (parent/init a monte, qui i controlli di
/// esistenza/vitalita'). Idempotente (resume di un running = ok, no-op).
/// Un bloccato in `recv` con messaggi in coda si sveglia subito; un bloccato
/// con coda vuota (o in attesa di reply) resta bloccato e i waker futuri lo
/// riaggiungono (ora `set_ready` funziona di nuovo).
pub fn resume(pid: usize) -> bool {
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
        return false; // processo kernel (solo idle oltre init)
    }
    if !sched.processes[pid].suspended {
        return true;
    }
    let name = name_buf(&sched.processes[pid]);
    {
        let p = &mut sched.processes[pid];
        p.suspended = false;
        if p.ipc_state == crate::process::IpcState::BlockedOnRecv && !p.msg_queue.is_empty() {
            // Messaggi arrivati da sospeso: sveglia ora (`ipc_recv`, che al
            // ritorno dallo switch ricontrolla la coda, li trovera').
            p.ipc_state = crate::process::IpcState::None;
            p.state = State::Ready;
            sched.set_ready(pid);
        } else if p.state == State::Ready {
            sched.set_ready(pid);
        }
        // Blocked (coda vuota o attesa reply): resta; i waker lo riprendono.
    }
    crate::serial_println!("[job  ] pid {} '{}' ripreso", pid, name_str_of(&name));
    true
}

impl Scheduler {
    /// Morte logica del processo `pid` (Fase 14, ADR-0010): marca
    /// `Terminated`, sblocca i mittenti sincroni che attendevano una reply da
    /// `pid`, enumera i peer da notificare (coppie peer/channel salvate nel
    /// PCB per il reclaim), termina la discendenza NON-detached in cascata e
    /// ri-parenta a init i figli detached (Fase 22), libera canali/servizi/CBS
    /// e accoda il processo al reclaim. Il teardown fisico (stack/TSS/address
    /// space) e la notifica EXIT ai peer sono differiti a `drain_reclaim`. Se `pid` e' il processo corrente, il chiamante deve
    /// poi fare lo switch (vedi `exit_current`).
    pub(super) fn terminate(&mut self, pid: usize, code: i64) {
        if pid >= self.processes.len() || self.processes[pid].state == State::Terminated {
            return;
        }
        // init e' la radice della process tree: non deve mai morire.
        if pid == 1 {
            panic!("init terminato (pid 1)");
        }

        let name = name_buf(&self.processes[pid]);

        {
            let p = &mut self.processes[pid];
            p.state = State::Terminated;
            // Il morto non torna (44a): azzera il flag cosi' nessun percorso
            // (wake/reuse) lo vede mai insieme a Terminated.
            p.suspended = false;
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

        // Cascata: tutta la discendenza NON-detached muore con il capostipite.
        // I figli detached (Fase 22, flag dello spawner) sopravvivono e
        // vengono ri-parentati a init (pid 1, che non muore mai): niente
        // orfani con parent morto, niente cascata oltre il flag.
        for child in 0..self.processes.len() {
            if child == pid {
                continue;
            }
            let is_child = self.processes[child].state != State::Terminated
                && self.processes[child].parent == Some(pid);
            if !is_child {
                continue;
            }
            if self.processes[child].detached {
                self.processes[child].parent = Some(1);
                let name = name_buf(&self.processes[child]);
                crate::serial_println!(
                    "[proc ] '{}' pid {} detached: ri-parentato a init (morto parent {})",
                    name_str_of(&name), child, pid
                );
            } else {
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
            name_str_of(&name), pid, code
        );
    }

    /// Sblocca i processi bloccati in `send` sincrono in attesa di una reply
    /// dal morente `pid`: tornano da `ipc_send` con errore e possono rifare
    /// un `service_lookup` (niente deadlock client-su-servizio-morto).
    pub(super) fn wake_senders(&mut self, pid: usize) {
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
    pub(super) fn drain_reclaim(&mut self) {
        while self.reclaim_len > 0 {
            let pid = self.reclaim_q[self.reclaim_head];
            self.reclaim_head = (self.reclaim_head + 1) % MAX_PIDS;
            self.reclaim_len -= 1;
            self.reclaim_one(pid);
        }
    }

    /// Teardown di un singolo PID Terminated (skip se non reclamabile).
    pub(super) fn reclaim_one(&mut self, pid: usize) {
        if pid >= self.processes.len() {
            return;
        }
        if self.current == Some(pid) {
            return; // mai liberare il processo in esecuzione
        }
        if self.processes[pid].state != State::Terminated {
            return;
        }

        let (die_peers, npeer, name, stack_base, cr3, exit_code, tss_slot, text_id) = {
            let p = &self.processes[pid];
            (p.die_peers, p.die_peer_count, name_buf(p), p.stack_base, p.cr3, p.exit_code, p.tss_slot, p.text_id)
        };

        crate::phys_mem::free_contiguous(stack_base, crate::process::STACK_FRAMES);
        crate::gdt::free_tss_slot(tss_slot);

        let is_user = cr3 != crate::vmm_user::kernel_cr3();
        if is_user {
            unsafe { crate::vmm_user::teardown_user_space(cr3, pid) };
        }
        // Fase 32: rilascia la text image condivisa DOPO il teardown (il walk
        // non libera le foglie non-owned; a refcount 0 i frame sono liberati).
        if text_id != 0 {
            crate::text::release(text_id);
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
            name_str_of(&name),
            pid,
            if is_user { " (addr space)" } else { "" },
            crate::phys_mem::free_frames()
        );
    }
}
