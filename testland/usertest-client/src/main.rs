//! usertestcli — helper generico della test suite (testland).
//!
//! La modalita' d'uso e' decisa dall'orchestratore (usertests) via IPC:
//! il primo messaggio ricevuto e' una CFG con `w0=mode`:
//!   - 0 ECHO:     `w1` round di `send(T_REQ, payload)`; ogni reply deve essere
//!                 `2*payload` (identita' per client → rileva cross-talk).
//!   - 1 ZEROREAD: apre `/dev/zero` (con retry) e fa `w1` read da 4096 byte,
//!                 verificando che siano tutti zeri.
//!   - 2 NULLW:    apre `/dev/null`, scrive 512 byte, verifica read==0.
//!   - 3 SRV:      server echo per IPC async (Fase 13): riceve richieste
//!                 `T_REQ` (anche multiple in volo via `send_async` del parent)
//!                 e risponde con reply implicita (`2*w0`); quando arriva
//!                 `T_STOP` termina con `T_DONE`.
//!   - 4 CHURN:    (Fase 14) riserva `w1` KiB di heap, li tocca tutti e poi
//!                 `exit(0)` SENZA T_DONE: il parent osserva il riuso dei PID
//!                 e il no-leak dai frame (notifica EXIT_NOTIFY).
//!   - 5 KILLME:   (Fase 14) resta in busy-wait finche' il parent lo kill(); il
//!                 parent osserva la notifica EXIT_NOTIFY con il code del kill.
//!   - 6 SRVDIE:   (Fase 14, t24) server sacrificale: registra il servizio
//!                 `Test`, poi busy-wait SENZA mai fare recv (i messaggi si
//!                 accumulano, i send sync restano bloccati). Il parent lo
//!                 killa: TUTTI i peer (parent + client via lookup) ricevono
//!                 EXIT_NOTIFY (notifica unificata).
//!   - 7 SYNCWAIT: (Fase 14, t24) client sync sacrificale: risolve `Test`,
//!                 avvisa il parent con T_READY (handshake: il parent killa
//!                 solo dopo, a lookup avvenuto), poi fa send SYNC verso il
//!                 server e resta bloccato. Sbloccato con Err alla morte del
//!                 server, attende l'EXIT_NOTIFY unificata, poi attende il
//!                 via-libera T_GO del parent e riporta T_DONE(w0=1, w1=code).
//!   - 8 MNTDIE:   (Fase 14, t25) driver sacrificale: registra il prefix
//!                 "/tdie" presso userfs e serve il minimo (DEV_OPEN → fake
//!                 fd, DEV_CLOSE → ok). T_READY(w0=1) a registrazione avvenuta.
//!                 Il parent lo killa: userfs deve purgare il mount (altrimenti
//!                 lo stale avvelena resolve_mount anche dopo re-registrazione).
//!   - 9 OPENDIE:  (Fase 14, t26) client sacrificale: apre /dev/null +
//!                 /dev/zero + hello.txt e poi esce SENZA close e SENZA T_DONE.
//!                 Il parent osserva via wait_exit; userfs deve purgare
//!                 rings/ftable (con DEV_CLOSE inoltrato ai driver).
//!   - 10 MAPHAMMER: (Fase 14, t29) martella map_physical di due pagine scratch
//!                 sulla stessa VA (0x4000003E0000) in loop, verificando sempre
//!                 i propri marker. Con un peer che fa lo stesso su pagine
//!                 diverse (stesso numero VA, tabelle diverse) rivela cross-talk
//!                 TLB/mapping: mismatch = FAIL rumoroso via T_DONE(w0).
//!   - 11 FLOOD:    (t30, buon vicinato) client "cattivo vicino": martella
//!                 open+write+close di /dev/null alla massima velocita',
//!                 finche' il parent manda T_STOP (controllato ogni 64 op via
//!                 recv_poll). Intenzionalmente SENZA throttling: riproduce la
//!                 tempesta di open che affamava la registrazione di devfs
//!                 (lezione t27: /dev smontato → open veloci falliti in loop).
//!                 Termina via T_STOP con T_DONE(w0=1, w1=ops).
//!
//! In ogni caso termina con `send(T_DONE, ok, dettagli)` e `exit(0)`.
//! Il processo e' sempre "garantito che risponde": l'orchestratore reply ad
//! ogni messaggio ricevuto per non lasciare il client bloccato.

#![no_std]
#![no_main]

extern crate alloc;
use alloc::vec;

use libr::{print_str, println};

const T_ACK: u64 = 101;
const T_REQ: u64 = 102;
const T_DONE: u64 = 103;
const T_STOP: u64 = 104;
const T_OPENED: u64 = 105;
const T_GO: u64 = 106;
const T_READY: u64 = 107;

const MODE_ECHO: u64 = 0;
const MODE_ZEROREAD: u64 = 1;
const MODE_NULLW: u64 = 2;
const MODE_SRV: u64 = 3;
const MODE_CHURN: u64 = 4;
const MODE_KILLME: u64 = 5;
const MODE_SRVDIE: u64 = 6;
const MODE_SYNCWAIT: u64 = 7;
const MODE_MNTDIE: u64 = 8;
const MODE_OPENDIE: u64 = 9;
const MODE_MAPHAMMER: u64 = 10;
const MODE_FLOOD: u64 = 11;

// Tag DEV_* (driver IPC, speculari a userfs/devfs).
const DEV_OPEN: u64 = 0x20;
const DEV_CLOSE: u64 = 0x23;
const ERR: u64 = !0u64;

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let my_pid = libr::getpid();
    println!("[utcli] pid={} up", my_pid);

    let cfg = match libr::recv() {
        Ok(m) => m,
        Err(_) => {
            println!("[utcli] recv cfg failed");
            libr::exit(1);
        }
    };
    // Il parent e' raggiungibile sul canale di nascita (ADR-0008).
    let parent = libr::CHANNEL_PARENT;
    let mode = cfg.w0;
    let rounds = cfg.w1 as usize;

    // ACK con il nostro pid: l'orchestratore verifica spawn/getpid.
    let _ = libr::reply(T_ACK, libr::getpid() as u64, 0);
    println!("[utcli] pid={} acked, sending done...", my_pid);

    match mode {
        MODE_CHURN => {
            // Lifecycle (Fase 14): materializza `rounds` KiB di heap e termina
            // SENZA T_DONE: il parent segue la notifica EXIT_NOTIFY.
            let ok = churn_heap(rounds);
            libr::exit(if ok { 0 } else { 1 });
        }
        MODE_KILLME => {
            // Lifecycle (Fase 14): dorme in recv finche' il parent lo kill().
            // (Prima: busy-spin a pari priorita' — ogni round-trip FS degli
            // altri processi aspettava i nostri quanti: load da 15t a 3500t
            // in t24. Bloccato si kill() uguale, a costo zero.)
            loop {
                let _ = libr::recv();
            }
        }
        MODE_SRVDIE => {
            // Server sacrificale (t24, notifica unificata): registra `Test`
            // e dorme in recv SENZA MAI RISPONDERE (i sender restano bloccati
            // come col vecchio spin; i messaggi vengono consumati ma senza
            // reply nessuno si sblocca). Se la registrazione fallisce (slot
            // occupato) esci subito: il test fallira' in modo rumoroso (kill
            // su processo gia' morto), mai hang.
            // (Prima: busy-spin — vedi KILLME sopra.)
            if libr::service_register(libr::Service::Test).is_err() {
                println!("[utcli] pid={} srvdie: register Test FAILED", my_pid);
                libr::exit(1);
            }
            println!("[utcli] pid={} srvdie: registered, parking in recv", my_pid);
            loop {
                let _ = libr::recv();
            }
        }
        MODE_SYNCWAIT => {
            // Client sync sacrificale (t24): risolve `Test`, handshake T_READY
            // al parent, poi send SYNC verso il server (che non risponde mai).
            // Sbloccato con Err alla morte del server, attende l'EXIT_NOTIFY
            // unificata, poi attende il via-libera T_GO del parent (che lo
            // manda solo dopo il proprio wait_reply: cosi' il nostro T_DONE
            // non puo' mai anticipare la notifica nella sua coda) e riporta
            // T_DONE(w0=1, w1=code). Ogni deviazione: T_DONE(w0=0, w1=detail).
            let srv = match libr::service_lookup(libr::Service::Test) {
                Ok(c) => c as u64,
                Err(_) => {
                    let _ = libr::send(parent, T_DONE, 0, 10);
                    libr::exit(0);
                }
            };
            if libr::send(parent, T_READY, 1, 0).is_err() {
                libr::exit(1);
            }
            match libr::send(srv, T_REQ, 0xCAFE, 0) {
                Ok(_) => {
                    // Il server non risponde mai: successo impossibile.
                    let _ = libr::send(parent, T_DONE, 0, 11);
                    libr::exit(0);
                }
                Err(_) => {}
            }
            let code = loop {
                match libr::recv() {
                    Ok(m) if libr::is_exit_notify(&m) => break m.w0,
                    Ok(_) => {
                        let _ = libr::reply(T_ACK, 0, 0);
                    }
                    Err(_) => {
                        let _ = libr::send(parent, T_DONE, 0, 12);
                        libr::exit(0);
                    }
                }
            };
            // Via-libera del parent (dopo il suo wait_reply).
            loop {
                match libr::recv() {
                    Ok(m) if m.tag == T_GO => {
                        let _ = libr::reply(T_ACK, 0, 0);
                        break;
                    }
                    Ok(_) => {
                        let _ = libr::reply(T_ACK, 0, 0);
                    }
                    Err(_) => {
                        let _ = libr::send(parent, T_DONE, 0, 13);
                        libr::exit(0);
                    }
                }
            }
            let _ = libr::send(parent, T_DONE, 1, code);
            libr::exit(0);
        }
        MODE_MNTDIE => {
            // Driver sacrificale (t25): registra "/tdie", handshake T_READY e
            // serve il minimo. Se la registrazione fallisce: T_READY(w0=0) +
            // exit(1) — il test fallisce rumoroso, mai hang.
            if libr::fs_register(b"/tdie") < 0 {
                println!("[utcli] pid={} mntdie: fs_register FAILED", my_pid);
                let _ = libr::send(parent, T_READY, 0, 0);
                libr::exit(1);
            }
            println!("[utcli] pid={} mntdie: registered /tdie", my_pid);
            if libr::send(parent, T_READY, 1, 0).is_err() {
                libr::exit(1);
            }
            loop {
                match libr::recv() {
                    Ok(m) => match m.tag {
                        DEV_OPEN => {
                            let _ = libr::reply(0, 1, 0);
                        }
                        DEV_CLOSE => {
                            let _ = libr::reply(0, 0, 0);
                        }
                        _ => {
                            let _ = libr::reply(0, ERR, 0);
                        }
                    },
                    Err(_) => {}
                }
            }
        }
        MODE_OPENDIE => {
            // Client sacrificale (t26): apre e muore senza close.
            let f1 = libr::open("/dev/null", 0);
            let f2 = libr::open("/dev/zero", 0);
            let f3 = libr::open("hello.txt", 0);
            if f1 < 0 || f2 < 0 || f3 < 0 {
                println!("[utcli] opendie: open failed ({},{},{})", f1, f2, f3);
                libr::exit(2);
            }
            libr::exit(0);
        }
        _ => {
            let (ok, detail) = match mode {
                MODE_ECHO => run_echo(parent, rounds),
                MODE_ZEROREAD => run_zeroread(parent, rounds),
                MODE_NULLW => run_nullw(),
                MODE_SRV => run_srv(),
                MODE_MAPHAMMER => run_maphammer(rounds),
                MODE_FLOOD => run_flood(parent),
                _ => (false, 1),
            };
            let _ = libr::send(parent, T_DONE, ok as u64, detail as u64);
            println!("[utcli] pid={} done ok={} detail={}", my_pid, ok, detail);
            libr::exit(0);
        }
    }
}

/// Materializza `kib` KiB di heap on-demand (sbrk + touch ogni pagina) e
/// verifica il contenuto. Usato dal lifecycle test (riuso PID + no frame leak).
fn churn_heap(kib: usize) -> bool {
    let bytes = kib * 1024;
    let mut buf = vec![0u8; bytes];
    let mut ok = true;
    let step = 4096usize;
    let mut i = 0usize;
    while i < bytes {
        buf[i] = (i & 0xFF) as u8;
        i += step;
    }
    let mut j = 0usize;
    while j < bytes {
        if buf[j] != (j & 0xFF) as u8 {
            ok = false;
        }
        j += step;
    }
    drop(buf);
    ok
}

/// Martella map_physical di P2 su VA_X verificando i marker B (t29): con un
/// peer che fa lo stesso su P1 (stessa VA, altre tabelle), un mismatch prova
/// cross-talk di mapping/TLB. Ritorna (ok, mismatches).
fn run_maphammer(rounds: usize) -> (bool, usize) {
    const VA_X: u64 = 0x0000_4000_003E_0000;
    let p2 = libr::MAP_TEST_PHYS + 0x1000;
    let mut bad = 0usize;
    for _ in 0..rounds {
        if libr::map_physical(p2, VA_X, 1).is_err() {
            return (false, 0xFFFF);
        }
        unsafe {
            let p = VA_X as *mut u8;
            for i in 0..64 {
                core::ptr::write_volatile(p.add(i), 0x55);
            }
            for i in 0..64 {
                if core::ptr::read_volatile(p.add(i)) != 0x55 {
                    bad += 1;
                    break;
                }
            }
        }
        if bad > 0 {
            break;
        }
    }
    (bad == 0, bad)
}

/// Client "cattivo vicino" (t30, buon vicinato): martella open+write+close di
/// /dev/null alla massima velocita' (a /dev smontato sono open veloci falliti
/// in loop — lo scenario t27-red). Dopo WARMUP_OPS operazioni segnala T_READY
/// al parent (cosi' il kill avviene a flood stabilizzato, non in startup) e
/// continua finche' arriva T_STOP (ogni 64 op via `recv_poll`). Termina con
/// (true, ops): il chiamante generico invia poi T_DONE(w0=1, w1=ops).
fn run_flood(parent: u64) -> (bool, usize) {
    const WARMUP_OPS: usize = 1000;
    let mut ops = 0usize;
    let mut warmed = false;
    let data = [0x5Au8; 16];
    loop {
        let fd = libr::open("/dev/null", 0);
        if fd >= 0 {
            let _ = libr::write_fs(fd, &data, 16);
            let _ = libr::close(fd);
        }
        ops += 1;
        if !warmed && ops >= WARMUP_OPS {
            warmed = true;
            // Il parent risponde (recv_expect): poi il flood continua.
            let _ = libr::send(parent, T_READY, 1, 0);
        }
        if ops % 64 == 0 {
            match libr::recv_poll() {
                Some(m) if m.tag == T_STOP => {
                    let _ = libr::reply(T_ACK, 0, 0);
                    return (true, ops);
                }
                // Se il parent muore, la suite e' finita: esci in silenzio
                // invece di floodare per sempre (igiene, mai wedge).
                Some(m)
                    if m.channel == libr::CHANNEL_PARENT
                        && libr::is_exit_notify(&m) =>
                {
                    libr::exit(0);
                }
                Some(m) if libr::is_exit_notify(&m) => {}
                Some(_) => {
                    let _ = libr::reply(T_ACK, 0, 0);
                }
                None => {}
            }
        }
    }
}

/// Server echo per IPC async (Fase 13): loop di `recv`; per ogni `T_REQ`
/// risponde con reply implicita `2*w0`. Un `T_STOP` (da raccogliere in coda)
/// fa terminare. Il server non distingue richieste sync da async: risponde
/// sempre; il kernel recapita la reply al parent bloccato (reply_slot) o
/// accodandola come messaggio (req_id negativo) se il parent era async.
fn run_srv() -> (bool, usize) {
    let mut served = 0usize;
    loop {
        match libr::recv() {
            Ok(m) => match m.tag {
                T_REQ => {
                    let _ = libr::reply(0, 2 * m.w0, 0);
                    served += 1;
                }
                T_STOP => {
                    let _ = libr::reply(T_ACK, 0, 0);
                    return (true, served);
                }
                _ => {
                    let _ = libr::reply(T_ACK, 0, 0);
                }
            },
            Err(_) => return (false, served),
        }
    }
}

fn run_echo(parent: u64, rounds: usize) -> (bool, usize) {
    let mut mismatches = 0usize;
    for seq in 0..rounds {
        let payload = (seq + 1) as u64;
        match libr::send(parent, T_REQ, payload, 0) {
            Ok(r) => {
                if r.w0 != 2 * payload {
                    mismatches += 1;
                }
            }
            Err(_) => mismatches += 1,
        }
    }
    (mismatches == 0, mismatches)
}

fn run_zeroread(parent: u64, rounds: usize) -> (bool, usize) {
    // Throttled (Livello 1, buon vicinato): vedi `libr::open_wait`.
    let fd = libr::open_wait("/dev/zero", 0, 1000, libr::POLL_PERIOD_TICKS);
    // Il buffer FS e' per-processo (Fase 9.6), quindi piu' client possono
    // leggere /dev/zero senza corrompersi a vicenda. L'handshake T_OPENED resta
    // come barriera di coordinamento: l'orchestratore attende che tutti abbiano
    // aperto prima di rilasciare le read con T_GO.
    let open_ok = fd >= 0;
    let _ = libr::send(parent, T_OPENED, open_ok as u64, 0);    match libr::recv() {
        Ok(m) if m.tag == T_GO => {
            let _ = libr::reply(T_ACK, 0, 0);
        }
        _ => return (false, 1),
    }
    if fd < 0 {
        println!("[utcli] zeroread: open /dev/zero failed");
        return (false, 1);
    }
    let mut bad = 0usize;
    let mut buf = vec![0u8; 4096];
    for i in 0..rounds {
        let n = libr::read_fs(fd, &mut buf, 4096);
        if n != 4096 {
            bad += 1;
            println!("[utcli] zeroread read#{} n={}", i, n);
            continue;
        }
        if buf.iter().any(|&b| b != 0) {
            bad += 1;
            print_str!("[utcli] zeroread read#{} nonzero: ", i);
            for j in 0..16 {
                print_str!("{:02x} ", buf[j]);
            }
            println!();
        }
    }
    let _ = libr::close(fd);
    (bad == 0, bad)
}

fn run_nullw() -> (bool, usize) {
    let fd = libr::open_wait("/dev/null", 0, 1000, libr::POLL_PERIOD_TICKS);
    if fd < 0 {
        println!("[utcli] nullw: open /dev/null failed");
        return (false, 1);
    }
    let data = [0x5Au8; 512];
    let n = libr::write_fs(fd, &data, 512);
    let mut buf = [0u8; 64];
    let r = libr::read_fs(fd, &mut buf, 64);
    let _ = libr::close(fd);
    (n == 512 && r == 0, 1)
}

#[panic_handler]
fn panic_handler(_info: &core::panic::PanicInfo) -> ! {
    println!("[utcli] panic");
    libr::exit(1)
}
