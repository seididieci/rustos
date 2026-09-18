// Split from syscall.rs (byte-identical move; see facade).
use super::entry::current_id;

/// ADR-0008 — `service_register(service)`: il chiamante occupa lo slot del
/// servizio `service`. -1 se gia' occupato da un processo vivo.
/// Fase 35 (hardening): i servizi di sistema si registrano solo da figli di
/// init (tutti i driver veri lo sono): impedisce lo squat a slot libero dopo
/// un kill. `Test` resta aperto (slot sacrificale della suite); `Init` e'
/// registrabile solo da un figlio di init (nessun processo reale lo registra,
/// quindi non cambia nulla — la regola e' uniforme).
pub(super) fn sys_service_register(service_disc: u64) -> i64 {
    let service = match service_from_disc(service_disc) {
        Some(s) => s,
        None => return -1,
    };
    let me = current_id() as usize;
    if service != syscall_numbers::Service::Test {
        let is_init_child = matches!(crate::sched::process_ps(me), Some(s) if s.parent == Some(1));
        if !is_init_child {
            crate::serial_println!(
                "[svc] register '{}' da pid={} rifiutato (non figlio di init)",
                service_name(service), me
            );
            return -1;
        }
    }
    match crate::channels::register(service, me) {
        Ok(()) => {
            crate::serial_println!(
                "[svc] '{}' registrato da pid={}",
                service_name(service), me
            );
            0
        }
        Err(()) => -1,
    }
}

/// ADR-0008 — `service_lookup(service)`: risolve il servizio in un channel
/// verso l'attuale owner. Ritorna il channel id, o -1 se non registrato.
pub(super) fn sys_service_lookup(service_disc: u64) -> i64 {
    let service = match service_from_disc(service_disc) {
        Some(s) => s,
        None => return -1,
    };
    let me = current_id() as usize;
    match crate::channels::lookup(service) {
        Some(owner) => match crate::channels::alloc(me, owner) {
            Some(chan) => chan as i64,
            None => -1,
        },
        None => -1,
    }
}

/// Fase 14 (init-restart) — `service_pid(service)`: ritorna il pid
/// dell'attuale owner del servizio, o -1 se non registrato. Nota: come
/// `lookup`, si fida dello slot owner (azzerato da `release_pid` alla morte;
/// riuso PID da parte di terzi nel mentre = futura generazione, vedi ADR-0010).
pub(super) fn sys_service_pid(service_disc: u64) -> i64 {
    let service = match service_from_disc(service_disc) {
        Some(s) => s,
        None => return -1,
    };
    match crate::channels::lookup(service) {
        Some(owner) => owner as i64,
        None => -1,
    }
}

/// Fase 35 (hardening) — `peer_pid(chan)`: pid del peer del canale `chan`
/// (0 = canale di nascita, come `send`/`recv`), o -1. I server lo usano per
/// attribuire una richiesta a un processo (es. la policy `FS_REGISTER` di
/// userfs: replace di un prefix solo da figli di init). Non rivela nulla in
/// piu' di `ps_info` (gia' pubblico).
pub(super) fn sys_peer_pid(chan: usize) -> i64 {
    let me = current_id() as usize;
    let real = if chan == syscall_numbers::CHANNEL_PARENT as usize {
        match crate::sched::parent_channel(me) {
            Some(c) => c,
            None => return -1,
        }
    } else {
        chan
    };
    match crate::channels::peer(real, me) {
        Some(p) => p as i64,
        None => -1,
    }
}

/// Converti un discriminant in un `Service` valido.
fn service_from_disc(disc: u64) -> Option<syscall_numbers::Service> {
    if disc < syscall_numbers::SERVICE_COUNT as u64 {
        Some(unsafe { core::mem::transmute(disc) })
    } else {
        None
    }
}

/// Nome leggibile di un servizio (per log di debug).
fn service_name(s: syscall_numbers::Service) -> &'static str {
    match s {
        syscall_numbers::Service::Console => "console",
        syscall_numbers::Service::Fs => "fs",
        syscall_numbers::Service::Devfs => "devfs",
        syscall_numbers::Service::Init => "init",
        syscall_numbers::Service::Test => "test",
        syscall_numbers::Service::Kbd => "kbd",
        syscall_numbers::Service::Tty => "tty",
        syscall_numbers::Service::Disk => "disk",
    }
}

/// Coda comune di spawn (ADR-0008): canale di nascita tra parent e figlio.
/// Ritorna il channel id o -1 a pool esaurito (mai panic a boot).
pub(super) fn finish_spawn(parent: usize, pid: usize, log_name: &str) -> i64 {
    match crate::channels::alloc(parent, pid) {
        Some(chan) => {
            crate::sched::set_parent_chan(pid, Some(chan));
            crate::serial_println!("[spawn] '{}' → pid={} canale={}", log_name, pid, chan);
            chan as i64
        }
        None => {
            crate::serial_println!("[syscall] spawn: pool canali esaurito");
            -1
        }
    }
}
