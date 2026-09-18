// Split from sched_rt.rs (byte-identical move; see facade).
use crate::process::State;
use core::sync::atomic::Ordering;
use super::ctx::{SCHED, INITIALIZED, switch_to};
use super::queue::Scheduler;

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
