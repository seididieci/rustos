//! posix-server (Fase 40.3, P1): skeleton supervisionato.
//!
//! In P1 non fa ancora nulla (tabelle fd virtuali, pipe e job control dalla
//! Fase 42): registra il servizio `Posix`, segnala READY a init ed entra in
//! loop event-driven (dorme in `recv`, zero CPU). Lo scheletro esiste per tre
//! motivi: (1) supervisione reale da subito (init lo riavvia alla morte come
//! gli altri servizi); (2) il gate di registro e' testato (t53); (3) le
//! tabelle stub documentano dove vivra' lo stato globale POSIX in Fase 42.
//!
//! Niente ring FS in P1 (nessun protocollo IPC nuovo: handoff via COW, mai
//! via posix-server). Niente manifest hash qui dentro (sarebbe il ciclo
//! Fase 36: il manifest copre userposix, che quindi non deve incorporarlo).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::BTreeMap;
use libr;
use libr::println;

/// Stato globale POSIX (Fase 42+): tabella vfd `(chan, pid) → (fd_reale, ...)`.
/// In P1 solo dichiarata (fondazione visibile, zero contenuto).
#[allow(dead_code)]
struct PosixState {
    vfd: BTreeMap<(u64, i64), (i64, i64)>,
}

libr::entry!(real_main);
fn real_main(_sp: u64) -> ! {
    println!("[posix] starting");

    // Registra il servizio Posix (slot 8, ADR-0030): consentito perche'
    // figlio di init (gate Fase 35). A fallimento: log loud, mai degradato
    // silenzioso (uno squat sullo slot va visto subito nel log di boot).
    if libr::service_register(libr::Service::Posix).is_err() {
        println!("[posix] FAILED to register service Posix");
    } else {
        println!("[posix] registered as service Posix");
    }

    let _state = PosixState { vfd: BTreeMap::new() };

    // READY a init (fire-and-forget come gli altri server): da qui init puo'
    // supervisionare (la tabella `supervised` risolve il pid via service_pid).
    libr::signal_ready(1);
    println!("[posix] skeleton up, hanging in recv");

    loop {
        match libr::recv() {
            // Morte di un peer: nessuno stato per-peer in P1, scarta senza
            // reply (il peer e' morto per definizione).
            Ok(m) if libr::is_exit_notify(&m) => {}
            // Sconosciuti: reply difensiva (pattern wait_job della shell).
            // In P1 nessuno dovrebbe scriverci: se accade, si vede nel log?
            // No: i server non loggano per-messaggio (rumore sotto flood).
            Ok(_) => {
                let _ = libr::reply(0, 0, 0);
            }
            Err(_) => {}
        }
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    println!("[posix] panic");
    libr::exit(1)
}
