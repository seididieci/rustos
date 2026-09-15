//! usertests — suite di regressione user completa (testland).
//!
//! Orchestratore spawnato da init alla fine del boot (dopo testfs/testfat,
//! prima della shell). Copre: syscall core, heap lazy demand-zero, ramfs,
//! device file (/dev/null, /dev/zero), map_physical (aliasing su pagina
//! scratch kernel), IPC sincrono mono/multi-client (reply_target, fix 9.2.2),
//! devfs remoto concorrente + churn heap (regressione lost-wakeup/overlap) e
//! scheduler (preemption ring-3, priorita').
//!
//! Reporting: riga `[usertests] PASS N/N` (o FAIL) + righe per singolo test.

#![no_std]
#![no_main]

extern crate alloc;
use alloc::vec;
use alloc::vec::Vec;

use libr::println;

// Tags protocollo (speculari a usertest-client / usertest-spin).
const T_CFG: u64 = 100;
const T_ACK: u64 = 101;
const T_REQ: u64 = 102;
const T_DONE: u64 = 103;
const T_STOP: u64 = 104;
const T_OPENED: u64 = 105;
const T_GO: u64 = 106;
const T_READY: u64 = 107;

// Mode client.
const M_ECHO: u64 = 0;
const M_ZERO: u64 = 1;
// Lifecycle (Fase 14).
const M_CHURN: u64 = 4;
const M_KILLME: u64 = 5;
const M_SRVDIE: u64 = 6;
const M_SYNCWAIT: u64 = 7;
const M_MNTDIE: u64 = 8;
const M_OPENDIE: u64 = 9;
const M_MAPHAMMER: u64 = 10;
const M_FLOOD: u64 = 11;

// VA per i test map_physical/aliasing (zona libera tra USER_FS_BUFFER e lo
// heap: 0x4000_0020_0000..0x4000_0040_0000).
const VA_A: u64 = 0x0000_4000_0030_0000;
const VA_B: u64 = 0x0000_4000_0038_0000;
const SPIN_VA: u64 = 0x0000_4000_003C_0000;

const HELLO: &[u8] = b"Hello from Velordor ramfs!\n";

// ── mini-harness ─────────────────────────────────────────────────────

fn report(total: &mut u32, ok: &mut u32, name: &str, pass: bool) {
    *total += 1;
    if pass {
        *ok += 1;
        println!("[usertests] {}: PASS", name);
    } else {
        println!("[usertests] {}: FAIL", name);
    }
}

fn spin_ticks(n: i64) {
    let t0 = libr::get_ticks();
    while libr::get_ticks() - t0 < n {
        core::hint::spin_loop();
    }
}

/// Spawna un helper e gli invia la CFG (modo=w0,param=w1) sul canale di nascita
/// (ADR-0008). Ritorna (canale verso il figlio, ack.w0).
fn spawn_cfg(name: &[u8], mode: u64, param: u64) -> Option<(u64, u64)> {
    let chan = libr::spawn(name).ok()? as u64;
    let ack = libr::send(chan, T_CFG, mode, param).ok()?;
    Some((chan, ack.w0))
}

/// Legge `want` entry di una dir e dice se contiene `needle`.
fn dir_contains(path: &str, needle: &str) -> bool {
    let mut e = [0u8; 2048];
    let n = libr::readdir(path, &mut e, 2048);
    if n < 0 {
        return false;
    }
    let count = n as usize;
    let mut i = 0usize;
    let mut seen = 0usize;
    let mut found = false;
    while i < e.len() && seen < count {
        if e[i] == 0 {
            i += 1;
            continue;
        }
        let start = i;
        while i < e.len() && e[i] != 0 {
            i += 1;
        }
        if i > start {
            let name = core::str::from_utf8(&e[start..i]).unwrap_or("");
            seen += 1;
            if name == needle {
                found = true;
            }
        }
        if i < e.len() && e[i] == 0 {
            i += 1;
        }
        if i < e.len() && e[i] == 0 {
            break;
        }
    }
    found
}

// ── singoli test (ritornano true = pass) ─────────────────────────────

fn t_getpid() -> bool {
    libr::getpid() > 0
}

fn t_ticks() -> bool {
    let t1 = libr::get_ticks();
    spin_ticks(1);
    let t2 = libr::get_ticks();
    // Attendi esplicitamente che il contatore cambi (timer attivo + IF in user).
    let t2b = t2;
    let mut guard = 0u32;
    while libr::get_ticks() == t2b && guard < 1_000_000 {
        guard += 1;
        core::hint::spin_loop();
    }
    t2 >= t1 && libr::get_ticks() > t2b
}

/// Deve girare PRIMA di altre allocazioni heap rilevanti (prime pagine sbrk
/// mai toccate → demand-zero → lette come 0).
fn t_heap_fresh_zero() -> bool {
    let mut a = vec![0u8; 65536];
    let fresh = a.iter().all(|&b| b == 0);
    for i in 0..a.len() {
        a[i] = (i % 251) as u8;
    }
    let mut verify = true;
    for i in (0..a.len()).step_by(997) {
        if a[i] != (i % 251) as u8 {
            verify = false;
            break;
        }
    }
    drop(a);
    fresh && verify
}

fn t_heap_reuse() -> bool {
    let a = vec![0x11u8; 4096];
    drop(a);
    let b = vec![0x22u8; 65536];
    let ok1 = b[0] == 0x22 && b[65535] == 0x22;
    drop(b);
    let c = vec![0x33u8; 65536];
    let ok2 = c[0] == 0x33 && c[65535] == 0x33 && c[32768] == 0x33;
    drop(c);
    ok1 && ok2
}

fn t_spawn_identity() -> bool {
    // spawn ritorna un canale verso il figlio (ADR-0008); il figlio conferma
    // con ACK portando il proprio pid. Il parent non conosce il pid (identita'
    // interna al kernel): verifica che il canale sia valido e che il figlio
    // abbia risposto (ack > 0) e completato (DONE).
    match spawn_cfg(b"usertestcli", M_ECHO, 0) {
        Some((chan, ack_pid)) => {
            // rounds=0 → nessun REQ, solo DONE. Risponde/scarta eventuali
            // residui finche' non arriva il DONE del figlio.
            chan > 0 && ack_pid > 0 && recv_expect(chan, T_DONE)
        }
        None => false,
    }
}

fn t_hello() -> bool {
    let fd = libr::open("hello.txt", 0);
    if fd < 0 {
        return false;
    }
    let mut buf = [0u8; 64];
    let n = libr::read_fs(fd, &mut buf, 64);
    if !(n as usize >= HELLO.len() && buf[..HELLO.len()] == *HELLO) {
        let _ = libr::close(fd);
        return false;
    }
    // Contratto EOF (Fase 18.2-bis): leggere oltre la fine torna 0, non -1
    // (il server scrive sempre il response frame, anche vuoto).
    let mut one = [0u8; 1];
    let eof = libr::read_fs(fd, &mut one, 1);
    let _ = libr::close(fd);
    if eof != 0 {
        println!("[usertests] t6: read oltre EOF = {} (atteso 0)", eof);
        return false;
    }
    true
}

fn read_all(fd: i64, out: &mut Vec<u8>, total: usize) -> bool {
    let mut got = 0usize;
    while got < total {
        let mut chunk = [0u8; 2000];
        let n = libr::read_fs(fd, &mut chunk, 2000);
        if n <= 0 {
            return false;
        }
        out.extend_from_slice(&chunk[..n as usize]);
        got += n as usize;
    }
    got == total
}

fn t_ramfs_write_chunk() -> bool {
    let fd = libr::open("utdata.bin", libr::O_CREAT);
    if fd < 0 {
        return false;
    }
    // 3 chunk da 3000 (9 KiB totali > 1 shared page): multi-call write.
    for c in 0..3u32 {
        let chunk: Vec<u8> = (0..3000).map(|i| (((c as usize) * 7 + i) % 251) as u8).collect();
        if libr::write_fs(fd, &chunk, 3000) != 3000 {
            let _ = libr::close(fd);
            return false;
        }
    }
    let _ = libr::close(fd);

    let fd2 = libr::open("utdata.bin", 0);
    if fd2 < 0 {
        return false;
    }
    let mut all = Vec::new();
    let ok = read_all(fd2, &mut all, 9000);
    let _ = libr::close(fd2);
    if !ok || all.len() != 9000 {
        return false;
    }
    for c in 0..3usize {
        for i in 0..3000 {
            if all[c * 3000 + i] != ((c * 7 + i) % 251) as u8 {
                return false;
            }
        }
    }
    true
}

fn t_ramfs_mkdir() -> bool {
    if libr::mkdir("utdir") < 0 {
        return false;
    }
    dir_contains("/", "utdir")
}

fn t_fs_errors() -> bool {
    // open di path vuoto → -1 (path_len 0).
    let a = libr::open("", 0) < 0;
    // read/write/close su fd inesistente → -1 (fd non nella tabella del server).
    let b = libr::read_fs(-1, &mut [0u8; 8], 8) < 0;
    let c = libr::write_fs(-1, &[0u8; 8], 8) < 0;
    let d = libr::close(-1) < 0;
    a && b && c && d
}

fn t_dev_null() -> bool {
    // Throttled (Livello 1): un device non ancora registrato non giustifica
    // mai una tempesta di open verso userfs.
    let fd = libr::open_wait("/dev/null", 0, 1000, libr::POLL_PERIOD_TICKS);
    if fd < 0 {
        return false;
    }
    let data = [0x5Au8; 512];
    let w = libr::write_fs(fd, &data, 512);
    let mut b = [0u8; 16];
    let r = libr::read_fs(fd, &mut b, 16);
    let _ = libr::close(fd);
    w == 512 && r == 0
}

fn t_dev_zero() -> bool {
    let fd = libr::open_wait("/dev/zero", 0, 1000, libr::POLL_PERIOD_TICKS);
    if fd < 0 {
        return false;
    }
    let mut ok = true;
    let mut buf = vec![0xFFu8; 4096];
    for _ in 0..2 {
        let n = libr::read_fs(fd, &mut buf, 4096);
        if n != 4096 || buf.iter().any(|&b| b != 0) {
            ok = false;
        }
    }
    let _ = libr::close(fd);
    ok
}

fn t_map_alias() -> bool {
    if libr::map_physical(libr::MAP_TEST_PHYS, VA_A, 1).is_err() {
        return false;
    }
    if libr::map_physical(libr::MAP_TEST_PHYS, VA_B, 1).is_err() {
        return false;
    }
    let pa = VA_A as *mut u8;
    let pb = VA_B as *const u8;
    for i in 0..64usize {
        unsafe {
            core::ptr::write_volatile(pa.add(i), (i * 7) as u8);
        }
    }
    for i in 0..64usize {
        let v = unsafe { core::ptr::read_volatile(pb.add(i)) };
        if v != (i * 7) as u8 {
            return false;
        }
    }
    true
}

/// Attende un T_DONE da uno dei canali in `chans`. Risponde col request-id.
/// Risponde a qualunque messaggio (anche da canali estranei, es. un DONE
/// tardivo di un test precedente) per non lasciare mittenti bloccati, ma conta
/// solo un T_DONE da un canale atteso.
/// Ritorna (ok, canale del mittente).
fn recv_done(chans: &[u64]) -> (bool, u64) {
    loop {
        match libr::recv() {
            Ok(m) => {
                let _ = libr::reply(T_ACK, 0, 0);
                if m.tag == T_DONE && chans.contains(&m.channel) {
                    return (m.w0 == 1, m.channel);
                }
                // Messaggio estraneo: risposto, continua ad attendere.
            }
            Err(_) => return (false, 0),
        }
    }
}

/// Attende un messaggio con `tag` dal canale `chan`, rispondendo e scartando
/// qualunque messaggio estraneo arrivi prima (residui di test precedenti).
/// Ritorna true se il messaggio atteso e' arrivato con w0==1.
fn recv_expect(chan: u64, tag: u64) -> bool {
    loop {
        match libr::recv() {
            Ok(m) => {
                let _ = libr::reply(T_ACK, 0, 0);
                if m.tag == tag && m.channel == chan {
                    return m.w0 == 1;
                }
                // Estraneo: risposto e scartato, continua.
            }
            Err(_) => return false,
        }
    }
}

/// Svuota i messaggi residui in coda (es. le notifiche EXIT_NOTIFY dei helper
/// dei test precedenti, che escono dopo il loro T_DONE). Da chiamare all'inizio
/// dei test che usano `recv`/`wait_reply` "stretti". Le EXIT_NOTIFY vengono
/// scartate SENZA reply: il mittente e' morto, rispondere e' concettualmente
/// sbagliato (notifica unificata, Fase 14).
fn drain_stray() {
    while let Some(m) = libr::recv_poll() {
        if !libr::is_exit_notify(&m) {
            let _ = libr::reply(T_ACK, 0, 0);
        }
    }
}

/// Fase 14 — attende sul canale `chan` la notifica `EXIT_NOTIFY` del kernel
/// (il figlio e' morto: w0 = exit code, w1 = pid). Gli estranei vengono
/// ignorati (niente reply: il canale di un figlio morto non ha peer vivo).
fn wait_exit(chan: u64) -> Option<(i64, i64)> {
    loop {
        match libr::recv() {
            Ok(m) if m.channel == chan && libr::is_exit_notify(&m) => {
                return Some((m.w0 as i64, m.w1 as i64));
            }
            Ok(_) => {}
            Err(_) => return None,
        }
    }
}

fn t_ipc_echo() -> bool {
    drain_stray();
    let (chan, _) = match spawn_cfg(b"usertestcli", M_ECHO, 8) {
        Some(x) => x,
        None => return false,
    };
    // 8 REQ + 1 DONE. Il server risponde a ogni REQ col payload raddoppiato:
    // il client verifica che la risposta sia proprio la SUA request *2.
    let mut got = 0usize;
    loop {
        match libr::recv() {
            Ok(m) => {
                if m.tag == T_REQ && m.channel == chan {
                    let _ = libr::reply(T_ACK, m.w0 * 2, 0);
                    got += 1;
                    if got == 8 {
                        break;
                    }
                } else {
                    // Estraneo: rispondi e scarta (residuo di test precedente).
                    let _ = libr::reply(T_ACK, 0, 0);
                }
            }
            Err(_) => return false,
        }
    }
    recv_done(&[chan]).0
}

fn t_ipc_multiclient() -> bool {
    drain_stray();
    let n_clients = 3usize;
    let rounds = 50usize;
    let mut chans = Vec::new();
    for _ in 0..n_clients {
        match spawn_cfg(b"usertestcli", M_ECHO, rounds as u64) {
            Some((c, _)) => chans.push(c),
            None => return false,
        }
    }
    let mut reqs = 0usize;
    let mut done = 0usize;
    let mut ok_done = true;
    while reqs < n_clients * rounds || done < n_clients {
        match libr::recv() {
            Ok(m) => match m.tag {
                T_REQ => {
                    let _ = libr::reply(T_ACK, m.w0 * 2, 0);
                    reqs += 1;
                }
                T_DONE => {
                    let _ = libr::reply(T_ACK, 0, 0);
                    if m.w0 != 1 || !chans.contains(&m.channel) {
                        ok_done = false;
                    }
                    done += 1;
                }
                tag if libr::is_exit_notify(&m) => {
                    // Un helper e' terminato dopo il suo T_DONE (Fase 14):
                    // notifica kernel→parent, niente da rispondere.
                    let _ = tag;
                }
                _ => return false,
            },
            Err(_) => return false,
        }
    }
    ok_done && reqs == n_clients * rounds
}

fn t_devfs_concurrent_churn() -> bool {
    drain_stray();
    let mut chans = Vec::new();
    // Buffer FS per-processo (Fase 9.6): ogni client ha la propria pagina, non
    // serve serializzare le OPEN. L'handshake T_OPENED resta come barriera.
    for _ in 0..3 {
        match spawn_cfg(b"usertestcli", M_ZERO, 30) {
            Some((c, _)) => {
                // Il client apre /dev/zero e notifica; reply per sbloccarlo.
                // `recv_expect` ignora gli estranei (es. EXIT_NOTIFY di helper
                // dei test precedenti, Fase 14) finche' non arriva l'OPENED.
                if !recv_expect(c, T_OPENED) {
                    return false;
                }
                chans.push(c);
            }
            None => return false,
        }
    }
    // Tutti hanno aperto: GO rilascia le read insieme.
    for &c in &chans {
        let _ = libr::send(c, T_GO, 0, 0);
    }
    // Churn heap mentre i client leggono /dev/zero (regressione lazy+IPC).
    let mut churn_ok = true;
    let mut size = 96 * 1024;
    for i in 0..18 {
        let mut v = vec![0u8; size];
        for j in 0..(size / 1024) {
            v[j * 1024] = (i % 250) as u8;
        }
        if v[0] != (i % 250) as u8 || v[size - 1] != 0 {
            churn_ok = false;
        }
        drop(v);
        size = if i % 3 == 0 { 192 * 1024 } else { 96 * 1024 };
    }
    for _ in 0..3 {
        if !recv_done(&chans).0 {
            churn_ok = false;
        }
    }
    churn_ok
}

fn t_sched_preempt() -> bool {
    drain_stray();
    let mapped = libr::map_physical(libr::MAP_TEST_PHYS, SPIN_VA, 1).is_ok();
    let ctr = SPIN_VA as *mut u64;
    unsafe { core::ptr::write_volatile(ctr, 0) };

    let (chan, _) = match spawn_cfg(b"utspin_norm", 25, 1) {
        Some(x) => x,
        None => return false,
    };

    // Parent spinge in ring 3 SENZA mai bloccare: il figlio (Normal) può
    // avanzare solo se il timer lo preempta (RR tra Normal).
    spin_ticks(110);
    let progress = unsafe { core::ptr::read_volatile(ctr) };

    let (done, _dchan) = recv_done(&[chan]);
    mapped && progress >= 5 && done
}

fn t_sched_priority() -> bool {
    // High (budget 20) vs Normal (budget 8): entrambi runnable → l'High deve
    // terminare per primo (una fascia Normal resta affamata finche' c'e' un
    // High). Niente Low qui: i server Normal idle (fs/shell) girano in recv-loop
    // sempre-Ready e affamerebbero una fascia Low.
    drain_stray();
    let (high, _) = match spawn_cfg(b"utspin_high", 20, 0) {
        Some(x) => x,
        None => return false,
    };
    let (norm, _) = match spawn_cfg(b"utspin_norm", 8, 0) {
        Some(x) => x,
        None => return false,
    };
    let (ok1, first) = recv_done(&[high, norm]);
    let (ok2, _second) = recv_done(&[high, norm]);
    ok1 && ok2 && first == high
}

// ── CBS tests (Fase 11.5) ─────────────────────────────────────────────

/// Test admission control CBS: una richiesta oltre il cap (~70%) deve
/// essere rifiutata, una entro il cap deve essere accettata.
fn t_cbs_admission() -> bool {
    // 80% bandwidth → supera il cap → deve fallire.
    if libr::cbs_create(8, 10).is_ok() {
        println!("[usertests] t_cbs_admission: 80% should have been rejected");
        return false;
    }
    // 5% → dentro il cap → deve riuscire.
    let s1 = match libr::cbs_create(1, 20) {
        Ok(id) => id,
        Err(()) => {
            println!("[usertests] t_cbs_admission: cbs_create(1,20) FAILED");
            return false;
        }
    };
    // +70% = 75% totale → deve fallire.
    if libr::cbs_create(7, 10).is_ok() {
        println!("[usertests] t_cbs_admission: 75% total should have been rejected");
        return false;
    }
    // +10% = 15% totale → deve riuscire.
    if libr::cbs_create(1, 10).is_err() {
        println!("[usertests] t_cbs_admission: 15% total should have been accepted");
        return false;
    }
    let _ = s1;
    true
}

/// Test bandwidth CBS: task "audio" con CBS (Q=3, P=10 → 30%) + task hog che
/// satura la CPU (utspin_norm, nessun CBS). Il CBS deve garantire all'audio
/// la sua quota (~30%) anche sotto carico: se il CBS non throttla, audio e
/// hog (stessa priorita' Normal) si spartirebbero ~50/50.
///
/// Ogni task conta i tick PIT che OSSERVA durante il proprio busy-loop e li
/// riporta al parent con T_DONE (w1). Il parent NON fa spin su get_ticks
/// (maschera gli interrupt e affama il timer): resta bloccato in recv e
/// valuta i conteggi riportati dai figli.
fn t_cbs_bandwidth() -> bool {
    drain_stray();
    // Precondizione: il CBS e' sempre attivo (scheduler RT unico). Se la
    // creazione di un server fallisce il test e' FAILED.
    if libr::cbs_create(1, 10).is_err() {
        println!("[usertests] t_cbs_bandwidth: cbs_create FAILED");
        return false;
    }

    // Audio: utcbstest crea server Q=3 P=10 e si attacha; busy-loop di 200
    // tick wall-clock contando i tick osservati (~60 attesi a 30%).
    let (audio_chan, _) = match spawn_cfg(b"utcbstest", 3, 10) {
        Some(x) => x,
        None => {
            println!("[usertests] t_cbs_bandwidth: spawn utcbstest FAILED");
            return false;
        }
    };

    // Hog: utspin_norm senza CBS, budget 300 tick wall-clock: resta attivo
    // per l'intera finestra dell'audio (200) e contende la CPU.
    let (hog_chan, _) = match spawn_cfg(b"utspin_norm", 300, 0) {
        Some(x) => x,
        None => {
            println!("[usertests] t_cbs_bandwidth: spawn hog FAILED");
            return false;
        }
    };

    // Raccogli i due DONE (audio finisce prima dell'hog). Nessuno spin: il
    // parent resta bloccato in recv e i figli girano con il timer libero.
    let mut audio_obs: i64 = -1;
    let mut hog_obs: i64 = -1;
    let mut received = 0u32;
    while received < 2 {
        match libr::recv() {
            Ok(m) => {
                let _ = libr::reply(T_ACK, 0, 0);
                if m.tag == T_DONE && m.channel == audio_chan && audio_obs < 0 {
                    audio_obs = m.w1 as i64;
                    received += 1;
                } else if m.tag == T_DONE && m.channel == hog_chan && hog_obs < 0 {
                    hog_obs = m.w1 as i64;
                    received += 1;
                }
            }
            Err(_) => return false,
        }
    }
    if audio_obs < 0 || hog_obs < 0 {
        return false;
    }

    // Finestra audio = 200 tick. Con CBS 30% → ~60 osservati. Margini ampi
    // [40, 90]: sotto ~20% l'audio sarebbe stato affamato (CBS rotto), sopra
    // ~45% non sarebbe stato throttlato a 30% (avrebbe preso ~50% in RR con
    // l'hog). L'hog (nessun CBS) deve osservare piu' dell'audio.
    let bw_ok = audio_obs >= 40 && audio_obs <= 90 && hog_obs > audio_obs;

    println!("[usertests] t_cbs_bandwidth: audio={}/200 hog={} bw={}",
        audio_obs, hog_obs, if bw_ok { "PASS" } else { "FAIL" });

    bw_ok
}

/// Fase 13 — t20: FS async 1-in-volo. Apre hello.txt (sync), lancia una
/// `read_async` (non bloccante), fa lavoro utile, poi `fs_collect`. Verifica
/// che i dati letti in modo async combacino con il contenuto atteso.
fn t_fs_async() -> bool {
    drain_stray();
    let fd = libr::open("hello.txt", 0);
    if fd < 0 {
        println!("[usertests] t_fs_async: open hello.txt FAILED");
        return false;
    }
    // La read deve stare in un solo frame (<= RING_MAX_PAYLOAD ~4000).
    let req = libr::read_async(fd, 64);
    if req < 0 {
        println!("[usertests] t_fs_async: read_async FAILED (req={})", req);
        let _ = libr::close(fd);
        return false;
    }
    // Lavoro utile mentre userfs risponde: batch di spin puro (IF=1, nessuna
    // syscall nel mezzo) per non affamare il timer.
    for _ in 0..200_000 {
        core::hint::spin_loop();
    }
    let mut buf = [0u8; 128];
    let n = libr::fs_collect(req, &mut buf, 128);
    let _ = libr::close(fd);
    if n as usize >= HELLO.len() && buf[..HELLO.len()] == *HELLO {
        true
    } else {
        println!("[usertests] t_fs_async: collect n={} (atteso >= {})", n, HELLO.len());
        false
    }
}

/// Fase 13 — t21: IPC async su IPC puro (canale di nascita verso un helper
/// server echo MODE_SRV). Due sotto-casi:
///   1) K richieste `send_async` in volo sullo stesso canale, raccolte FIFO con
///      `wait_reply` in ordine → ogni reply vale 2*payload.
///   2) Backpressure: spamma `send_async` finche' la coda del server (cap 8) si
///      riempie → -1 osservato; poi drena le reply in volo e chiude con T_STOP.
fn t_ipc_async() -> bool {
    drain_stray();
    // Helper server echo (modalita' 3): risponde a ogni T_REQ con 2*w0.
    // (srv_pid serve a distinguere la morte DEL server dalle EXIT_NOTIFY
    // tardive di helper precedenti: il parent le riceve tutte.)
    let (chan, srv_pid) = match spawn_cfg(b"usertestcli", 3, 0) {
        Some(x) => x,
        None => {
            println!("[usertests] t_ipc_async: spawn MODE_SRV FAILED");
            return false;
        }
    };

    // ── Sotto-caso 1: K richieste in volo, raccolte FIFO.
    const K: usize = 4;
    let mut req_ids = [0i64; K];
    for i in 0..K {
        let payload = (i as u64) + 100;
        match libr::send_async(chan, T_REQ, payload, 0) {
            Ok(r) => req_ids[i] = r,
            Err(_) => {
                println!("[usertests] t_ipc_async: send_async#{} FAILED", i);
                return false;
            }
        }
    }
    let mut fifo_ok = true;
    for i in 0..K {
        let payload = (i as u64) + 100;
        loop {
            match libr::wait_reply(req_ids[i]) {
                Ok(m) => {
                    if m.req_id != req_ids[i] || m.w0 != 2 * payload {
                        fifo_ok = false;
                    }
                    break;
                }
                Err(libr::WaitReplyError::ServerDied { pid, .. }) if pid != srv_pid => {
                    // Stale: EXIT_NOTIFY tardiva di un helper precedente, non
                    // del nostro server. Consumata, si continua ad attendere.
                    continue;
                }
                Err(libr::WaitReplyError::ServerDied { pid, code }) => {
                    println!(
                        "[usertests] t_ipc_async: echo server died (pid={}, code={})",
                        pid, code
                    );
                    fifo_ok = false;
                    break;
                }
                Err(_) => {
                    fifo_ok = false;
                    break;
                }
            }
        }
    }
    if !fifo_ok {
        println!("[usertests] t_ipc_async: FIFO replies MISMATCH");
        // Chiude comunque il server prima di fallire.
        let _ = libr::send(chan, T_STOP, 0, 0);
        let _ = recv_expect(chan, T_DONE);
        return false;
    }

    // ── Sotto-caso 2: backpressure (coda server piena → -1), poi drenaggio.
    // Il server e' ora di nuovo bloccato in recv. Con send_async il client non
    // cede mai la CPU nel loop → dopo 8 messaggi in coda al server (cap 8) il
    // nono send_async ritorna -1 (deterministico entro il quantum).
    let mut sent_ok = 0usize;
    let mut seen_bp = false;
    let mut bp_reqs = [0i64; 16];
    for i in 0..16 {
        let payload = 1000 + i as u64;
        match libr::send_async(chan, T_REQ, payload, 0) {
            Ok(r) => {
                if sent_ok < 16 {
                    bp_reqs[sent_ok] = r;
                }
                sent_ok += 1;
            }
            Err(_) => {
                seen_bp = true;
                break;
            }
        }
    }
    if !seen_bp || sent_ok == 0 {
        println!("[usertests] t_ipc_async: backpressure NOT observed (ok={})", sent_ok);
        let _ = libr::send(chan, T_STOP, 0, 0);
        let _ = recv_expect(chan, T_DONE);
        return false;
    }
    let mut bp_ok = true;
    for i in 0..sent_ok {
        let payload = 1000 + i as u64;
        loop {
            match libr::wait_reply(bp_reqs[i]) {
                Ok(m) => {
                    if m.req_id != bp_reqs[i] || m.w0 != 2 * payload {
                        bp_ok = false;
                    }
                    break;
                }
                Err(libr::WaitReplyError::ServerDied { pid, .. }) if pid != srv_pid => {
                    // Stale (vedi sopra): consumata, si continua.
                    continue;
                }
                Err(libr::WaitReplyError::ServerDied { pid, code }) => {
                    println!(
                        "[usertests] t_ipc_async: echo server died (pid={}, code={})",
                        pid, code
                    );
                    bp_ok = false;
                    break;
                }
                Err(_) => {
                    bp_ok = false;
                    break;
                }
            }
        }
    }
    if !bp_ok {
        println!("[usertests] t_ipc_async: backpressure replies MISMATCH");
        let _ = libr::send(chan, T_STOP, 0, 0);
        let _ = recv_expect(chan, T_DONE);
        return false;
    }

    // Chiude il server e attende il suo T_DONE.
    if libr::send(chan, T_STOP, 0, 0).is_err() {
        return false;
    }
    recv_expect(chan, T_DONE)
}

// ── Lifecycle tests (Fase 14, ADR-0010) ────────────────────────────

/// t22 — lifecycle churn: spawna e termina molti piu' processi del vecchio
/// limite cumulativo (32 dal boot). Ogni helper CHURN materializza ~2 MiB di
/// heap e poi esce SENZA T_DONE: il parent osserva la notifica EXIT_NOTIFY.
/// Verifica:
///   1. ogni spawn riesce (il riuso dei PID evita l'esaurimento);
///   2. per ogni figlio arriva la EXIT_NOTIFY con exit code 0;
///   3. i PID vengono riusati (pid distinti < spawn totali);
///   4. niente frame leak: se il teardown non liberasse heap/stack i frame si
///      accumulerebbero (2 MiB × 42 = 84 MiB) finche' l'allocazione fallisce
///      e un figlio esplode o uno spawn fallisce.
fn t_lifecycle_churn() -> bool {
    const N: usize = 42;
    const KIB: u64 = 2048; // ~2 MiB materializzati da ogni figlio
    let mut pids = Vec::new();
    for i in 0..N {
        // Spawn + CFG (modo CHURN): l'ACK porta il pid del figlio.
        let (chan, pid) = match spawn_cfg(b"usertestcli", M_CHURN, KIB) {
            Some(x) => x,
            None => {
                println!("[usertests] t22: spawn #{} FAILED (pool esaurito?)", i);
                return false;
            }
        };
        pids.push(pid as i64);
        // Attendi la morte di QUESTO figlio (notifica kernel→parent).
        match wait_exit(chan) {
            Some((code, _)) if code == 0 => {}
            Some((code, _)) => {
                println!("[usertests] t22: child #{} exit code {} (atteso 0)", i, code);
                return false;
            }
            None => return false,
        }
    }
    // Riuso PID: con N > 32 spawn sequenziali i pid devono essersi ripetuti.
    let mut distinct = Vec::new();
    for &p in &pids {
        if !distinct.contains(&p) {
            distinct.push(p);
        }
    }
    let reused = distinct.len() < pids.len();
    println!(
        "[usertests] t22: {} spawn, {} pid distinti (riuso={})",
        pids.len(),
        distinct.len(),
        reused
    );
    reused
}

/// t23 — kill(pid) + notifica exit. Spawna un helper KILLME (busy-wait), ne
/// ricava il pid dall'ACK, lo kill() con un code noto e attende la notifica
/// EXIT_NOTIFY con quel code e quel pid. Infine verifica che il pool non sia
/// esaurito (spawn + exit di un altro helper riescono ancora).
fn t_kill() -> bool {
    drain_stray();
    let (chan, pid) = match spawn_cfg(b"usertestcli", M_KILLME, 0) {
        Some(x) => x,
        None => return false,
    };
    let code = -7i64;
    if libr::kill(pid as i64, code).is_err() {
        println!("[usertests] t23: kill(pid={}) FAILED", pid);
        return false;
    }
    match wait_exit(chan) {
        Some((c, p)) if c == code && p == pid as i64 => {}
        _ => {
            println!(
                "[usertests] t23: exit notify mismatch (atteso code {} pid {})",
                code, pid
            );
            return false;
        }
    }
    // Dopo la kill il pool deve accettare ancora spawn/exit (riuso sicuro).
    let (chan2, _pid2) = match spawn_cfg(b"usertestcli", M_CHURN, 256) {
        Some(x) => x,
        None => {
            println!("[usertests] t23: spawn post-kill FAILED");
            return false;
        }
    };
    match wait_exit(chan2) {
        Some((0, _)) => true,
        _ => false,
    }
}

/// t24 — notifica unificata di morte a TUTTI i peer (Fase 14). Un server
/// sacrificale SRVDIE registra `Service::Test` e non risponde mai; un client
/// SYNCWAIT lo risolve per nome e resta bloccato in `send` sync. Il test fa
/// 2 `send_async` e poi killa il server. Verifica:
///   1. path async: `wait_reply` ritorna `Err(ServerDied)` con pid+code esatti;
///   2. path sync: il client (sbloccato da `wake_senders`) osserva a sua
///      volta l'EXIT_NOTIFY e riporta T_DONE(w0=1);
///   3. lo slot servizio e' liberato (lookup → Err) e il pool resta sano.
/// Determinismo senza sleep: handshake T_READY (kill solo a lookup avvenuto)
/// e T_GO (il T_DONE del client non puo' anticipare la notifica nella coda
/// del test). Ogni attesa ha garanzia di terminazione (reclaim a ogni tick).
/// NOTA: la EXIT_NOTIFY del server viene consumata da `wait_reply`, quindi
/// niente `wait_exit` per lui qui (il path parent e' gia' coperto da t23).
fn t_server_death_notify() -> bool {
    drain_stray();
    let (s_chan, s_pid) = match spawn_cfg(b"usertestcli", M_SRVDIE, 0) {
        Some(x) => x,
        None => {
            println!("[usertests] t24: spawn SRVDIE FAILED");
            return false;
        }
    };
    let (h_chan, _h_pid) = match spawn_cfg(b"usertestcli", M_SYNCWAIT, 0) {
        Some(x) => x,
        None => {
            println!("[usertests] t24: spawn SYNCWAIT FAILED");
            return false;
        }
    };
    // Handshake: il client ha risolto Test (server vivo).
    if !recv_expect(h_chan, T_READY) {
        println!("[usertests] t24: T_READY dal client mancante");
        return false;
    }
    // Due richieste async in volo (mai risposte: il server non fa recv).
    let r1 = match libr::send_async(s_chan, T_REQ, 0xBEEF, 0) {
        Ok(r) => r,
        Err(_) => {
            println!("[usertests] t24: send_async FAILED");
            return false;
        }
    };
    let _ = libr::send_async(s_chan, T_REQ, 0xBEEF + 1, 0);
    // Kill: wake_senders sblocca il client sync, il reclaim notifica tutti.
    let code = -9i64;
    if libr::kill(s_pid as i64, code).is_err() {
        println!("[usertests] t24: kill(pid={}) FAILED", s_pid);
        return false;
    }
    // Path async: la reply non arrivera' mai → ServerDied con pid+code.
    // (Notifiche di altri pid = stale di helper precedenti: consumate.)
    loop {
        match libr::wait_reply(r1) {
            Err(libr::WaitReplyError::ServerDied { pid, code: c })
                if pid == s_pid && c == code => break,
            Err(libr::WaitReplyError::ServerDied { pid, .. }) if pid != s_pid => continue,
            Err(libr::WaitReplyError::ServerDied { pid, code: c }) => {
                println!(
                    "[usertests] t24: ServerDied errato (pid={}, code={}, attesi pid={} code={})",
                    pid, c, s_pid, code
                );
                return false;
            }
            other => {
                println!("[usertests] t24: wait_reply inatteso: {:?}", other);
                return false;
            }
        }
    }
    // Via-libera al client: ora puo' inviare T_DONE senza race.
    if libr::send(h_chan, T_GO, 0, 0).is_err() {
        println!("[usertests] t24: T_GO al client FAILED");
        return false;
    }
    // Path sync: il client riporta T_DONE(w0=1) dopo aver visto EXIT_NOTIFY.
    if !recv_expect(h_chan, T_DONE) {
        println!("[usertests] t24: T_DONE(w0=1) dal client mancante");
        return false;
    }
    // Il client e' uscito pulito dopo il report.
    match wait_exit(h_chan) {
        Some((0, _)) => {}
        _ => {
            println!("[usertests] t24: exit del client anomala");
            return false;
        }
    }
    // Slot servizio liberato dal morto.
    if libr::service_lookup(libr::Service::Test).is_ok() {
        println!("[usertests] t24: slot Test ancora occupato dopo la morte");
        return false;
    }
    // Pool sano: spawn/exit post-mortem.
    let (chan2, _) = match spawn_cfg(b"usertestcli", M_CHURN, 64) {
        Some(x) => x,
        None => {
            println!("[usertests] t24: spawn post-kill FAILED");
            return false;
        }
    };
    match wait_exit(chan2) {
        Some((0, _)) => true,
        _ => false,
    }
}

/// t25 — purge dei mount alla morte di un driver (Fase 14). Un driver
/// sacrificale MNTDIE registra "/tdie"; il test apre /tdie/null (routing al
/// driver provato), killa il driver e ne registra un secondo sullo stesso
/// prefix. Senza purge lo stale (primo in lista per resolve_mount)
/// avvelenerebbe il routing anche dopo la re-registrazione → open fallisce.
/// Con purge: serve il nuovo driver. Deterministico, nessun timing.
fn t_driver_death_mount() -> bool {
    drain_stray();
    let (d1_chan, d1_pid) = match spawn_cfg(b"usertestcli", M_MNTDIE, 0) {
        Some(x) => x,
        None => {
            println!("[usertests] t25: spawn MNTDIE#1 FAILED");
            return false;
        }
    };
    if !recv_expect(d1_chan, T_READY) {
        println!("[usertests] t25: T_READY da MNTDIE#1 mancante");
        let _ = wait_exit(d1_chan);
        return false;
    }
    let fd1 = libr::open("/tdie/null", 0);
    if fd1 < 0 {
        println!("[usertests] t25: open /tdie/null via D1 FAILED");
        return false;
    }
    if libr::kill(d1_pid as i64, -11).is_err() {
        println!("[usertests] t25: kill D1 FAILED");
        return false;
    }
    match wait_exit(d1_chan) {
        Some((c, p)) if c == -11 && p == d1_pid as i64 => {}
        _ => {
            println!("[usertests] t25: exit notify D1 anomala");
            return false;
        }
    }
    // Re-registrazione stesso prefix: deve servire il NUOVO driver.
    let (d2_chan, d2_pid) = match spawn_cfg(b"usertestcli", M_MNTDIE, 0) {
        Some(x) => x,
        None => {
            println!("[usertests] t25: spawn MNTDIE#2 FAILED");
            return false;
        }
    };
    if !recv_expect(d2_chan, T_READY) {
        println!("[usertests] t25: T_READY da MNTDIE#2 mancante");
        let _ = wait_exit(d2_chan);
        return false;
    }
    let fd2 = libr::open("/tdie/null", 0);
    if fd2 < 0 {
        println!("[usertests] t25: open /tdie/null via D2 FAILED (mount stale?)");
        return false;
    }
    // Igiene: chiudi e uccidi D2 (nessun mount orfano per i test/shell dopo).
    let _ = libr::close(fd1);
    let _ = libr::close(fd2);
    if libr::kill(d2_pid as i64, 0).is_err() {
        println!("[usertests] t25: kill D2 FAILED");
        return false;
    }
    match wait_exit(d2_chan) {
        Some((0, _)) => {}
        _ => {
            println!("[usertests] t25: exit notify D2 anomala");
            return false;
        }
    }
    // Smoke ramfs: la purge non ha corrotto lo stato vivo.
    let fd = libr::open("hello.txt", 0);
    if fd < 0 {
        println!("[usertests] t25: smoke hello.txt FAILED");
        return false;
    }
    let mut buf = [0u8; 64];
    let n = libr::read_fs(fd, &mut buf, 64);
    let _ = libr::close(fd);
    n as usize >= HELLO.len() && buf[..HELLO.len()] == *HELLO
}

/// t26 — purge rings/ftable alla morte di client (Fase 14). N helper OPENDIE
/// aprono /dev/null + /dev/zero + hello.txt e muoiono SENZA close: userfs deve
/// purgare rings/ftable (con DEV_CLOSE inoltrato ai driver) senza corrompere
/// lo stato vivo. Poi smoke FS completo (null/zero/hello/write/mkdir/readdir).
fn t_client_death_purge() -> bool {
    drain_stray();
    const N: usize = 10;
    for i in 0..N {
        let (chan, pid) = match spawn_cfg(b"usertestcli", M_OPENDIE, 0) {
            Some(x) => x,
            None => {
                println!("[usertests] t26: spawn OPENDIE#{} FAILED", i);
                return false;
            }
        };
        match wait_exit(chan) {
            Some((0, p)) if p == pid as i64 => {}
            other => {
                println!("[usertests] t26: exit OPENDIE#{} anomala: {:?}", i, other);
                return false;
            }
        }
    }
    // Smoke completo: il server e' sano dopo N purge.
    let fd = libr::open("/dev/null", 0);
    if fd < 0 {
        println!("[usertests] t26: smoke open /dev/null FAILED");
        return false;
    }
    let data = [0x5Au8; 16];
    if libr::write_fs(fd, &data, 16) != 16 {
        println!("[usertests] t26: smoke write /dev/null FAILED");
        return false;
    }
    let mut b = [0u8; 16];
    if libr::read_fs(fd, &mut b, 16) != 0 {
        println!("[usertests] t26: smoke read /dev/null FAILED");
        return false;
    }
    let _ = libr::close(fd);
    let fdz = libr::open("/dev/zero", 0);
    if fdz < 0 {
        println!("[usertests] t26: smoke open /dev/zero FAILED");
        return false;
    }
    let mut z = [0xFFu8; 16];
    if libr::read_fs(fdz, &mut z, 16) != 16 || z.iter().any(|&x| x != 0) {
        println!("[usertests] t26: smoke read /dev/zero FAILED");
        return false;
    }
    let _ = libr::close(fdz);
    let fdh = libr::open("hello.txt", 0);
    if fdh < 0 {
        println!("[usertests] t26: smoke open hello.txt FAILED");
        return false;
    }
    let mut hb = [0u8; 64];
    let n = libr::read_fs(fdh, &mut hb, 64);
    let _ = libr::close(fdh);
    if n as usize >= HELLO.len() && hb[..HELLO.len()] != *HELLO {
        println!("[usertests] t26: smoke content hello.txt FAILED");
        return false;
    }
    if (n as usize) < HELLO.len() {
        println!("[usertests] t26: smoke short read hello.txt");
        return false;
    }
    let fw = libr::open("ut26.bin", libr::O_CREAT);
    if fw < 0 {
        println!("[usertests] t26: smoke open ut26.bin FAILED");
        return false;
    }
    let wb = [0xA5u8; 64];
    if libr::write_fs(fw, &wb, 64) != 64 {
        println!("[usertests] t26: smoke write ut26.bin FAILED");
        return false;
    }
    let _ = libr::close(fw);
    let fr = libr::open("ut26.bin", 0);
    if fr < 0 {
        println!("[usertests] t26: smoke reopen ut26.bin FAILED");
        return false;
    }
    let mut rb = [0u8; 64];
    let nr = libr::read_fs(fr, &mut rb, 64);
    let _ = libr::close(fr);
    if nr != 64 || rb.iter().any(|&x| x != 0xA5) {
        println!("[usertests] t26: smoke verify ut26.bin FAILED");
        return false;
    }
    if libr::mkdir("utdir26") < 0 {
        println!("[usertests] t26: smoke mkdir FAILED");
        return false;
    }
    if !dir_contains("/", "utdir26") {
        println!("[usertests] t26: smoke readdir FAILED");
        return false;
    }
    true
}

/// t27 — init-restart di devfs (Fase 14). Uccide devfs (pid via `service_pid`,
/// non suo figlio) e attende che init lo riavvii: prima sparizione dallo slot,
/// poi ricomparsa, poi /dev/null di nuovo operativo. Bound generosi (1000
/// tick ~ 10 s contro restart atteso ~50 tick): o PASS o FAIL rumoroso, mai
/// hang. userfs non viene mai toccato (il canale FS del test resta vivo).
/// NOTA: non confronta pid vecchio/nuovo (il riuso PID puo' ridare lo stesso
/// numero); osserva sparizione → ricomparsa.
fn t_devfs_restart() -> bool {
    drain_stray();
    let fd = libr::open("/dev/null", 0);
    if fd < 0 {
        println!("[usertests] t27: baseline open /dev/null FAILED");
        return false;
    }
    let _ = libr::close(fd);
    let p1 = match libr::service_pid(libr::Service::Devfs) {
        Ok(p) => p,
        Err(_) => {
            println!("[usertests] t27: service_pid(Devfs) FAILED");
            return false;
        }
    };
    if libr::kill(p1, -13).is_err() {
        println!("[usertests] t27: kill devfs pid={} FAILED", p1);
        return false;
    }
    // Fase A: attendi sparizione dallo slot (morte osservata dal registry).
    // Poll throttled (Livello 1, buon vicinato): vedi `libr::poll_wait`.
    if !libr::poll_wait(1000, libr::POLL_PERIOD_TICKS, || {
        libr::service_pid(libr::Service::Devfs).is_err()
    }) {
        println!("[usertests] t27: devfs mai sparito (timeout)");
        return false;
    }
    // Fase B: attendi ricomparsa (init ha riavviato + registrato).
    let p2 = match libr::poll_value(1000, libr::POLL_PERIOD_TICKS, || {
        libr::service_pid(libr::Service::Devfs).ok()
    }) {
        Some(p) => p,
        None => {
            println!("[usertests] t27: devfs mai riapparso (timeout)");
            return false;
        }
    };
    println!("[usertests] t27: devfs riavviato (pid {} -> {})", p1, p2);
    // Fase C: operativita' — open finche' riesce (bound come sopra).
    // Throttled via `libr::open_wait` (igiene Livello 1, buon vicinato).
    // NOTA (esperimento B): t27 PASSA anche in busy-loop non throttled —
    // lo storm del test NON e' causale del vecchio FAIL (N=1, confound).
    let fd2 = libr::open_wait("/dev/null", 0, 1000, libr::POLL_PERIOD_TICKS);
    if fd2 < 0 {
        println!("[usertests] t27: /dev/null mai tornato (timeout)");
        return false;
    }
    let data = [0x5Au8; 16];
    let ok = libr::write_fs(fd2, &data, 16) == 16;
    let mut b = [0u8; 16];
    let okr = libr::read_fs(fd2, &mut b, 16) == 0;
    let _ = libr::close(fd2);
    if !ok || !okr {
        println!("[usertests] t27: write/read post-restart FAILED");
        return false;
    }
    // Smoke ramfs: userfs mai toccato dal restart.
    let fdh = libr::open("hello.txt", 0);
    if fdh < 0 {
        println!("[usertests] t27: smoke hello.txt FAILED");
        return false;
    }
    let mut hb = [0u8; 64];
    let n = libr::read_fs(fdh, &mut hb, 64);
    let _ = libr::close(fdh);
    n as usize >= HELLO.len() && hb[..HELLO.len()] == *HELLO
}

/// t28 — restart di userfs end-to-end (Fase 14). Uccide userfs (pid via
/// `service_pid`) e attende che init lo riavvii. Poi verifica: fixture fresh
/// funzionanti (mkdir/write/read), hello.txt ricreato, probe ramfs sparito
/// (wipe: la ramfs e' volatile, contratto codificato qui), /fat leggibile
/// (persistente su disco: contrasto), /dev/null operativo (driver
/// re-registrati via ensure_mounted). Bound generosi, mai hang.
fn t_userfs_restart() -> bool {
    drain_stray();
    // Baseline: hello + /dev/null.
    let fdh = libr::open("hello.txt", 0);
    if fdh < 0 {
        println!("[usertests] t28: baseline hello.txt FAILED");
        return false;
    }
    let _ = libr::close(fdh);
    let fdn = libr::open("/dev/null", 0);
    if fdn < 0 {
        println!("[usertests] t28: baseline /dev/null FAILED");
        return false;
    }
    let _ = libr::close(fdn);
    // Probe ramfs (wipe check dopo il restart).
    let fp = libr::open("td28probe", 0x200);
    if fp < 0 {
        println!("[usertests] t28: create probe FAILED");
        return false;
    }
    let pwb = [0xBEu8; 32];
    if libr::write_fs(fp, &pwb, 32) != 32 {
        println!("[usertests] t28: write probe FAILED");
        return false;
    }
    let _ = libr::close(fp);
    // Kill + sparizione + ricomparsa (come t27).
    let p1 = match libr::service_pid(libr::Service::Fs) {
        Ok(p) => p,
        Err(_) => {
            println!("[usertests] t28: service_pid(Fs) FAILED");
            return false;
        }
    };
    if libr::kill(p1, -14).is_err() {
        println!("[usertests] t28: kill userfs pid={} FAILED", p1);
        return false;
    }
    // Kill + sparizione + ricomparsa (come t27, poll throttled Livello 1).
    if !libr::poll_wait(1000, libr::POLL_PERIOD_TICKS, || {
        libr::service_pid(libr::Service::Fs).is_err()
    }) {
        println!("[usertests] t28: userfs mai sparito (timeout)");
        return false;
    }
    let p2 = match libr::poll_value(1000, libr::POLL_PERIOD_TICKS, || {
        libr::service_pid(libr::Service::Fs).ok()
    }) {
        Some(p) => p,
        None => {
            println!("[usertests] t28: userfs mai riapparso (timeout)");
            return false;
        }
    };
    println!("[usertests] t28: userfs riavviato (pid {} -> {})", p1, p2);
    // Fixture fresh (re-handshake trasparente via NOHANDSHAKE se serve).
    // Throttled (lezione t27/t28): martellare userfs in busy-loop affama la
    // re-registrazione dei driver (devfs/console ricreano il mount proprio
    // su questo userfs).
    if !libr::poll_wait(1000, libr::POLL_PERIOD_TICKS, || {
        libr::mkdir("/td28") == 0
    }) {
        println!("[usertests] t28: mkdir post-restart mai riuscito (timeout)");
        return false;
    }
    let fw = libr::open("/td28/f", 0x200);
    if fw < 0 {
        println!("[usertests] t28: create /td28/f FAILED");
        return false;
    }
    let wb = [0xD8u8; 32];
    if libr::write_fs(fw, &wb, 32) != 32 {
        println!("[usertests] t28: write /td28/f FAILED");
        return false;
    }
    let _ = libr::close(fw);
    let fr = libr::open("/td28/f", 0);
    if fr < 0 {
        println!("[usertests] t28: reopen /td28/f FAILED");
        return false;
    }
    let mut rb = [0u8; 32];
    let nr = libr::read_fs(fr, &mut rb, 32);
    let _ = libr::close(fr);
    // Dettaglio diagnostico (solo su FAIL): nr e primo byte diverso.
    if nr != 32 {
        println!("[usertests] t28: verify nr={} (atteso 32)", nr);
        return false;
    }
    if let Some((i, &x)) = rb.iter().enumerate().find(|&(_, &x)| x != 0xD8) {
        println!("[usertests] t28: verify mismatch i={} val={:#x}", i, x);
        return false;
    }
    // hello.txt ricreato dal fresh userfs.
    let fh = libr::open("hello.txt", 0);
    if fh < 0 {
        println!("[usertests] t28: hello.txt ricreato mancante");
        return false;
    }
    let mut hb = [0u8; 64];
    let n = libr::read_fs(fh, &mut hb, 64);
    let _ = libr::close(fh);
    if (n as usize) < HELLO.len() || hb[..HELLO.len()] != *HELLO {
        println!("[usertests] t28: hello.txt ricreato corrotto");
        return false;
    }
    // Wipe: il probe pre-restart non esiste piu' (ramfs volatile). NOTA: via
    // readdir, MAI via open (su ramfs open crea il file se manca!).
    if dir_contains("/", "td28probe") {
        println!("[usertests] t28: probe sopravvissuto al restart?!");
        return false;
    }
    // Persistente: /fat leggibile (rimontato dal disco).
    let ff = libr::open("/fat/HELLO.TXT", 0);
    if ff < 0 {
        println!("[usertests] t28: /fat/HELLO.TXT illeggibile");
        return false;
    }
    let mut fb = [0u8; 32];
    let nf = libr::read_fs(ff, &mut fb, 32);
    let _ = libr::close(ff);
    if nf <= 0 {
        println!("[usertests] t28: /fat/HELLO.TXT vuoto");
        return false;
    }
    // Driver re-registrati: /dev/null operativo. Retry con bound (throttled):
    // devfs ricrea il mount in modo asincrono su EXIT_NOTIFY e puo' laggare
    // dietro il fresh userfs; un singolo tentativo darebbe falsi FAIL.
    let fd2 = libr::open_wait("/dev/null", 0, 1000, libr::POLL_PERIOD_TICKS);
    if fd2 < 0 {
        println!("[usertests] t28: /dev/null post-restart FAILED");
        return false;
    }
    let data = [0x5Au8; 16];
    let ok = libr::write_fs(fd2, &data, 16) == 16;
    let mut b = [0u8; 16];
    let okr = libr::read_fs(fd2, &mut b, 16) == 0;
    let _ = libr::close(fd2);
    if !ok || !okr {
        println!("[usertests] t28: write/read /dev/null FAILED");
        return false;
    }
    true
}

/// t29 — map-flap isolation (Fase 14, diagnosi t28): martella `map_physical`
/// di P1 su VA_X verificando marker, prima da solo (3000 iter) poi con un
/// helper che fa lo stesso su P2 (stessa VA, altre tabelle, altri frame).
/// Qualunque mismatch = cross-talk di mapping/TLB/aliasing. Non tocca il FS:
/// robusto a qualunque stato userfs. VA_X in zona libera, P1/P2 nello scratch
/// kernel riservato (MAP_TEST_PHYS).
fn t_mapflap() -> bool {
    drain_stray();
    const VA_X: u64 = 0x0000_4000_003E_0000;
    const P1: u64 = libr::MAP_TEST_PHYS;
    const N: usize = 3000;
    // Fase A: da solo.
    for _ in 0..N {
        if libr::map_physical(P1, VA_X, 1).is_err() {
            println!("[usertests] t29: map solo FAILED");
            return false;
        }
        unsafe {
            let p = VA_X as *mut u8;
            for i in 0..64 {
                core::ptr::write_volatile(p.add(i), 0xAA);
            }
            for i in 0..64 {
                if core::ptr::read_volatile(p.add(i)) != 0xAA {
                    println!("[usertests] t29: mismatch solo i={}", i);
                    return false;
                }
            }
        }
    }
    // Fase B: con helper concorrente (stessa VA, pagine diverse).
    let (h_chan, _) = match spawn_cfg(b"usertestcli", M_MAPHAMMER, N as u64) {
        Some(x) => x,
        None => {
            println!("[usertests] t29: spawn MAPHAMMER FAILED");
            return false;
        }
    };
    let mut bad = 0u32;
    for _ in 0..N {
        if libr::map_physical(P1, VA_X, 1).is_err() {
            println!("[usertests] t29: map conc FAILED");
            return false;
        }
        unsafe {
            let p = VA_X as *mut u8;
            for i in 0..64 {
                core::ptr::write_volatile(p.add(i), 0xAA);
            }
            for i in 0..64 {
                if core::ptr::read_volatile(p.add(i)) != 0xAA {
                    bad += 1;
                    break;
                }
            }
        }
        if bad > 0 {
            break;
        }
    }
    // Join helper (loop bounded da entrambe le parti, poi T_DONE).
    let h_ok = recv_expect(h_chan, T_DONE);
    match wait_exit(h_chan) {
        Some((0, _)) => {}
        _ => {
            println!("[usertests] t29: exit helper anomala");
            return false;
        }
    }
    if bad > 0 || !h_ok {
        println!(
            "[usertests] t29: mismatch concorrente (locali={}, helper_ok={})",
            bad, h_ok
        );
        return false;
    }
    true
}

/// t30 — fairness dello scheduler sotto carico IPC (gate anti-regressione).
/// Un helper FLOOD martella open+write+close di /dev/null a flood stabilizzato
/// (warm-up 3000 op) mentre devfs viene killato e riavviato da init (come
/// t27). Verdetto: /dev/null deve tornare operativo entro 300 tick.
/// Il flood crea transizioni runnable/blocked continue tra N processi: se la
/// rotazione dello scheduler si rompe (es. il bug di parita' del round-robin,
/// che affamava meta' dei pronti PER SEMPRE), la latenza di mount esplode da
/// ~1 a infinito tick e t30 lo becca. NON e' un test di saturazione di userfs:
/// con client FS sincroni (<=1 richiesta in volo ciascuno) la coda da 8 slot
/// non si riempie mai e il serving resta in ~1 tick anche sotto tempesta
/// (misurato); la saturazione vera richiederebbe client async N-in-volo
/// (futura fase FS-async: formato frame con lunghezza, fair queuing,
/// backpressure). Il flooder viene sempre fermato (T_STOP) e reaped.
fn t_neighbor() -> bool {
    drain_stray();
    let fb = libr::open_wait("/dev/null", 0, 1000, libr::POLL_PERIOD_TICKS);
    if fb < 0 {
        println!("[usertests] t30: baseline open /dev/null FAILED");
        return false;
    }
    let _ = libr::close(fb);
    // Helper "cattivo vicino" (nessun T_DONE atteso prima di T_STOP).
    let (fchan, _) = match spawn_cfg(b"usertestcli", M_FLOOD, 0) {
        Some(x) => x,
        None => {
            println!("[usertests] t30: spawn flooder FAILED");
            return false;
        }
    };
    // Attendi flood stabilizzato (T_READY dopo WARMUP_OPS): il kill deve
    // avvenire sotto carico reale, non durante lo startup dell'helper.
    // Bounded VERO (1000 tick): recv_poll non bloccante + bound sul clock —
    // un recv() bloccante qui resterebbe appeso per sempre se l'helper muore
    // prima del READY (nessun messaggio sveglia piu' nessuno).
    // Attendi T_READY BLOCCANTE (il parent dorme mentre il flooder scalda).
    // Lezione t30/Fase 15: un'attesa in recv_poll tiene il parent sempre
    // Ready e diluisce la rotazione (~1 quantum per hop IPC → flooder 25x
    // piu' lento, warmup mai raggiunto). Bloccando, il flooder gira libero.
    // EXIT_NOTIFY dal flooder = morto -> false (mai hang; il flooder in
    // warmup non puo' morire — loop infinito — ma non si resta appesi).
    let warmed = loop {
        match libr::recv() {
            Ok(m) => {
                if m.tag == T_READY && m.channel == fchan {
                    let _ = libr::reply(T_ACK, 0, 0);
                    break m.w0 == 1;
                }
                if libr::is_exit_notify(&m) && m.channel == fchan {
                    break false;
                }
                if !libr::is_exit_notify(&m) {
                    let _ = libr::reply(T_ACK, 0, 0);
                }
            }
            Err(_) => break false,
        }
    };
    if !warmed {
        println!("[usertests] t30: flooder mai pronto (timeout)");
        stop_flooder(fchan);
        return false;
    }
    let p1 = match libr::service_pid(libr::Service::Devfs) {
        Ok(p) => p,
        Err(_) => {
            println!("[usertests] t30: service_pid(Devfs) FAILED");
            stop_flooder(fchan);
            return false;
        }
    };
    if libr::kill(p1, -15).is_err() {
        println!("[usertests] t30: kill devfs pid={} FAILED", p1);
        stop_flooder(fchan);
        return false;
    }
    // Sparizione + ricomparsa (poll throttled, come t27).
    if !libr::poll_wait(1000, libr::POLL_PERIOD_TICKS, || {
        libr::service_pid(libr::Service::Devfs).is_err()
    }) {
        println!("[usertests] t30: devfs mai sparito (timeout)");
        stop_flooder(fchan);
        return false;
    }
    if libr::poll_value(1000, libr::POLL_PERIOD_TICKS, || {
        libr::service_pid(libr::Service::Devfs).ok()
    })
    .is_none()
    {
        println!("[usertests] t30: devfs mai riapparso (timeout)");
        stop_flooder(fchan);
        return false;
    }
    // Misura: operativita' sotto flood.
    let t_start = libr::get_ticks();
    let fd = libr::open_wait("/dev/null", 0, 1000, libr::POLL_PERIOD_TICKS);
    let elapsed = libr::get_ticks() - t_start;
    stop_flooder(fchan);
    if fd < 0 {
        println!("[usertests] t30: /dev/null mai tornato (timeout)");
        return false;
    }
    let data = [0x5Au8; 16];
    let ok = libr::write_fs(fd, &data, 16) == 16;
    let mut b = [0u8; 16];
    let okr = libr::read_fs(fd, &mut b, 16) == 0;
    let _ = libr::close(fd);
    if !ok || !okr {
        println!("[usertests] t30: write/read post-flood FAILED");
        return false;
    }
    // Smoke ramfs: il flood e' read-only, hello.txt intatto.
    let fdh = libr::open("hello.txt", 0);
    if fdh < 0 {
        println!("[usertests] t30: smoke hello.txt FAILED");
        return false;
    }
    let mut hb = [0u8; 64];
    let n = libr::read_fs(fdh, &mut hb, 64);
    let _ = libr::close(fdh);
    if n as usize >= HELLO.len() && hb[..HELLO.len()] != *HELLO {
        println!("[usertests] t30: hello.txt corrotto dal flood?!");
        return false;
    }
    // Verdetto di vicinato: bound stretto sotto flood.
    const NEIGHBOR_TICKS: i64 = 300;
    println!("[usertests] t30: /dev operativo dopo {} tick sotto flood", elapsed);
    if elapsed > NEIGHBOR_TICKS {
        println!(
            "[usertests] t30: FAIL vicinato ({} > {} tick: un client satura userfs)",
            elapsed, NEIGHBOR_TICKS
        );
        return false;
    }
    n as usize >= HELLO.len() && hb[..HELLO.len()] == *HELLO
}

/// Ferma il flooder di t30 (T_STOP sync: l'helper risponde entro ~64 op) e lo
/// reaped (T_DONE + EXIT_NOTIFY). Se l'helper e' gia' morto, send fallisce e
/// resta solo il reap. Mai hang: l'helper o risponde o e' morto.
fn stop_flooder(fchan: u64) {
    if libr::send(fchan, T_STOP, 0, 0).is_ok() {
        let _ = recv_done(&[fchan]);
    }
    let _ = wait_exit(fchan);
}

/// t31 — presenza keyboard stack in userspace (Fase 15, gate leggero).
/// Verifica che i servizi Kbd/Tty siano registrati e i device apribili:
/// /dev/kbd (scancode raw da userkbd) e /dev/input/keyboard (byte cotti da
/// usertty, stesso path di prima). Niente digitazione reale (serve QMP).
fn t_kbd_presence() -> bool {
    drain_stray();
    if libr::service_pid(libr::Service::Kbd).is_err() {
        println!("[usertests] t31: service Kbd non registrato");
        return false;
    }
    if libr::service_pid(libr::Service::Tty).is_err() {
        println!("[usertests] t31: service Tty non registrato");
        return false;
    }
    let fk = libr::open_wait("/dev/kbd/kbd", 0, 1000, libr::POLL_PERIOD_TICKS);
    if fk < 0 {
        println!("[usertests] t31: open /dev/kbd/kbd FAILED");
        return false;
    }
    let _ = libr::close(fk);
    let ft = libr::open_wait("/dev/input/keyboard", 0, 1000, libr::POLL_PERIOD_TICKS);
    if ft < 0 {
        println!("[usertests] t31: open /dev/input/keyboard FAILED");
        return false;
    }
    let _ = libr::close(ft);
    true
}

/// Contenuto atteso di /fat/HELLO.TXT (come testfat Test 2).
const FAT_HELLO: &[u8] = b"Hello from Velordor FAT32!\n";

/// Apre /dev/sda raw (throttled, Livello 1), legge il settore 0 e verifica la
/// firma boot 0x55AA a offset 510 (stesso settore del mount /fat: prova il
/// data-plane DISK di userdisk e il relay DEV di userfs in un colpo solo).
fn disk_sector0_ok() -> bool {
    let fd = libr::open_wait("/dev/sda", 0, 1000, libr::POLL_PERIOD_TICKS);
    if fd < 0 {
        return false;
    }
    let mut buf = [0u8; 512];
    let n = libr::read_fs(fd, &mut buf, 512);
    let _ = libr::close(fd);
    n == 512 && buf[510] == 0x55 && buf[511] == 0xAA
}

/// Legge tutto il file `fd` in `dst` (come testfat `read_all`).
fn t33_read_all(fd: i64, dst: &mut [u8]) -> usize {
    let mut got = 0usize;
    while got < dst.len() {
        let rest = dst.len() - got;
        let n = libr::read_fs(fd, &mut dst[got..], rest);
        if n <= 0 {
            break;
        }
        got += n as usize;
    }
    got
}

/// t33 — mount/umount espliciti (Fase 16b).
/// mkdir /mnt (ramfs) → mount /dev/sda /mnt → /mnt/HELLO.TXT col contenuto
/// FAT → umount busy rifiutato con fd aperto → close → umount ok → /mnt
/// torna ramfs (readdir senza entry FAT). Error paths: sorgente inesistente
/// o senza disco, target invalido, doppio mount (idempotente: ok e ancora
/// operativo), umount di non-montato e di `/`. Ultimo test: dopo solo shell.
fn t_mount() -> bool {
    drain_stray();
    if libr::mkdir("/mnt") < 0 {
        println!("[usertests] t33: mkdir /mnt FAILED");
        return false;
    }
    if libr::mount("/dev/sda", "/mnt") < 0 {
        println!("[usertests] t33: mount /dev/sda /mnt FAILED");
        return false;
    }
    // Re-mount identico: idempotente (replace), resta operativo.
    if libr::mount("/dev/sda", "/mnt") < 0 {
        println!("[usertests] t33: re-mount FAILED");
        return false;
    }
    // Contenuto via mount dinamico (stesso della statica /fat).
    let fd = libr::open("/mnt/HELLO.TXT", 0);
    if fd < 0 {
        println!("[usertests] t33: open /mnt/HELLO.TXT FAILED");
        return false;
    }
    let mut hb = [0u8; 32];
    let n = t33_read_all(fd, &mut hb);
    if n != FAT_HELLO.len() || hb[..FAT_HELLO.len()] != *FAT_HELLO {
        println!("[usertests] t33: /mnt/HELLO.TXT corrotto");
        let _ = libr::close(fd);
        return false;
    }
    // Umount busy: fd aperto sul mount → rifiutato.
    if libr::umount("/mnt") == 0 {
        println!("[usertests] t33: umount busy accettato?!");
        let _ = libr::close(fd);
        return false;
    }
    let _ = libr::close(fd);
    if libr::umount("/mnt") < 0 {
        println!("[usertests] t33: umount /mnt FAILED");
        return false;
    }
    // Dopo umount /mnt e' di nuovo ramfs: niente entry FAT.
    let mut eb = [0u8; 256];
    let c = libr::readdir("/mnt", &mut eb, 256);
    if c < 0 {
        println!("[usertests] t33: readdir /mnt post-umount FAILED");
        return false;
    }
    let mut i = 0usize;
    let mut fat_left = false;
    while i < eb.len() && eb[i] != 0 {
        let start = i;
        while i < eb.len() && eb[i] != 0 {
            i += 1;
        }
        if &eb[start..i] == b"HELLO.TXT" || &eb[start..i] == b"SUB" {
            fat_left = true;
        }
        i += 1;
    }
    if fat_left {
        println!("[usertests] t33: entry FAT dopo umount?!");
        return false;
    }
    // Error paths.
    if libr::mount("/dev/xxx", "/mnt") == 0 {
        println!("[usertests] t33: mount sorgente invalida accettato?!");
        return false;
    }
    if libr::mount("/dev/sdz", "/mnt") == 0 {
        println!("[usertests] t33: mount disco assente accettato?!");
        return false;
    }
    if libr::mount("/dev/sda", "/a/../b") == 0 {
        println!("[usertests] t33: mount target invalido accettato?!");
        return false;
    }
    if libr::umount("/mnt") == 0 {
        println!("[usertests] t33: doppio umount accettato?!");
        return false;
    }
    if libr::umount("/") == 0 {
        println!("[usertests] t33: umount / accettato?!");
        return false;
    }
    true
}

/// t35 — resolve nome→handle lato driver (Fase 16c).
/// Nomi ignoti (ben formati ma assenti, o malformati) rifiutati SENZA cambio
/// di stato (nessuna spec fantasma: l'umount successivo deve fallire);
/// replace-con-bad-source su target attivo non distrugge il buon mount;
/// mount valido ancora operativo dopo i rifiuti (tabella intatta).
/// (t34 resta libero per la Fase 17.)
fn t_resolve() -> bool {
    drain_stray();
    // 1. Nome ben formato ma assente (fat.img non partizionata: niente sda1).
    if libr::mount("/dev/sda1", "/phantom") == 0 {
        println!("[usertests] t35: mount sda1 assente accettato?!");
        return false;
    }
    // Nessuna spec fantasma: umount deve fallire.
    if libr::umount("/phantom") == 0 {
        println!("[usertests] t35: spec fantasma dopo mount fallito?!");
        return false;
    }
    // 2. Nome malformato/ignoto: stesso contratto.
    if libr::mount("/dev/zzz", "/phantom2") == 0 {
        println!("[usertests] t35: mount nome ignoto accettato?!");
        return false;
    }
    if libr::umount("/phantom2") == 0 {
        println!("[usertests] t35: spec fantasma (nome ignoto)?!");
        return false;
    }
    // 3. Mount valido ancora operativo dopo i rifiuti (tabella intatta).
    if libr::mount("/dev/sda", "/mnt") < 0 {
        println!("[usertests] t35: mount /dev/sda /mnt FAILED");
        return false;
    }
    let fd = libr::open("/mnt/HELLO.TXT", 0);
    if fd < 0 {
        println!("[usertests] t35: open /mnt/HELLO.TXT FAILED");
        let _ = libr::umount("/mnt");
        return false;
    }
    let mut hb = [0u8; 32];
    let n = t33_read_all(fd, &mut hb);
    let _ = libr::close(fd);
    if n != FAT_HELLO.len() || hb[..FAT_HELLO.len()] != *FAT_HELLO {
        println!("[usertests] t35: /mnt/HELLO.TXT corrotto");
        let _ = libr::umount("/mnt");
        return false;
    }
    // 4. Replace con bad source non distrugge il buon mount.
    if libr::mount("/dev/zzz", "/mnt") == 0 {
        println!("[usertests] t35: replace con bad source accettato?!");
        let _ = libr::umount("/mnt");
        return false;
    }
    let fd = libr::open("/mnt/HELLO.TXT", 0);
    if fd < 0 {
        println!("[usertests] t35: buon mount distrutto dal bad replace?!");
        let _ = libr::umount("/mnt");
        return false;
    }
    let _ = libr::close(fd);
    if libr::umount("/mnt") < 0 {
        println!("[usertests] t35: umount /mnt FAILED");
        return false;
    }
    true
}

/// t34 — diritti per-canale lato server (Fase 17, self-restriction).
/// Diretto sul canale di usertests (nessun helper: la semantica e' proprio
/// "riduco i MIEI diritti"). ESEGUITO PER ULTIMO: i drop sono irrevocabili.
/// (1) GET default = ALL+root, baseline write+read ok. (2) drop WRITE:
/// write -1, read ok (riapertura: OPEN resta). (3) drop MOUNT + subtree /fat:
/// mount -1, open fuori -1, open dentro + read + readdir dentro ok, readdir
/// fuori -1 (ogni rifiuto e' seguito da un'op valida: nessun disallineamento
/// ring). (4) widen a root rifiutato, GET conferma i diritti invariati.
fn t_rights() -> bool {
    drain_stray();
    // 1. Default {ALL, root}: GET ritorna ALL, subtree vuoto (= root).
    let mut sb = [0u8; 32];
    if libr::rights_get(&mut sb) != libr::RIGHTS_ALL as i64 || sb[0] != 0 {
        println!("[usertests] t34: GET default non ALL+root");
        return false;
    }
    let fd = libr::open("/t34.txt", libr::O_CREAT);
    if fd < 0 {
        println!("[usertests] t34: open baseline FAILED");
        return false;
    }
    if libr::write_fs(fd, b"abcdef", 6) != 6 {
        println!("[usertests] t34: write baseline FAILED");
        let _ = libr::close(fd);
        return false;
    }
    let _ = libr::close(fd);
    // 2. Drop solo-ops (WRITE via, resto invariato): write -1, read ok.
    if libr::rights_drop(libr::RIGHTS_ALL & !libr::RIGHTS_WRITE, None) != 0 {
        println!("[usertests] t34: rights_drop WRITE FAILED");
        return false;
    }
    let fd = libr::open("/t34.txt", 0);
    if fd < 0 {
        println!("[usertests] t34: reopen dopo drop FAILED");
        return false;
    }
    if libr::write_fs(fd, b"x", 1) >= 0 {
        println!("[usertests] t34: write accettata dopo drop?!");
        let _ = libr::close(fd);
        return false;
    }
    let mut rb = [0u8; 8];
    if libr::read_fs(fd, &mut rb, 6) != 6 || rb[..6] != *b"abcdef" {
        println!("[usertests] t34: read dopo drop FAILED/corrotto");
        let _ = libr::close(fd);
        return false;
    }
    let _ = libr::close(fd);
    // 3. Drop MOUNT + subtree /fat.
    if libr::rights_drop(
        libr::RIGHTS_ALL & !libr::RIGHTS_WRITE & !libr::RIGHTS_MOUNT,
        Some("/fat"),
    ) != 0
    {
        println!("[usertests] t34: rights_drop MOUNT+/fat FAILED");
        return false;
    }
    if libr::mount("/dev/sda", "/mnt") == 0 {
        println!("[usertests] t34: mount accettato senza bit?!");
        return false;
    }
    if libr::open("/hello.txt", 0) >= 0 {
        println!("[usertests] t34: open fuori subtree accettato?!");
        return false;
    }
    let fd = libr::open("/fat/HELLO.TXT", 0);
    if fd < 0 {
        println!("[usertests] t34: open dentro subtree FAILED");
        return false;
    }
    let mut hb = [0u8; 32];
    let n = t33_read_all(fd, &mut hb);
    let _ = libr::close(fd);
    if n != FAT_HELLO.len() || hb[..FAT_HELLO.len()] != *FAT_HELLO {
        println!("[usertests] t34: /fat/HELLO.TXT corrotto");
        return false;
    }
    let mut eb = [0u8; 256];
    if libr::readdir("/fat", &mut eb, 256) < 0 {
        println!("[usertests] t34: readdir dentro subtree FAILED");
        return false;
    }
    if libr::readdir("/", &mut eb, 256) != -1 {
        println!("[usertests] t34: readdir fuori subtree accettato?!");
        return false;
    }
    // 4. Widen a root rifiutato (da /fat): -1 e diritti invariati.
    if libr::rights_drop(libr::RIGHTS_ALL, Some("/")) != -1 {
        println!("[usertests] t34: widen a root accettato?!");
        return false;
    }
    let mut sb2 = [0u8; 32];
    let want = (libr::RIGHTS_ALL & !libr::RIGHTS_WRITE & !libr::RIGHTS_MOUNT) as i64;
    if libr::rights_get(&mut sb2) != want || sb2[..3] != *b"fat" || sb2[3] != 0 {
        println!("[usertests] t34: GET finale non mask+/fat");
        return false;
    }
    true
}

/// t37 — syscall `ps_info` (Fase 19.1): snapshot processi.
fn t_ps() -> bool {
    let me = libr::getpid() as u32;
    let mut count = 0u32;
    let mut found_me = false;
    let mut my_ticks = 0u64;
    for pid in 0..libr::PS_SCAN_MAX {
        let Some(e) = libr::ps_info(pid) else { continue; };
        count += 1;
        if e.prio > 31 {
            println!("[usertests] t37: prio assurda pid={}", pid);
            return false;
        }
        if pid == 0 {
            // idle: processo kernel senza padre.
            if e.name_str() != "idle" || e.parent.is_some() {
                println!("[usertests] t37: idle anomalo");
                return false;
            }
        }
        if pid == 1 {
            // init: gira da boot, ha consumato tick di sicuro.
            if e.name_str() != "userinit" || e.parent.is_some() || e.ticks == 0 {
                println!("[usertests] t37: init anomalo");
                return false;
            }
        }
        if e.pid == me {
            found_me = true;
            my_ticks = e.ticks;
            // Sto eseguendo: Ready (mai Blocked durante una syscall).
            if e.state != 0 {
                println!("[usertests] t37: self non Ready");
                return false;
            }
        }
    }
    if !found_me {
        println!("[usertests] t37: self assente");
        return false;
    }
    if count < 8 {
        println!("[usertests] t37: solo {} processi?!", count);
        return false;
    }
    // TIME cresce mentre giro: attendo (bound 500 tick) che il contatore del
    // processo avanzi — robusto a qualunque velocita' CPU. Lo spin fisso da
    // 20M iterazioni finiva sotto un tick sulle CPU veloci (flake "TIME fermo"
    // osservato sotto KVM): ora si aspetta l'evento con bound, mai un tempo
    // fisso. Batch di spin puri tra le letture (pattern utspin: niente
    // busy-loop su syscall).
    let bound = libr::get_ticks() + 500;
    let mut my_ticks2 = my_ticks;
    loop {
        let mut x = 0u64;
        for i in 0..1_000_000u64 {
            x = x.wrapping_add(i ^ 0x9E3779B97F4A7C15);
        }
        core::hint::black_box(x);
        for pid in 0..libr::PS_SCAN_MAX {
            if let Some(e) = libr::ps_info(pid) {
                if e.pid == me {
                    my_ticks2 = e.ticks;
                }
            }
        }
        if my_ticks2 > my_ticks {
            break;
        }
        if libr::get_ticks() > bound {
            println!("[usertests] t37: TIME fermo ({} -> {})", my_ticks, my_ticks2);
            return false;
        }
    }
    true
}

/// t38 — `stat` lato userfs (Fase 19.2): metadati senza aprire.
fn t_stat() -> bool {
    let mut st = libr::Stat { size: 0, kind: 0, readonly: false };
    // File ramfs: size esatta, non readonly.
    if libr::stat("hello.txt", &mut st) != 0
        || !st.is_file()
        || st.size as usize != HELLO.len()
        || st.readonly
    {
        println!("[usertests] t38: stat hello.txt FAILED");
        return false;
    }
    // Root ramfs: dir.
    if libr::stat("/", &mut st) != 0 || !st.is_dir() {
        println!("[usertests] t38: stat / FAILED");
        return false;
    }
    // Dir ramfs creata ad hoc + rimozione (stat segue la vita del nodo).
    if libr::mkdir("/t38dir") != 0 {
        println!("[usertests] t38: mkdir /t38dir FAILED");
        return false;
    }
    if libr::stat("/t38dir", &mut st) != 0 || !st.is_dir() || st.readonly {
        println!("[usertests] t38: stat /t38dir FAILED");
        return false;
    }
    if libr::remove("/t38dir") != 0 || libr::stat("/t38dir", &mut st) == 0 {
        println!("[usertests] t38: stat dopo rm accettata?!");
        return false;
    }
    // FAT (scrivibile dalla Fase 20): file + dir NON readonly.
    if libr::stat("/fat/HELLO.TXT", &mut st) != 0
        || !st.is_file()
        || st.size == 0
        || st.readonly
    {
        println!("[usertests] t38: stat /fat/HELLO.TXT FAILED");
        return false;
    }
    if libr::stat("/fat", &mut st) != 0 || !st.is_dir() || st.readonly {
        println!("[usertests] t38: stat /fat FAILED");
        return false;
    }
    // Device: tipo device, size 0; padri sintetizzati: dir.
    if libr::stat("/dev/null", &mut st) != 0 || !st.is_device() || st.size != 0 {
        println!("[usertests] t38: stat /dev/null FAILED");
        return false;
    }
    if libr::stat("/dev", &mut st) != 0 || !st.is_dir() {
        println!("[usertests] t38: stat /dev FAILED");
        return false;
    }
    // Error paths: inesistente e sotto-device (foglie).
    if libr::stat("/nonexistent-t38", &mut st) == 0 {
        println!("[usertests] t38: stat inesistente accettata?!");
        return false;
    }
    if libr::stat("/dev/null/trailing", &mut st) == 0 {
        println!("[usertests] t38: stat sotto-device accettata?!");
        return false;
    }
    true
}

/// Fixture disco secondario (Fase 16d, accoppiate a run.sh: fat2.img
/// generata con `--serial C0FFEE01 --label SECOND --marker ...`).
const DISK2_UUID: &str = "C0FFEE01";
const DISK2_LABEL: &str = "SECOND";
const DISK2_MARKER: &[u8] = b"second disk marker";

/// true se il buffer readdir (voci NUL-separate) contiene `name` intero.
fn readdir_contains(buf: &[u8], name: &[u8]) -> bool {
    let mut i = 0usize;
    while i < buf.len() && buf[i] != 0 {
        let start = i;
        while i < buf.len() && buf[i] != 0 {
            i += 1;
        }
        if &buf[start..i] == name {
            return true;
        }
        i += 1;
    }
    false
}

/// t36 — identità stabile UUID/LABEL + discovery (Fase 16d).
/// Mount per UUID e per LABEL del secondo disco (contenuto MARKER prova il
/// disco giusto), open raw dei by-path (firma + seriale dal settore 0),
/// listing sintetizzato (/dev ∋ disk+sda, by-uuid ∋ U2, by-label ∋ L2).
/// Gira in entrambi gli ordini IDE (SWAP_DRIVES): le lettere possono
/// cambiare, le chiavi stabili no.
fn t_stable_id() -> bool {
    drain_stray();
    if libr::mkdir("/u2") < 0 {
        println!("[usertests] t36: mkdir /u2 FAILED");
        return false;
    }
    // 1. Mount per UUID.
    if libr::mount("UUID=C0FFEE01", "/u2") < 0 {
        println!("[usertests] t36: mount UUID=C0FFEE01 FAILED");
        return false;
    }
    let fd = libr::open("/u2/MARKER.TXT", 0);
    if fd < 0 {
        println!("[usertests] t36: open MARKER via UUID FAILED");
        let _ = libr::umount("/u2");
        return false;
    }
    let mut mb = [0u8; 32];
    let n = t33_read_all(fd, &mut mb);
    let _ = libr::close(fd);
    if n != DISK2_MARKER.len() || mb[..n] != *DISK2_MARKER {
        println!("[usertests] t36: MARKER via UUID corrotto (disco sbagliato?)");
        let _ = libr::umount("/u2");
        return false;
    }
    if libr::umount("/u2") < 0 {
        println!("[usertests] t36: umount /u2 FAILED");
        return false;
    }
    // 2. Mount per LABEL.
    if libr::mount("LABEL=SECOND", "/u2") < 0 {
        println!("[usertests] t36: mount LABEL=SECOND FAILED");
        return false;
    }
    let fd = libr::open("/u2/MARKER.TXT", 0);
    if fd < 0 {
        println!("[usertests] t36: open MARKER via LABEL FAILED");
        let _ = libr::umount("/u2");
        return false;
    }
    let mut mb = [0u8; 32];
    let n = t33_read_all(fd, &mut mb);
    let _ = libr::close(fd);
    let _ = libr::umount("/u2");
    if n != DISK2_MARKER.len() || mb[..n] != *DISK2_MARKER {
        println!("[usertests] t36: MARKER via LABEL corrotto");
        return false;
    }
    // 3. Open raw dei by-path: settore 0 con firma + seriale atteso.
    for path in ["/dev/disk/by-uuid/C0FFEE01", "/dev/disk/by-label/SECOND"] {
        let fd = libr::open(path, 0);
        if fd < 0 {
            println!("[usertests] t36: open raw {} FAILED", path);
            return false;
        }
        let mut sec = [0u8; 512];
        let r = libr::read_fs(fd, &mut sec, 512);
        let _ = libr::close(fd);
        if r != 512 || sec[510] != 0x55 || sec[511] != 0xAA {
            println!("[usertests] t36: settore 0 raw {} invalido", path);
            return false;
        }
        let serial = u32::from_le_bytes([sec[67], sec[68], sec[69], sec[70]]);
        if serial != 0xC0FFEE01 {
            println!("[usertests] t36: seriale raw {} = {:08X} (atteso C0FFEE01)", path, serial);
            return false;
        }
    }
    // 4. Listing sintetizzato.
    let mut eb = [0u8; 512];
    if libr::readdir("/dev", &mut eb, 512) < 0
        || !readdir_contains(&eb, b"disk")
        || !readdir_contains(&eb, b"sda")
    {
        println!("[usertests] t36: readdir /dev senza disk/sda");
        return false;
    }
    let mut eb = [0u8; 256];
    if libr::readdir("/dev/disk/by-uuid", &mut eb, 256) < 0
        || !readdir_contains(&eb, DISK2_UUID.as_bytes())
    {
        println!("[usertests] t36: readdir by-uuid senza C0FFEE01");
        return false;
    }
    let mut eb = [0u8; 256];
    if libr::readdir("/dev/disk/by-label", &mut eb, 256) < 0
        || !readdir_contains(&eb, DISK2_LABEL.as_bytes())
    {
        println!("[usertests] t36: readdir by-label senza SECOND");
        return false;
    }
    true
}

/// t32 — disk driver in userspace (Fase 16).
/// (A) Baseline: /dev/sda leggibile raw con firma boot. (B) Kill userdisk
/// (pid via `service_pid`, non figlio nostro) e attesa init-restart come
/// t27/t28: sparizione dallo slot, ricomparsa, poi /dev/sda di nuovo
/// operativo + smoke /fat/HELLO.TXT (riconnessione lazy di userfs al driver
/// rinato, senza rimontare: il mount sopravvive). Bound generosi (1000 tick
/// ~ 10 s contro restart atteso ~50), mai hang; poll throttled Livello 1.
fn t_disk() -> bool {
    drain_stray();
    if !disk_sector0_ok() {
        println!("[usertests] t32: baseline /dev/sda FAILED");
        return false;
    }
    let p1 = match libr::service_pid(libr::Service::Disk) {
        Ok(p) => p,
        Err(_) => {
            println!("[usertests] t32: service_pid(Disk) FAILED");
            return false;
        }
    };
    if libr::kill(p1, -16).is_err() {
        println!("[usertests] t32: kill userdisk pid={} FAILED", p1);
        return false;
    }
    // Fase A: sparizione dallo slot (morte osservata dal registry).
    if !libr::poll_wait(1000, libr::POLL_PERIOD_TICKS, || {
        libr::service_pid(libr::Service::Disk).is_err()
    }) {
        println!("[usertests] t32: userdisk mai sparito (timeout)");
        return false;
    }
    // Fase B: ricomparsa (init ha riavviato + registrato).
    let p2 = match libr::poll_value(1000, libr::POLL_PERIOD_TICKS, || {
        libr::service_pid(libr::Service::Disk).ok()
    }) {
        Some(p) => p,
        None => {
            println!("[usertests] t32: userdisk mai riapparso (timeout)");
            return false;
        }
    };
    println!("[usertests] t32: userdisk riavviato (pid {} -> {})", p1, p2);
    // Fase C: operativita' raw dopo il restart.
    if !libr::poll_wait(1000, libr::POLL_PERIOD_TICKS, disk_sector0_ok) {
        println!("[usertests] t32: /dev/sda mai tornato (timeout)");
        return false;
    }
    // Smoke /fat via riconnessione (il driver e' nuovo, il mount e' quello di boot).
    let fdf = libr::open_wait("/fat/HELLO.TXT", 0, 1000, libr::POLL_PERIOD_TICKS);
    if fdf < 0 {
        println!("[usertests] t32: open /fat/HELLO.TXT post-restart FAILED");
        return false;
    }
    let mut fb = [0u8; 32];
    let n = libr::read_fs(fdf, &mut fb, 32);
    let _ = libr::close(fdf);
    if n as usize != FAT_HELLO.len() || fb[..FAT_HELLO.len()] != *FAT_HELLO {
        println!("[usertests] t32: /fat/HELLO.TXT post-restart corrotto");
        return false;
    }
    true
}

// ── main ─────────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let my_pid = libr::getpid();
    println!("[usertests] suite up, pid={}", my_pid);

    let mut total = 0u32;
    let mut ok = 0u32;

    report(&mut total, &mut ok, "t1  getpid", t_getpid());
    report(&mut total, &mut ok, "t2  ticks monotonic", t_ticks());
    report(&mut total, &mut ok, "t3  heap fresh zero", t_heap_fresh_zero());
    report(&mut total, &mut ok, "t4  heap alloc reuse", t_heap_reuse());
    report(&mut total, &mut ok, "t5  spawn + child getpid", t_spawn_identity());
    report(&mut total, &mut ok, "t6  ramfs read hello.txt", t_hello());
    report(&mut total, &mut ok, "t7  ramfs write multi-chunk", t_ramfs_write_chunk());
    report(&mut total, &mut ok, "t8  ramfs mkdir + readdir", t_ramfs_mkdir());
    report(&mut total, &mut ok, "t9  fs error paths", t_fs_errors());
    report(&mut total, &mut ok, "t10 /dev/null", t_dev_null());
    report(&mut total, &mut ok, "t11 /dev/zero", t_dev_zero());
    report(&mut total, &mut ok, "t12 map_physical aliasing", t_map_alias());
    report(&mut total, &mut ok, "t13 IPC single echo", t_ipc_echo());
    report(&mut total, &mut ok, "t14 IPC multi-client", t_ipc_multiclient());
    report(&mut total, &mut ok, "t15 devfs conc + heap churn", t_devfs_concurrent_churn());
    report(&mut total, &mut ok, "t16 sched preempt ring3", t_sched_preempt());
    report(&mut total, &mut ok, "t17 sched priority", t_sched_priority());
    report(&mut total, &mut ok, "t18 cbs admission", t_cbs_admission());
    report(&mut total, &mut ok, "t19 cbs bandwidth", t_cbs_bandwidth());
    report(&mut total, &mut ok, "t20 fs async 1-in-volo", t_fs_async());
    report(&mut total, &mut ok, "t21 ipc async + backpressure", t_ipc_async());
    report(&mut total, &mut ok, "t22 lifecycle churn (riuso pid)", t_lifecycle_churn());
    report(&mut total, &mut ok, "t23 kill + exit notify", t_kill());
    report(&mut total, &mut ok, "t24 server death notify", t_server_death_notify());
    report(&mut total, &mut ok, "t25 driver death mount purge", t_driver_death_mount());
    report(&mut total, &mut ok, "t26 client death purge", t_client_death_purge());
    report(&mut total, &mut ok, "t27 devfs kill + init restart", t_devfs_restart());
    report(&mut total, &mut ok, "t28 userfs kill + full recovery", t_userfs_restart());
    report(&mut total, &mut ok, "t29 map flap isolation", t_mapflap());
    report(&mut total, &mut ok, "t30 neighbor under flood", t_neighbor());
    report(&mut total, &mut ok, "t31 kbd/tty presence", t_kbd_presence());
    report(&mut total, &mut ok, "t32 disk kill + init restart", t_disk());
    report(&mut total, &mut ok, "t33 mount/umount espliciti", t_mount());
    report(&mut total, &mut ok, "t35 resolve nome->handle lato driver", t_resolve());
    report(&mut total, &mut ok, "t36 UUID/LABEL + discovery stabile", t_stable_id());
    report(&mut total, &mut ok, "t37 ps_info snapshot processi", t_ps());
    report(&mut total, &mut ok, "t38 stat metadati senza open", t_stat());
    // t34 per ULTIMO: i drop sono irrevocabili sul canale di usertests.
    report(&mut total, &mut ok, "t34 diritti per-canale lato server", t_rights());

    println!("[usertests] SUMMARY {}/{} PASS", ok, total);
    let _ = libr::send(libr::CHANNEL_PARENT, 0x7E, ok as u64, 0); // init: test finito
    if ok == total {
        println!("[usertests] PASS {}/{}", ok, total);
        libr::exit(0);
    } else {
        println!("[usertests] FAIL {}/{}", total - ok, total);
        libr::exit(1);
    }
}

#[panic_handler]
fn panic_handler(_info: &core::panic::PanicInfo) -> ! {
    println!("[usertests] panic");
    libr::exit(1)
}
