// Split from syscall.rs (byte-identical move; see facade).
use super::entry::current_id;

/// ADR-0008 — `service_register(service)`: il chiamante occupa lo slot del
/// servizio `service`. -1 se gia' occupato da un processo vivo.
pub(super) fn sys_service_register(service_disc: u64) -> i64 {
    let service = match service_from_disc(service_disc) {
        Some(s) => s,
        None => return -1,
    };
    match crate::channels::register(service, current_id() as usize) {
        Ok(()) => {
            crate::serial_println!(
                "[svc] '{}' registrato da pid={}",
                service_name(service), current_id()
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
