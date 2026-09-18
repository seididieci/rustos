use super::*;

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
pub fn t_lifecycle_churn() -> bool {
    const N: usize = 42;
    const KIB: u64 = 2048; // ~2 MiB materializzati da ogni figlio
    let mut pids = Vec::new();
    for i in 0..N {
        // Spawn + CFG (modo CHURN): l'ACK porta il pid del figlio.
        let (chan, pid) = match helpers::spawn_cfg("/fat/test/testcli.bin", "utcli", 16, helpers::M_CHURN, KIB) {
            Some(x) => x,
            None => {
                println!("[usertests] t22: spawn #{} FAILED (pool esaurito?)", i);
                return false;
            }
        };
        pids.push(pid as i64);
        // Attendi la morte di QUESTO figlio (notifica kernel→parent).
        match helpers::wait_exit(chan) {
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
pub fn t_kill() -> bool {
    helpers::drain_stray();
    let (chan, pid) = match helpers::spawn_cfg("/fat/test/testcli.bin", "utcli", 16, helpers::M_KILLME, 0) {
        Some(x) => x,
        None => return false,
    };
    let code = -7i64;
    if libr::kill(pid as i64, code).is_err() {
        println!("[usertests] t23: kill(pid={}) FAILED", pid);
        return false;
    }
    match helpers::wait_exit(chan) {
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
    let (chan2, _pid2) = match helpers::spawn_cfg("/fat/test/testcli.bin", "utcli", 16, helpers::M_CHURN, 256) {
        Some(x) => x,
        None => {
            println!("[usertests] t23: spawn post-kill FAILED");
            return false;
        }
    };
    match helpers::wait_exit(chan2) {
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
pub fn t_server_death_notify() -> bool {
    helpers::drain_stray();
    let (s_chan, s_pid) = match helpers::spawn_cfg("/fat/test/testcli.bin", "utcli", 16, helpers::M_SRVDIE, 0) {
        Some(x) => x,
        None => {
            println!("[usertests] t24: spawn SRVDIE FAILED");
            return false;
        }
    };
    let (h_chan, _h_pid) = match helpers::spawn_cfg("/fat/test/testcli.bin", "utcli", 16, helpers::M_SYNCWAIT, 0) {
        Some(x) => x,
        None => {
            println!("[usertests] t24: spawn SYNCWAIT FAILED");
            return false;
        }
    };
    // Handshake: il client ha risolto Test (server vivo).
    if !helpers::recv_expect(h_chan, helpers::T_READY) {
        println!("[usertests] t24: T_READY dal client mancante");
        return false;
    }
    // Due richieste async in volo (mai risposte: il server non fa recv).
    let r1 = match libr::send_async(s_chan, helpers::T_REQ, 0xBEEF, 0) {
        Ok(r) => r,
        Err(_) => {
            println!("[usertests] t24: send_async FAILED");
            return false;
        }
    };
    let _ = libr::send_async(s_chan, helpers::T_REQ, 0xBEEF + 1, 0);
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
    if libr::send(h_chan, helpers::T_GO, 0, 0).is_err() {
        println!("[usertests] t24: T_GO al client FAILED");
        return false;
    }
    // Path sync: il client riporta T_DONE(w0=1) dopo aver visto EXIT_NOTIFY.
    if !helpers::recv_expect(h_chan, helpers::T_DONE) {
        println!("[usertests] t24: T_DONE(w0=1) dal client mancante");
        return false;
    }
    // Il client e' uscito pulito dopo il report.
    match helpers::wait_exit(h_chan) {
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
    let (chan2, _) = match helpers::spawn_cfg("/fat/test/testcli.bin", "utcli", 16, helpers::M_CHURN, 64) {
        Some(x) => x,
        None => {
            println!("[usertests] t24: spawn post-kill FAILED");
            return false;
        }
    };
    match helpers::wait_exit(chan2) {
        Some((0, _)) => true,
        _ => false,
    }
}

/// t25 — purge dei mount alla morte di un driver (Fase 14). Un driver
/// sacrificale MNTDIE registra "/dev/tdie"; il test apre /dev/tdie/null (routing al
/// driver provato), killa il driver e ne registra un secondo sullo stesso
/// prefix. Senza purge lo stale (primo in lista per resolve_mount)
/// avvelenerebbe il routing anche dopo la re-registrazione → open fallisce.
/// Con purge: serve il nuovo driver. Deterministico, nessun timing.
pub fn t_driver_death_mount() -> bool {
    helpers::drain_stray();
    let (d1_chan, d1_pid) = match helpers::spawn_cfg("/fat/test/testcli.bin", "utcli", 16, helpers::M_MNTDIE, 0) {
        Some(x) => x,
        None => {
            println!("[usertests] t25: spawn MNTDIE#1 FAILED");
            return false;
        }
    };
    if !helpers::recv_expect(d1_chan, helpers::T_READY) {
        println!("[usertests] t25: T_READY da MNTDIE#1 mancante");
        let _ = helpers::wait_exit(d1_chan);
        return false;
    }
    let fd1 = libr::open("/dev/tdie/null", 0);
    if fd1 < 0 {
        println!("[usertests] t25: open /dev/tdie/null via D1 FAILED");
        return false;
    }
    if libr::kill(d1_pid as i64, -11).is_err() {
        println!("[usertests] t25: kill D1 FAILED");
        return false;
    }
    match helpers::wait_exit(d1_chan) {
        Some((c, p)) if c == -11 && p == d1_pid as i64 => {}
        _ => {
            println!("[usertests] t25: exit notify D1 anomala");
            return false;
        }
    }
    // Re-registrazione stesso prefix: deve servire il NUOVO driver.
    let (d2_chan, d2_pid) = match helpers::spawn_cfg("/fat/test/testcli.bin", "utcli", 16, helpers::M_MNTDIE, 0) {
        Some(x) => x,
        None => {
            println!("[usertests] t25: spawn MNTDIE#2 FAILED");
            return false;
        }
    };
    if !helpers::recv_expect(d2_chan, helpers::T_READY) {
        println!("[usertests] t25: T_READY da MNTDIE#2 mancante");
        let _ = helpers::wait_exit(d2_chan);
        return false;
    }
    let fd2 = libr::open("/dev/tdie/null", 0);
    if fd2 < 0 {
        println!("[usertests] t25: open /dev/tdie/null via D2 FAILED (mount stale?)");
        return false;
    }
    // Igiene: chiudi e uccidi D2 (nessun mount orfano per i test/shell dopo).
    let _ = libr::close(fd1);
    let _ = libr::close(fd2);
    if libr::kill(d2_pid as i64, 0).is_err() {
        println!("[usertests] t25: kill D2 FAILED");
        return false;
    }
    match helpers::wait_exit(d2_chan) {
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
    n as usize >= helpers::HELLO.len() && buf[..helpers::HELLO.len()] == *helpers::HELLO
}

/// t26 — purge rings/ftable alla morte di client (Fase 14). N helper OPENDIE
/// aprono /dev/null + /dev/zero + hello.txt e muoiono SENZA close: userfs deve
/// purgare rings/ftable (con DEV_CLOSE inoltrato ai driver) senza corrompere
/// lo stato vivo. Poi smoke FS completo (null/zero/hello/write/mkdir/readdir).
pub fn t_client_death_purge() -> bool {
    helpers::drain_stray();
    const N: usize = 10;
    for i in 0..N {
        let (chan, pid) = match helpers::spawn_cfg("/fat/test/testcli.bin", "utcli", 16, helpers::M_OPENDIE, 0) {
            Some(x) => x,
            None => {
                println!("[usertests] t26: spawn OPENDIE#{} FAILED", i);
                return false;
            }
        };
        match helpers::wait_exit(chan) {
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
    if n as usize >= helpers::HELLO.len() && hb[..helpers::HELLO.len()] != *helpers::HELLO {
        println!("[usertests] t26: smoke content hello.txt FAILED");
        return false;
    }
    if (n as usize) < helpers::HELLO.len() {
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
    if !helpers::dir_contains("/", "utdir26") {
        println!("[usertests] t26: smoke readdir FAILED");
        return false;
    }
    true
}

/// t27 — init-restart di devfs (Fase 14). Bounce via init (`init_bounce`:
/// init e' parent e riavvia per la via normale) e attesa: prima sparizione dallo slot,
/// poi ricomparsa, poi /dev/null di nuovo operativo. Bound: la sparizione e'
/// solo registry (1000 tick larghi); la ricomparsa include il RELOAD DA DISCO
/// del binario (Fase 21: ~8 read FS × ~15 round-trip DISK l'uno, ognuno dei
/// quali puo' attendere un quanto sotto carico — misurato ~730 tick con
/// usertests che polla) → bound 2000, o PASS o FAIL rumoroso, mai hang.
/// userfs non viene mai toccato (il canale FS del test resta vivo).
/// NOTA: non confronta pid vecchio/nuovo (il riuso PID puo' ridare lo stesso
/// numero); osserva sparizione → ricomparsa.
pub fn t_devfs_restart() -> bool {
    helpers::drain_stray();
    let fd = libr::open("/dev/null", 0);
    if fd < 0 {
        println!("[usertests] t27: baseline open /dev/null FAILED");
        return false;
    }
    let _ = libr::close(fd);
    // Fase 35 (hardening): i servizi supervisionati si uccidono tramite init
    // (bounce: init e' parent e riavvia per la via normale). Il kill diretto
    // e' parent-scoped e qui fallirebbe (devfs e' figlio di init, non nostro).
    let p1 = match libr::init_bounce(libr::Service::Devfs) {
        Ok(p) => p,
        Err(_) => {
            println!("[usertests] t27: bounce devfs FAILED");
            return false;
        }
    };
    // Fase A: attendi sparizione dallo slot (morte osservata dal registry).
    // Poll throttled (Livello 1, buon vicinato): vedi `libr::poll_wait`.
    if !libr::poll_wait(1000, libr::POLL_PERIOD_TICKS, || {
        libr::service_pid(libr::Service::Devfs).is_err()
    }) {
        println!("[usertests] t27: devfs mai sparito (timeout)");
        return false;
    }
    // Fase B: attendi ricomparsa (init ha riavviato + registrato).
    // Bound 2000 (vedi sopra: include il reload da disco sotto carico).
    let p2 = match libr::poll_value(2000, libr::POLL_PERIOD_TICKS, || {
        libr::service_pid(libr::Service::Devfs).ok()
    }) {
        Some(p) => p,
        None => {
            println!("[usertests] t27: devfs mai riapparso (timeout)");
            return false;
        }
    };
    println!("[usertests] t27: devfs riavviato (pid {} -> {})", p1, p2);
    // Fase C: operativita' — open finche' riesce (bound come sopra: il driver
    // puo' aver registrato lo slot ma non ancora i mount).
    // Throttled via `libr::open_wait` (igiene Livello 1, buon vicinato).
    // NOTA (esperimento B): t27 PASSA anche in busy-loop non throttled —
    // lo storm del test NON e' causale del vecchio FAIL (N=1, confound).
    let fd2 = libr::open_wait("/dev/null", 0, 2000, libr::POLL_PERIOD_TICKS);
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
    n as usize >= helpers::HELLO.len() && hb[..helpers::HELLO.len()] == *helpers::HELLO
}

/// t28 — restart di userfs end-to-end (Fase 14). Uccide userfs (pid via
/// `service_pid`) e attende che init lo riavvii. Poi verifica: fixture fresh
/// funzionanti (mkdir/write/read), hello.txt ricreato, probe ramfs sparito
/// (wipe: la ramfs e' volatile, contratto codificato qui), /fat leggibile
/// (persistente su disco: contrasto), /dev/null operativo (driver
/// re-registrati via ensure_mounted). Bound generosi, mai hang.
pub fn t_userfs_restart() -> bool {
    helpers::drain_stray();
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
    // Bounce via init (Fase 35: userfs e' figlio di init, kill diretto qui
    // fallirebbe col kill parent-scoped).
    let p1 = match libr::init_bounce(libr::Service::Fs) {
        Ok(p) => p,
        Err(_) => {
            println!("[usertests] t28: bounce userfs FAILED");
            return false;
        }
    };
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
    if (n as usize) < helpers::HELLO.len() || hb[..helpers::HELLO.len()] != *helpers::HELLO {
        println!("[usertests] t28: hello.txt ricreato corrotto");
        return false;
    }
    // Wipe: il probe pre-restart non esiste piu' (ramfs volatile). NOTA: via
    // readdir, MAI via open (su ramfs open crea il file se manca!).
    if helpers::dir_contains("/", "td28probe") {
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


/// t49 — fork COW (Fase 34, ADR-0024): l'helper duplica se stesso; padre e
/// figlio scrivono un globale COW e verificano l'isolamento (nessuno vede la
/// scrittura dell'altro); il figlio riporta sul canale di nascita ed esce 0.
/// L'orchestratore osserva solo il T_DONE dell'helper (dettagli dentro).
pub fn t_fork() -> bool {
    helpers::drain_stray();
    let (chan, _pid) = match helpers::spawn_cfg(
        "/fat/test/testcli.bin", "utcli", 16, helpers::M_FORKDEMO, 0,
    ) {
        Some(x) => x,
        None => {
            println!("[usertests] t49: spawn helper FAILED");
            return false;
        }
    };
    let (ok, _) = helpers::recv_done(&[chan]);
    if !ok {
        println!("[usertests] t49: helper fork FAIL");
    }
    ok
}
