//! Processo init (Fase 8.1): primo processo user (PID 1), antenato degli altri.
//!
//! init e' l'unico processo che spawna i servizi user (via syscall `spawn`).
//! Il kernel spawna solo init (parent `None`); tutti gli altri processi user
//! comunicano con init sul canale di nascita creato da spawn (ADR-0008):
//! `spawn` ritorna il channel id verso il figlio, il figlio usa il canale 0
//! (= parent) per rispondere (es. TEST_DONE).

#![no_std]
#![no_main]

extern crate alloc;

use libr;
use libr::{println, print_str};
// Tag di fine-test e servizio-pronto (DocsB): single source in
// `syscall-numbers`, via `libr` (prima duplicati qui).
use libr::{SVC_READY, TEST_DONE};

/// Manifest degli hash dei servizi (Fase 36, identita' misurata, Strato 2 di
/// ADR-0026): generato a build-time da scripts/gen-service-hashes.sh sui
/// `.bin` finali, incluso qui via `VELORDOR_SERVICE_HASHES` (esportata da
/// build-userland.sh; senza, questa compilazione fallisce loud).
include!(env!("VELORDOR_SERVICE_HASHES"));

/// Hash atteso del servizio `bin` (nome display, es. `b"userconsole"`), o
/// `None` se non e' nel manifest: embedded disk/fs (TCB del kernel — init non
/// ha i byte in mano per verificarli, li spawna per nome) e binari di test
/// (solo suite, mai servizi). Solo i servizi da disco sono pinnati.
fn expected_hash(bin: &[u8]) -> Option<u64> {
    match bin {
        b"userconsole" => Some(HASH_USERCONSOLE),
        b"useruptime" => Some(HASH_USERUPTIME),
        b"userdevfs" => Some(HASH_USERDEVFS),
        b"userkbd" => Some(HASH_USERKBD),
        b"usertty" => Some(HASH_USERTTY),
        b"usershell" => Some(HASH_USERSHELL),
        _ => None,
    }
}

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
        Err(_) => {
            println!(" -> FAILED");
            None
        }
    }
}

/// Metadati di un servizio avviabile da disco (Fase 21): `path=None` = binario
/// embedded (spawn per nome, solo disk/fs restano embedded); `path=Some` =
/// file da leggere via FS e spawnare con `spawn_image`. `io`/`prio` servono
/// solo al path da disco (l'embedded li prende dalla tabella kernel).
struct SvcMeta {
    bin: &'static [u8],
    path: Option<&'static str>,
    prio: u8,
    io: &'static [(u16, u16)],
}

/// Porte I/O per i manifest da disco (copia dei range kernel in
/// `user_binary.rs`: init e' TCB e li dichiara al kernel via SpawnMeta).
const VGA_CURSOR_RANGES: &[(u16, u16)] = &[(0x3D4, 0x3D5)];
const KBD_PS2_RANGES: &[(u16, u16)] = &[(0x60, 0x64)];

/// Spawna un servizio da disco (Fase 21): legge il file, costruisce SpawnMeta
/// e chiama `spawn_image`. Ritorna il channel id o None (file mancante,
/// meta invalida, spawn rifiutato). Il boot fallisce loud (panic), la
/// supervisione ritenta con backoff+hold come per gli embedded.
/// Fase 36 (identita' misurata): prima dello spawn ricalcola l'hash dei byte
/// caricati e lo confronta col manifest generato a build-time; mismatch =
/// None (a boot e' panic come sopra; in restart e' retry-con-hold con log
/// loud a ogni finestra — un disco manomesso non diventa mai servizio).
fn spawn_file(meta: &SvcMeta) -> Option<i64> {
    let path = meta.path?;
    print_str!("[init] load ");
    libr::write_raw(path.as_ptr(), path.len());
    let img = match libr::load_file(path) {
        Some(b) if !b.is_empty() => b,
        _ => {
            println!(" -> FAILED (file illeggibile)");
            return None;
        }
    };
    // `checked` = un pinning da manifest esisteva ed e' passato: solo allora
    // il log dice `hash-ok` (mai claim senza verifica).
    let checked = match expected_hash(meta.bin) {
        Some(expected) => {
            if libr::image_hash(&img) != expected {
                println!(" -> FAILED (hash mismatch)");
                return None;
            }
            true
        }
        None => false,
    };
    let name = match core::str::from_utf8(meta.bin) {
        Ok(s) => s,
        Err(_) => return None,
    };
    let sm = match libr::SpawnMeta::new(name, meta.prio, meta.io) {
        Some(m) => m,
        None => return None,
    };
    print_str!(" -> spawn ");
    match libr::spawn_image(&img, &sm) {
        Ok(chan) => {
            if checked {
                println!("{}B hash-ok -> child chan={}", img.len(), chan);
            } else {
                println!("{}B -> child chan={}", img.len(), chan);
            }
            Some(chan)
        }
        Err(_) => {
            println!(" -> FAILED (spawn_image rifiutato)");
            None
        }
    }
}

/// Spawna da manifest (embedded o disco) + log unificato.
fn spawn_entry(meta: &SvcMeta) -> Option<i64> {
    match meta.path {
        None => spawn_child(meta.bin),
        Some(_) => spawn_file(meta),
    }
}

/// Spawna un binario di test da disco (Fase 21: `/test/*.bin`) e aspetta che
/// segnali la fine (IPC TEST_DONE sul canale di nascita). I test girano in
/// SEQUENZA: condividono la ramfs di userfs (path e file di lavoro) e la
/// sequenza rende output e PID deterministici.
/// Gestisce anche le morti dei servizi supervisionati (es. t27 uccide devfs a
/// suite in corso): senza, il restart arriverebbe solo dopo la suite.
fn run_test(meta: &SvcMeta, supervised: &mut [Supervised]) {
    let Some(chan) = spawn_entry(meta) else {
        println!("[init] test mancante, FAIL loud");
        libr::exit(1);
    };
    loop {
        drain_stray_deaths(supervised);
        match libr::recv() {
            Ok(m) if m.channel == chan as u64 && m.tag == TEST_DONE => {
                let _ = libr::reply(TEST_DONE, 0, 0);
                return;
            }
            Ok(m) if m.tag == libr::INIT_BOUNCE => {
                let r = handle_bounce(supervised, m.w0);
                let _ = libr::reply(0, r, 0);
            }
            Ok(m) if m.tag == libr::EXIT_NOTIFY => {
                handle_child_death(supervised, m.w1 as i64, m.w0 as i64);
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
}

/// Bounce di un servizio supervisionato (Fase 35, hardening): uccide il figlio
/// (init e' parent: sempre consentito, anche col kill parent-scoped) e lascia
/// che il restart avvenga per la via normale (EXIT_NOTIFY → restart_service).
/// Ritorna il pid ucciso o `u64::MAX` se il servizio e' ignoto. Chiamato dai
/// loop che ricevono (run_test + supervisore): il chiamante risponde con
/// reply (i test guidano il caos tramite init invece di killare direttamente).
fn handle_bounce(supervised: &[Supervised], svc_disc: u64) -> u64 {
    match supervised.iter().find(|e| e.svc as u64 == svc_disc) {
        Some(e) => {
            println!("[init] bounce: uccido pid={} ({})", e.pid, e.svc as u64);
            let _ = libr::kill(e.pid, 0);
            e.pid as u64
        }
        None => {
            println!("[init] bounce: servizio ignoto ({})", svc_disc);
            u64::MAX
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
/// riavviato dalla sua sorgente (embedded o disco, Fase 21). Solo
/// console/disk/fs/devfs/kbd/tty; gli altri figli (uptime/shell/test) sono
/// loggati ma non riavviati.
struct Supervised {
    meta: &'static SvcMeta,
    svc: libr::Service,
    chan: i64,
    pid: i64,
    restarts: u32,
    window_start: i64,
    held: bool,
}

/// Attesa di `n` tick con spin puri IF=1 a batch (non affama il timer).
/// Attende SVC_READY sul canale di nascita del figlio appena respawnato.
/// Consuma SENZA reply (fire-and-forget, vedi wait_msg). Ritorna false se il
/// figlio muore prima del READY (EXIT_NOTIFY sul suo stesso canale) o se
/// scade il bound (500 tick ~ 5 s, restart atteso ~50): mai wedge il
/// supervisore. La notifica di morte e' consumata qui, il chiamante riprova.
/// Le EXIT_NOTIFY di ALTRI figli (una morte durante un restart) NON si
/// scartano: vanno nello stash e il chiamante le processa (sotto). Scartarle
/// perde restart (osservato t32: userdisk morto durante il restart di devfs
/// → mai riavviato → cascata fino al panic di init).
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
            Ok(m) if m.tag == libr::EXIT_NOTIFY => {
                stash_death(m.w1 as i64, m.w0 as i64);
            }
            Ok(_) => {}
            Err(_) => {}
        }
        if libr::get_ticks() - t0 > 500 {
            return false;
        }
    }
}

/// Morti altrui viste dentro `wait_ready` (vedi sopra): init e' single-thread
/// e single-loop, un array statico basta (cap 8 > 6 servizi supervisionati).
/// Accesso via `addr_of_mut!` (edition 2024: niente `static_mut_refs`).
static mut STRAY_DEATHS: [(i64, i64); STRAY_CAP] = [(0, 0); STRAY_CAP];
static mut N_STRAY: usize = 0;
const STRAY_CAP: usize = 8;

fn stash_death(pid: i64, code: i64) {
    unsafe {
        let n = core::ptr::addr_of_mut!(N_STRAY).read();
        if n < STRAY_CAP {
            core::ptr::addr_of_mut!(STRAY_DEATHS).cast::<(i64, i64)>().add(n).write((pid, code));
            core::ptr::addr_of_mut!(N_STRAY).write(n + 1);
        } else {
            println!("[init] supervisione: stash morti pieno, perdo pid={}", pid);
        }
    }
}

/// Processa le morti stashtate (restart/log come quelle viste in recv).
/// Da chiamare nei loop di attesa (run_test + supervisore) cosi' nessuna
/// morte va persa mentre un restart e' in corso.
fn drain_stray_deaths(supervised: &mut [Supervised]) {
    loop {
        let next = unsafe {
            let n = core::ptr::addr_of_mut!(N_STRAY).read();
            if n == 0 {
                None
            } else {
                core::ptr::addr_of_mut!(N_STRAY).write(n - 1);
                Some(core::ptr::addr_of_mut!(STRAY_DEATHS).cast::<(i64, i64)>().add(n - 1).read())
            }
        };
        match next {
            Some((pid, code)) => handle_child_death(supervised, pid, code),
            None => return,
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
        libr::spin_ticks(20);
        let Some(chan) = spawn_entry(e.meta) else {
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

/// Manifest dei servizi (Fase 21, servizi da disco): ordine = ordine di boot.
/// Solo disk/fs restano embedded (storage-TCB: init li spawna per nome prima
/// che il FS esista); tutto il resto vive in `/bin` su /fat (iniettati a
/// build via mcopy) e parte via `spawn_image`. Priorita': 16 Normal, 1 Low.
const SVC_CONSOLE: SvcMeta = SvcMeta {
    bin: b"userconsole",
    path: Some("/fat/bin/console.bin"),
    prio: 16,
    io: VGA_CURSOR_RANGES,
};
const SVC_UPTIME: SvcMeta = SvcMeta {
    bin: b"useruptime",
    path: Some("/fat/bin/uptime.bin"),
    prio: 1,
    io: &[],
};
const SVC_DEVFS: SvcMeta = SvcMeta {
    bin: b"userdevfs",
    path: Some("/fat/bin/devfs.bin"),
    prio: 16,
    io: &[],
};
const SVC_KBD: SvcMeta = SvcMeta {
    bin: b"userkbd",
    path: Some("/fat/bin/kbd.bin"),
    prio: 16,
    io: KBD_PS2_RANGES,
};
const SVC_TTY: SvcMeta = SvcMeta {
    bin: b"usertty",
    path: Some("/fat/bin/tty.bin"),
    prio: 16,
    io: &[],
};
const SVC_SHELL: SvcMeta = SvcMeta {
    bin: b"usershell",
    path: Some("/fat/bin/shell.bin"),
    prio: 16,
    io: &[],
};
/// Binari di test (Fase 21): `/test` su /fat, caricati solo in suite.
const TEST_FS: SvcMeta = SvcMeta { bin: b"usertestfs", path: Some("/fat/test/testfs.bin"), prio: 16, io: &[] };
const TEST_FAT: SvcMeta = SvcMeta { bin: b"usertestfat", path: Some("/fat/test/testfat.bin"), prio: 16, io: &[] };
const TESTS: SvcMeta = SvcMeta { bin: b"usertests", path: Some("/fat/test/tests.bin"), prio: 16, io: &[] };
/// Bench throughput (Fase 23): `/test` su /fat, solo con feature `bench`
/// (scripts/bench.sh). Ortogonale alla suite: gira anche in produzione
/// (skip_tests attivo), mai nel gate di regressione.
#[cfg(feature = "bench")]
const TEST_BENCH: SvcMeta = SvcMeta { bin: b"userbench", path: Some("/fat/test/bench.bin"), prio: 16, io: &[] };

/// Spawna dal manifest e attende SVC_READY se richiesto. A boot il fallimento
/// e' FAIL LOUD (panic via exit: senza servizi il sistema e' inutilizzabile e
/// un boot silenzioso degradato nasconderebbe l'errore di build).
fn boot_svc(meta: &'static SvcMeta, wait: bool) -> Option<i64> {
    let chan = spawn_entry(meta)?;
    if wait {
        wait_msg(chan, SVC_READY);
    }
    Some(chan)
}

/// Entry di init: spawna i servizi e resta vivo come root della process tree.
libr::entry!(real_main);
fn real_main(_sp: u64) -> ! {
    let my_pid = libr::getpid();
    println!("[init] up, pid={}", my_pid);

    // Spawna i servizi user. Ordine importante + attesa prontezza (SVC_READY
    // fire-and-forget, consumato senza reply). Fase 21: disk/fs sono gli UNICI
    // embedded (storage-TCB: spawn per nome prima che il FS esista); la
    // console NON puo' piu' essere prima (da disco: caricarla richiede userfs
    // gia' pronto — prima era prima solo perche' embedded). Resta comunque
    // prima di kbd, che risolve `Console` per nome:
    // 1. userdisk + attesa READY e userfs SUBITO DOPO + attesa READY.
    // 2. userconsole da disco + attesa READY + uptime.
    // 3. devfs + attesa READY, kbd + attesa READY, tty + attesa READY.
    // A boot ogni spawn mancato e' FAIL LOUD (exit → panic kernel): un
    // sistema senza servizi e' inutilizzabile, mai degradato silenzioso.
    // userdisk PRIMA di userfs (Fase 16): userfs monta /fat via IPC DISK a
    // boot e il suo HELLO richiede Disk gia' registrato. userdisk fa READY
    // subito dopo detection + service_register (prima del mount dei nodi,
    // che aspetta Fs): nessun deadlock. Attesa READY su entrambi (come
    // prima): chi usa il FS parte solo dopo che userfs e' pronto.
    let Some(disk_chan) = spawn_child(b"userdisk") else {
        println!("[init] boot FAILED (disk), panic");
        libr::exit(1);
    };
    wait_msg(disk_chan, SVC_READY);
    let Some(fs_chan) = spawn_child(b"userfs") else {
        println!("[init] boot FAILED (fs), panic");
        libr::exit(1);
    };
    wait_msg(fs_chan, SVC_READY);
    // Console da disco (Fase 21): registra Console + ack subito dopo la
    // registrazione (prima del mount /dev/input che richiede userfs, gia'
    // pronto qui). kbd la risolve per nome al passo 4.
    let Some(console_chan) = boot_svc(&SVC_CONSOLE, true) else {
        println!("[init] boot FAILED (console), panic");
        libr::exit(1);
    };
    let _ = console_chan;
    // uptime: nessuna attesa (solo informativo, come prima).
    boot_svc(&SVC_UPTIME, false);
    let Some(devfs_chan) = boot_svc(&SVC_DEVFS, true) else {
        println!("[init] boot FAILED (devfs), panic");
        libr::exit(1);
    };
    let _ = devfs_chan;
    // 4. userkbd + attesa READY (Fase 15: registra Kbd + mount /dev/kbd; Fs
    //    garantito dal passo 2, quindi riesce subito a boot).
    if boot_svc(&SVC_KBD, true).is_none() {
        println!("[init] boot FAILED (kbd), panic");
        libr::exit(1);
    }
    // 5. usertty + attesa READY (Fase 15: registra /dev/input; /dev/kbd e
    //    /dev/console garantiti dai passi precedenti, riesce subito a boot).
    if boot_svc(&SVC_TTY, true).is_none() {
        println!("[init] boot FAILED (tty), panic");
        libr::exit(1);
    }

    // Tabella supervisione (Fase 14, init-restart): console/fs/devfs/kbd/tty/
    // disk vengono riavviati alla morte (dalla loro sorgente: embedded per
    // disk/fs, disco per gli altri — Fase 21); gli altri figli solo loggati.
    // Costruita prima dei test cosi' anche run_test supervisiona (t27 uccide
    // devfs a suite in corso). NOTA: un restart di userfs wipa la ramfs
    // (fixture dei test) — in suite solo t28 lo uccide (Fase 14.12) e
    // ricostruisce la fixture al restart.
    //
    // Disk/fs embedded: manifest inline (path=None) con gli stessi nomi: il
    // restart riusa `spawn_child` come a boot.
    const META_DISK: SvcMeta = SvcMeta { bin: b"userdisk", path: None, prio: 16, io: &[] };
    const META_FS: SvcMeta = SvcMeta { bin: b"userfs", path: None, prio: 16, io: &[] };
    let mut supervised = [
        Supervised { meta: &SVC_CONSOLE, svc: libr::Service::Console, chan: -1, pid: -1, restarts: 0, window_start: 0, held: false },
        Supervised { meta: &META_DISK, svc: libr::Service::Disk, chan: -1, pid: -1, restarts: 0, window_start: 0, held: false },
        Supervised { meta: &META_FS, svc: libr::Service::Fs, chan: -1, pid: -1, restarts: 0, window_start: 0, held: false },
        Supervised { meta: &SVC_DEVFS, svc: libr::Service::Devfs, chan: -1, pid: -1, restarts: 0, window_start: 0, held: false },
        Supervised { meta: &SVC_KBD, svc: libr::Service::Kbd, chan: -1, pid: -1, restarts: 0, window_start: 0, held: false },
        Supervised { meta: &SVC_TTY, svc: libr::Service::Tty, chan: -1, pid: -1, restarts: 0, window_start: 0, held: false },
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
        run_test(&TEST_FS, &mut supervised);
        run_test(&TEST_FAT, &mut supervised);
        run_test(&TESTS, &mut supervised);
    }

    // Bench throughput (Fase 23, feature `bench`): dopo l'eventuale suite,
    // prima della shell. Mai nel gate (rumore di timing + log dedicato).
    #[cfg(feature = "bench")]
    {
        println!("[init] avvio bench");
        run_test(&TEST_BENCH, &mut supervised);
    }

    if boot_svc(&SVC_SHELL, false).is_none() {
        println!("[init] boot FAILED (shell), panic");
        libr::exit(1);
    }

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
        drain_stray_deaths(&mut supervised);
        match libr::recv() {
            Ok(m) if m.tag == libr::EXIT_NOTIFY => {
                handle_child_death(&mut supervised, m.w1 as i64, m.w0 as i64);
            }
            Ok(m) if m.tag == libr::INIT_BOUNCE => {
                let r = handle_bounce(&supervised, m.w0);
                let _ = libr::reply(0, r, 0);
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
