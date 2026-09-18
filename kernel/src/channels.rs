//! Canali IPC e registro servizi (ADR-0008, IPC per nome).
//!
//! Sostituisce l'indirizzamento per PID con canali opachi + request-id:
//!   - `Channel` = coppia bidirezionale tra due processi (come una
//!     pipe/socketpair). `spawn` crea il "canale di nascita" (il figlio lo
//!     eredita come canale 0 = parent); `service_lookup` crea un canale tra il
//!     client e l'attuale owner del servizio.
//!   - il registro dei servizi mappa `enum Service` → PID owner. La morte
//!     dell'owner libera lo slot e invalida i canali che lo coinvolgono
//!     (premessa per la pulizia completa, Fase 14).
//!   - `request_id` globali correlano una `reply` alla sua `send` (multi-client
//!     senza reply_target "ultimo mittente").
//!
//! Tabelle statiche (nessuna heap), protette da spinlock come lo scheduler.

use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

use syscall_numbers::Service;

/// Pool dei canali.
pub const MAX_CHANNELS: usize = 128;

#[derive(Clone, Copy, Debug)]
pub struct Channel {
    /// Endpoint A (pid).
    pub a: usize,
    /// Endpoint B (pid).
    pub b: usize,
    /// Falso quando uno dei due endpoint e' terminato (canale da invalidare).
    pub alive: bool,
}

static CHANNELS: Mutex<[Option<Channel>; MAX_CHANNELS]> = Mutex::new([None; MAX_CHANNELS]);
static CHAN_NEXT: AtomicUsize = AtomicUsize::new(1);

/// Owner per servizio: slot (discriminant Service) → pid che lo ha registrato.
/// `0` = libero (i pid partono da 1: init e' il primo processo user).
static SERVICE_OWNER: Mutex<[usize; syscall_numbers::SERVICE_COUNT]> =
    Mutex::new([0; syscall_numbers::SERVICE_COUNT]);

/// Crea un canale bidirezionale tra `a` e `b`. Ritorna il channel id, o `None`
/// se il pool e' esaurito. Lo slot 0 NON si assegna mai: l'id 0 e' il
/// sentinella `CHANNEL_PARENT` (canale di nascita) e un id numerico 0
/// verrebbe risolto al canale del parent invece che al peer (osservato:
/// lookup che ritorna 0 → messaggi al processo sbagliato + reply fantasma
/// da chi li scarta come stray). 127 slot utili su 128, bound invariato.
pub fn alloc(a: usize, b: usize) -> Option<usize> {
    if let Some(id) = find(a, b) {
        return Some(id);
    }
    let mut pool = CHANNELS.lock();
    for _ in 0..MAX_CHANNELS {
        let id = CHAN_NEXT.fetch_add(1, Ordering::Relaxed) % MAX_CHANNELS;
        if id == 0 {
            continue; // sentinella CHANNEL_PARENT: mai assegnare
        }
        if pool[id].is_none() {
            pool[id] = Some(Channel { a, b, alive: true });
            return Some(id);
        }
    }
    None
}

/// Cerca un canale vivo esistente tra `a` e `b` (in entrambi i versi).
/// Evita il burn di slot: `service_lookup` ripetuti tra gli stessi peer
/// riusano il canale invece di allocarne uno nuovo ogni volta (il pool e'
/// finito a 128 slot proprio per questo, Fase 15).
pub fn find(a: usize, b: usize) -> Option<usize> {
    let pool = CHANNELS.lock();
    pool.iter().enumerate().find_map(|(id, slot)| match slot {
        Some(ch) if ch.alive && ((ch.a == a && ch.b == b) || (ch.a == b && ch.b == a)) => {
            Some(id)
        }
        _ => None,
    })
}

/// Ritorna l'endpoint del canale `id` diverso da `me` (il peer), oppure `None`
/// se il canale non esiste, non e' vivo o `me` non ne fa parte.
pub fn peer(id: usize, me: usize) -> Option<usize> {
    let pool = CHANNELS.lock();
    match pool.get(id) {
        Some(Some(ch)) if ch.alive => {
            if ch.a == me {
                Some(ch.b)
            } else if ch.b == me {
                Some(ch.a)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Enumera le coppie `(peer, channel_id)` dei canali vivi che coinvolgono
/// `pid` (notifica unificata di morte, Fase 14). Deduplica per peer
/// (first-channel-wins: piu' canali verso lo stesso peer collassano in una
/// sola entry) e salta i self-channel. Al massimo `MAX_NOTIFY_PEERS` peer
/// distinti (bound provabile: PID diversi da se' con max 32 concorrenti).
/// Da chiamare in `terminate` PRIMA di `release_pid`; non prende altri lock
/// oltre CHANNELS (sicuro con SCHED held: ordine SCHED→CHANNELS stabilito).
pub fn enumerate_peers(pid: usize) -> ([(u32, u32); crate::process::MAX_NOTIFY_PEERS], usize) {
    let pool = CHANNELS.lock();
    let mut out = [(0u32, 0u32); crate::process::MAX_NOTIFY_PEERS];
    let mut n = 0usize;
    for (id, slot) in pool.iter().enumerate() {
        let Some(ch) = slot else { continue };
        if !ch.alive {
            continue;
        }
        let peer = if ch.a == pid {
            ch.b
        } else if ch.b == pid {
            ch.a
        } else {
            continue;
        };
        if peer == pid {
            continue; // self-channel: notificare se' stessi non ha senso
        }
        let mut dup = false;
        for i in 0..n {
            if out[i].0 == peer as u32 {
                dup = true;
                break;
            }
        }
        if dup {
            continue;
        }
        if n < out.len() {
            out[n] = (peer as u32, id as u32);
            n += 1;
        }
    }
    (out, n)
}

/// Rilascia TUTTO cio' che coinvolge il processo `pid` (Fase 14, ADR-0010):
/// libera gli slot dei canali di cui `pid` e' un endpoint (i peer vedranno
/// errore alla prossima send e potranno rifare un `service_lookup`) e libera
/// gli slot del registro servizi di cui `pid` era owner. Da chiamare alla
/// morte del processo (exit/kill), PRIMA che il pid torni nel free-set.
pub fn release_pid(pid: usize) {
    let mut pool = CHANNELS.lock();
    for slot in pool.iter_mut() {
        if let Some(ch) = slot {
            if ch.a == pid || ch.b == pid {
                *slot = None;
            }
        }
    }
    drop(pool);

    let mut owners = SERVICE_OWNER.lock();
    for o in owners.iter_mut() {
        if *o == pid {
            *o = 0;
        }
    }
}

/// Registra il processo corrente come owner del servizio `service`.
/// Fallisce se lo slot e' gia' occupato da un processo vivo.
pub fn register(service: Service, pid: usize) -> Result<(), ()> {
    let mut owners = SERVICE_OWNER.lock();
    let slot = &mut owners[service as usize];
    if *slot != 0 {
        // Occupato da un processo ancora vivo?
        if crate::sched::process_state(*slot).is_some() {
            return Err(());
        }
    }
    *slot = pid;
    Ok(())
}

/// Owner attuale del servizio `service` (se registrato e vivo).
pub fn lookup(service: Service) -> Option<usize> {
    let owners = SERVICE_OWNER.lock();
    let pid = owners[service as usize];
    if pid != 0 {
        crate::sched::process_state(pid).is_some().then_some(pid)
    } else {
        None
    }
}
