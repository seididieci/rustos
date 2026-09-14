//! Processo init (Fase 8.1): primo processo user (PID 1), antenato degli altri.
//!
//! init e' l'unico processo che spawna i servizi user (via syscall `spawn`).
//! Il kernel spawna solo init (parent `None`); tutti gli altri processi user
//! comunicano con init sul canale di nascita creato da spawn (ADR-0008):
//! `spawn` ritorna il channel id verso il figlio, il figlio usa il canale 0
//! (= parent) per rispondere (es. TEST_DONE).

#![no_std]
#![no_main]

use libr;
use libr::{println, print_str};

/// Tag di fine-test usato dai binari della test suite per notificare a init
/// che hanno terminato (spawn sequenziale, Fase 9.5).
const TEST_DONE: u64 = 0x7E;
/// Tag "servizio pronto": un servizio (userfs) lo manda a init sul canale di
/// nascita dopo essersi registrato per nome (ADR-0008). init lo usa per
/// sincronizzare il boot: chi usa il FS parte solo dopo che userfs e' pronto.
const SVC_READY: u64 = 0x7D;

/// Attende dal canale `chan` un messaggio con tag `tag` e lo consuma SENZA
/// reply (i READY sono fire-and-forget via send_async: rispondere accoderebbe
/// uno spurious message nel server). Usato per sincronizzare l'avvio.
fn wait_msg(chan: i64, tag: u64) {
    loop {
        match libr::recv() {
            Ok(m) if m.channel == chan as u64 && m.tag == tag => {
                return;
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
}

/// Spawna un binario embedded per nome e logga il canale figlio ottenuto.
/// Ritorna il channel id se lo spawn e' riuscito, altrimenti None.
fn spawn_child(name: &[u8]) -> Option<i64> {
    print_str!("[init] spawn ");
    libr::write_raw(name.as_ptr(), name.len());
    match libr::spawn(name) {
        Ok(chan) => {
            println!(" -> child chan={}", chan);
            Some(chan)
        }
        Err(()) => {
            println!(" -> FAILED");
            None
        }
    }
}

/// Spawna un binario di test e aspetta che segnali la fine (IPC TEST_DONE sul
/// canale di nascita). I test girano in SEQUENZA: condividono la ramfs di
/// userfs (path e file di lavoro) e la sequenza rende output e PID deterministici.
/// Gestisce anche le morti dei servizi supervisionati (es. t27 uccide devfs a
/// suite in corso): senza, il restart arriverebbe solo dopo la suite.
fn run_test(name: &[u8], supervised: &mut [Supervised]) {
    let Some(chan) = spawn_child(name) else {
        return;
    };
    loop {
        match libr::recv() {
            Ok(m) if m.channel == chan as u64 && m.tag == TEST_DONE => {
                let _ = libr::reply(TEST_DONE, 0, 0);
                return;
            }
            Ok(m) if m.tag == libr::EXIT_NOTIFY => {
                handle_child_death(supervised, m.w1 as i64, m.w0 as i64);
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
}

/// Gestisce EXIT_NOTIFY di un figlio: se supervisionato → restart, altrimenti
/// log. Usato sia da run_test che dal loop supervisore (single path).
fn handle_child_death(supervised: &mut [Supervised], pid: i64, code: i64) {
    match supervised.iter_mut().find(|e| e.pid == pid) {
        Some(e) if !e.held => {
            println!(
                "[init] supervisione: pid={} morto (code {}), riavvio",
                pid, code
            );
            restart_service(e);
        }
        Some(_) => {
            println!(
                "[init] supervisione: pid={} morto ma HELD, ignoro",
                pid
            );
        }
        None => {
            println!(
                "[init] child pid={} morto (code {}), non supervisionato",
                pid, code
            );
        }
    }
}

/// Servizio supervisionato da init (Fase 14, init-restart): alla morte viene
/// riavviato. Solo console/disk/fs/devfs/kbd/tty; gli altri figli
/// (uptime/shell/test) sono loggati ma non riavviati.
struct Supervised {
    bin: &'static [u8],
    svc: libr::Service,
    chan: i64,
    pid: i64,
    restarts: u32,
    window_start: i64,
    held: bool,
}

/// Attesa di `n` tick con spin puri IF=1 a batch (non affama il timer).
fn spin_ticks(n: i64) {
    let t0 = libr::get_ticks();
    while libr::get_ticks() - t0 < n {
        for _ in 0..512 {
            core::hint::spin_loop();
        }
    }
}

/// Attende SVC_READY sul canale di nascita del figlio appena respawnato.
/// Consuma SENZA reply (fire-and-forget, vedi wait_msg). Ritorna false se il
/// figlio muore prima del READY (EXIT_NOTIFY sul suo stesso canale) o se
/// scade il bound (500 tick ~ 5 s, restart atteso ~50): mai wedge il
/// supervisore. La notifica di morte e' consumata qui, il chiamante riprova.
fn wait_ready(chan: i64) -> bool {
    let t0 = libr::get_ticks();
    loop {
        match libr::recv() {
            Ok(m) if m.channel == chan as u64 && m.tag == SVC_READY => {
                return true;
            }
            Ok(m) if m.channel == chan as u64 && m.tag == libr::EXIT_NOTIFY => {
                return false;
            }
            Ok(_) => {}
            Err(_) => {}
        }
        if libr::get_ticks() - t0 > 500 {
            return false;
        }
    }
}

/// Riavvia un servizio morto (respawn + attesa prontezza). Backoff anti
/// spawn-storm: 20 tick prima di ogni tentativo; oltre 3 restart in 300 tick
/// il servizio va in hold (stop + log, sistema degradato ma vivo).
fn restart_service(e: &mut Supervised) {
    loop {
        if e.held {
            return;
        }
        let now = libr::get_ticks();
        if now - e.window_start > 300 {
            e.window_start = now;
            e.restarts = 0;
        }
        e.restarts += 1;
        if e.restarts > 3 {
            e.held = true;
            println!("[init] supervisione: restart falliti, HELD (stop)");
            return;
        }
        println!("[init] supervisione: riavvio (tentativo {})", e.restarts);
        spin_ticks(20);
        let Some(chan) = spawn_child(e.bin) else {
            println!("[init] supervisione: spawn FAILED, riprovo");
            continue;
        };
        e.chan = chan;
        if wait_ready(chan) {
            e.pid = libr::service_pid(e.svc).unwrap_or(-1);
            println!("[init] supervisione: riavviato pid={}", e.pid);
            return;
        }
        println!("[init] supervisione: READY mancante (morte/timeout), riprovo");
    }
}

/// Entry di init: spawna i servizi e resta vivo come root della process tree.
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let my_pid = libr::getpid();
    println!("[init] up, pid={}", my_pid);

    // Spawna i servizi user. Ordine importante + attesa prontezza (SVC_READY
    // fire-and-forget, consumato senza reply):
    // 1. userconsole per PRIMO + attesa READY (registra Console: kbd_process
    //    risolve per nome; l'ack arriva subito dopo la registrazione, prima
    //    del mount /dev/input che richiede userfs).
    // 2. userdisk + attesa READY (Fase 16: registra Disk + rileva i dischi;
    //    READY prima del mount nodi, che aspetta Fs) e userfs SUBITO DOPO +
    //    attesa READY (registra Fs; monta /fat via userdisk): tutti i client
    //    FS li risolvono per nome; chi usa il FS parte solo dopo.
    // 3. gli altri dopo: uptime, devfs + attesa READY (registra Devfs + mount
    //    /dev; Fs garantito dal passo 2), poi i test in sequenza (vedi
    //    run_test), usershell per ultimo (interattivo).
    if let Some(console_chan) = spawn_child(b"userconsole") {
        wait_msg(console_chan, SVC_READY);
    }
    // userdisk PRIMA di userfs (Fase 16): userfs monta /fat via IPC DISK a
    // boot e il suo HELLO richiede Disk gia' registrato. userdisk fa READY
    // subito dopo detection + service_register (prima del mount dei nodi,
    // che aspetta Fs): nessun deadlock.
    if let Some(disk_chan) = spawn_child(b"userdisk") {
        wait_msg(disk_chan, SVC_READY);
    }
    if let Some(fs_chan) = spawn_child(b"userfs") {
        wait_msg(fs_chan, SVC_READY);
    }
    spawn_child(b"useruptime");
    if let Some(devfs_chan) = spawn_child(b"userdevfs") {
        wait_msg(devfs_chan, SVC_READY);
    }
    // 4. userkbd + attesa READY (Fase 15: registra Kbd + mount /dev/kbd; Fs
    //    garantito dal passo 2, quindi riesce subito a boot).
    if let Some(kbd_chan) = spawn_child(b"userkbd") {
        wait_msg(kbd_chan, SVC_READY);
    }
    // 5. usertty + attesa READY (Fase 15: registra /dev/input; /dev/kbd e
    //    /dev/console garantiti dai passi precedenti, riesce subito a boot).
    if let Some(tty_chan) = spawn_child(b"usertty") {
        wait_msg(tty_chan, SVC_READY);
    }

    // Tabella supervisione (Fase 14, init-restart): console/fs/devfs/kbd/tty/
    // disk vengono riavviati alla morte; gli altri figli solo loggati. Costruita prima dei
    // test cosi' anche run_test supervisiona (t27 uccide devfs a suite in
    // corso). NOTA: un restart di userfs qui wiperebbe la ramfs (fixture dei
    // test) — in suite nessuno lo uccide; t28 futuro affrontera' il tema.
    let mut supervised = [
        Supervised { bin: b"userconsole", svc: libr::Service::Console, chan: -1, pid: -1, restarts: 0, window_start: 0, held: false },
        Supervised { bin: b"userdisk", svc: libr::Service::Disk, chan: -1, pid: -1, restarts: 0, window_start: 0, held: false },
        Supervised { bin: b"userfs", svc: libr::Service::Fs, chan: -1, pid: -1, restarts: 0, window_start: 0, held: false },
        Supervised { bin: b"userdevfs", svc: libr::Service::Devfs, chan: -1, pid: -1, restarts: 0, window_start: 0, held: false },
        Supervised { bin: b"userkbd", svc: libr::Service::Kbd, chan: -1, pid: -1, restarts: 0, window_start: 0, held: false },
        Supervised { bin: b"usertty", svc: libr::Service::Tty, chan: -1, pid: -1, restarts: 0, window_start: 0, held: false },
    ];
    for e in supervised.iter_mut() {
        e.pid = libr::service_pid(e.svc).unwrap_or(-1);
    }

    // Test suite in sequenza: usertestfs, usertestfat, usertests (Fase 9.5).
    // Di default (feature `skip_tests`, run di produzione) SALTATA: boot
    // veloce dritto alla shell. Con `--no-default-features` (RUN_TESTS=1,
    // run-tests.sh) eseguita come gate di regressione.
    #[cfg(feature = "skip_tests")]
    println!("[init] test suite saltata (production run)");
    #[cfg(not(feature = "skip_tests"))]
    {
        println!("[init] avvio test suite");
        run_test(b"usertestfs", &mut supervised);
        run_test(b"usertestfat", &mut supervised);
        run_test(b"usertests", &mut supervised);
    }

    spawn_child(b"usershell");

    // Supervisore init-restart (Fase 14): se un servizio e' morto prima della
    // supervisione (es. tra boot e run_test), riavvialo subito; poi loop.
    for e in supervised.iter_mut() {
        if e.pid < 0 {
            e.pid = libr::service_pid(e.svc).unwrap_or(-1);
        }
        if e.pid < 0 {
            println!("[init] supervisione: servizio assente all'avvio, riavvio");
            restart_service(e);
        }
    }

    println!("[init] supervisione attiva, hanging in recv");
    loop {
        match libr::recv() {
            Ok(m) if m.tag == libr::EXIT_NOTIFY => {
                handle_child_death(&mut supervised, m.w1 as i64, m.w0 as i64);
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    println!("[init] panic");
    libr::exit(1)
}
