use super::*;

/// t40 — detach + reparent a init (Fase 22). usertests spawna un MID (NEST)
/// che spawna due foglie parcheggiate (detached in ORPHAN, normale in KILLME)
/// e poi esce: la sua morte fa scattare cascata sulla normale e reparent a
/// init della detached. Osservazione SOLO via `ps` (usertests non e' peer
/// delle foglie, quindi niente EXIT_NOTIFY diretta): normale sparita,
/// detached viva con parent == init (pid 1); poi la detached esce DA SOLA
/// (~100 tick dopo la notify di morte del MID, igiene senza kill: il kill
/// diretto e' parent-scoped dal Fase 35 e nessuno fuori puo' pulirla).
/// Bound 500 tick per fase, mai hang.
pub fn t_detach() -> bool {
    helpers::drain_stray();
    let (mid_chan, _mid_pid) = match helpers::spawn_cfg("/fat/test/testcli.bin", "utcli", 16, helpers::M_NEST, 0) {
        Some(x) => x,
        None => {
            println!("[usertests] t40: spawn NEST FAILED");
            return false;
        }
    };
    let (det, norm) = match helpers::recv_ready(mid_chan) {
        Some((d, n)) if d != 0 && n != 0 => (d, n),
        _ => {
            println!("[usertests] t40: T_READY dal MID mancante");
            return false;
        }
    };
    // Il MID esce (exit 0): la cascata scatta qui.
    match helpers::wait_exit(mid_chan) {
        Some((0, _)) => {}
        other => {
            println!("[usertests] t40: exit del MID anomala: {:?}", other);
            return false;
        }
    }
    // Foglia normale: cascata → deve sparire da ps.
    if !helpers::poll_gone(norm, 500) {
        println!("[usertests] t40: foglia normale ancora viva (pid={})", norm);
        return false;
    }
    // Detached: viva e ri-parentata a init (pid 1).
    match helpers::poll_parent(det, 500) {
        Some(Some(1)) => {}
        other => {
            println!("[usertests] t40: detached parent errato: {:?}", other);
            return false;
        }
    }
    // Cleanup senza kill (Fase 35): la detached esce da sola ~100 tick dopo
    // la notify (ORPHAN); qui si attende solo la sparizione via ps.
    if !helpers::poll_gone(det, 500) {
        println!("[usertests] t40: detached ancora viva (self-exit mancato)");
        return false;
    }
    true
}

/// t36 — identità stabile UUID/LABEL + discovery (Fase 16d).
/// Mount per UUID e per LABEL del secondo disco (contenuto MARKER prova il
/// disco giusto), open raw dei by-path (firma + seriale dal settore 0),
/// listing sintetizzato (/dev ∋ disk+sda, by-uuid ∋ U2, by-label ∋ L2).
/// Gira in entrambi gli ordini IDE (SWAP_DRIVES): le lettere possono
/// cambiare, le chiavi stabili no.
pub fn t_stable_id() -> bool {
    helpers::drain_stray();
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
    let n = helpers::t33_read_all(fd, &mut mb);
    let _ = libr::close(fd);
    if n != helpers::DISK2_MARKER.len() || mb[..n] != *helpers::DISK2_MARKER {
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
    let n = helpers::t33_read_all(fd, &mut mb);
    let _ = libr::close(fd);
    let _ = libr::umount("/u2");
    if n != helpers::DISK2_MARKER.len() || mb[..n] != *helpers::DISK2_MARKER {
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
        || !helpers::readdir_contains(&eb, b"disk")
        || !helpers::readdir_contains(&eb, b"sda")
    {
        println!("[usertests] t36: readdir /dev senza disk/sda");
        return false;
    }
    let mut eb = [0u8; 256];
    if libr::readdir("/dev/disk/by-uuid", &mut eb, 256) < 0
        || !helpers::readdir_contains(&eb, helpers::DISK2_UUID.as_bytes())
    {
        println!("[usertests] t36: readdir by-uuid senza C0FFEE01");
        return false;
    }
    let mut eb = [0u8; 256];
    if libr::readdir("/dev/disk/by-label", &mut eb, 256) < 0
        || !helpers::readdir_contains(&eb, helpers::DISK2_LABEL.as_bytes())
    {
        println!("[usertests] t36: readdir by-label senza SECOND");
        return false;
    }
    true
}

/// t32 — disk driver in userspace (Fase 16).
/// (A) Baseline: /dev/sda leggibile raw con firma boot. (B) Bounce via init
/// (`init_bounce`, Fase 35) e attesa init-restart come t27/t28: sparizione dallo slot, ricomparsa, poi /dev/sda di nuovo
/// operativo + smoke /fat/HELLO.TXT (riconnessione lazy di userfs al driver
/// rinato, senza rimontare: il mount sopravvive). Bound generosi (1000 tick
/// ~ 10 s contro restart atteso ~50), mai hang; poll throttled Livello 1.
pub fn t_disk() -> bool {
    helpers::drain_stray();
    if !helpers::disk_sector0_ok() {
        println!("[usertests] t32: baseline /dev/sda FAILED");
        return false;
    }
    // Bounce via init (Fase 35: userdisk e' figlio di init, kill diretto qui
    // fallirebbe col kill parent-scoped).
    let p1 = match libr::init_bounce(libr::Service::Disk) {
        Ok(p) => p,
        Err(_) => {
            println!("[usertests] t32: bounce userdisk FAILED");
            return false;
        }
    };
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
    if !libr::poll_wait(1000, libr::POLL_PERIOD_TICKS, helpers::disk_sector0_ok) {
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
    if n as usize != helpers::FAT_HELLO.len() || fb[..helpers::FAT_HELLO.len()] != *helpers::FAT_HELLO {
        println!("[usertests] t32: /fat/HELLO.TXT post-restart corrotto");
        return false;
    }
    true
}

/// t50 — hardening (Fase 35, ADR-0026): i cancelli kernel/FS respingono i
/// tentativi ostili di un processo locale. (A) un helper prova a killare un
/// fratello (non suo figlio) e a registrare un servizio di sistema (`Init`):
/// entrambi rifiutati. (B) usertests prova a mappare RAM del kernel con
/// `map_physical`: rifiutato. (C) usertests prova a killare un servizio che
/// non e' suo figlio (devfs): rifiutato (il servizio resta vivo).
pub fn t_hardening() -> bool {
    helpers::drain_stray();
    // Vittima: un KILLME parcheggiato, figlio di usertests (fratello
    // dell'helper HARDEN, quindi NON suo figlio).
    let (b_chan, b_pid) = match helpers::spawn_cfg(
        "/fat/test/testcli.bin", "utcli", 16, helpers::M_KILLME, 0,
    ) {
        Some(x) => x,
        None => {
            println!("[usertests] t50: spawn vittima FAILED");
            return false;
        }
    };
    let (h_chan, _) = match helpers::spawn_cfg(
        "/fat/test/testcli.bin", "utcli", 16, helpers::M_HARDEN, b_pid,
    ) {
        Some(x) => x,
        None => {
            let _ = libr::kill(b_pid as i64, 0);
            let _ = helpers::wait_exit(b_chan);
            println!("[usertests] t50: spawn harden FAILED");
            return false;
        }
    };
    let (ok, detail) = helpers::recv_done(&[h_chan]);
    if !ok {
        println!("[usertests] t50: helper harden FAIL (detail={})", detail);
        let _ = libr::kill(b_pid as i64, 0);
        let _ = helpers::wait_exit(b_chan);
        return false;
    }
    // La vittima deve essere ancora viva (il kill ostile non e' passato).
    if libr::ps_info(b_pid as u32).is_none() {
        println!("[usertests] t50: vittima uccisa da kill non-figlio!");
        return false;
    }
    // (B) map_physical di RAM del kernel (0x100000) rifiutato.
    if libr::map_physical(0x10_0000, helpers::VA_A, 1).is_ok() {
        println!("[usertests] t50: map_physical RAM kernel NON rifiutato!");
        let _ = libr::kill(b_pid as i64, 0);
        let _ = helpers::wait_exit(b_chan);
        return false;
    }
    // (C) kill di un servizio non-figlio (devfs, figlio di init) rifiutato.
    if let Ok(devfs_pid) = libr::service_pid(libr::Service::Devfs) {
        if libr::kill(devfs_pid, 0).is_ok() {
            println!("[usertests] t50: kill di un servizio non-figlio NON rifiutato!");
            let _ = libr::kill(b_pid as i64, 0);
            let _ = helpers::wait_exit(b_chan);
            return false;
        }
    }
    // Cleanup: la vittima e' nostra figlia (kill consentito).
    let _ = libr::kill(b_pid as i64, 0);
    let _ = helpers::wait_exit(b_chan);
    true
}

/// t51 — identita' misurata (Fase 36, Strato 2 di ADR-0026).
/// (A) `peer_info` sui servizi caricati da disco (Console, Devfs) == manifest
/// generato: il kernel misura gli stessi byte che init ha verificato al boot.
/// (Su userfs embedded non c'e' pinning da manifest: `HASH_USERFS` non esiste
/// per costruzione — userfs incorpora il manifest e il suo hash sarebbe un
/// ciclo instabile, vedi gen-service-hashes.sh.)
/// (B) stabilita': due istanze dello stesso helper hanno lo stesso hash.
/// (C) same-image positivo: due istanze NON-figlie-di-init dello stesso
/// binario si passano un prefix (X2 rimpiazza X1 vivo) — con le sole regole
/// Fase 35 sarebbe rifiutato. Osservabile: kill X1 → il mount sopravvive
/// (driver X2), open ancora ok.
/// (D) squat negativo: Y (binario diverso) tenta il replace del prefix di X
/// vivo → rifiutato; alla morte di X il mount e' purgato (open fallisce).
/// Se il replace fosse passato, Y servirebbe e l'open riuscirebbe.
/// (E) `peer_info` a canale morto → Err.
pub fn t_identity() -> bool {
    helpers::drain_stray();
    // (A) hash dal kernel == manifest di build, per due servizi da disco.
    for (svc, expected, name) in [
        (libr::Service::Console, crate::HASH_USERCONSOLE, "Console"),
        (libr::Service::Devfs, crate::HASH_USERDEVFS, "Devfs"),
    ] {
        let chan = match libr::service_lookup(svc) {
            Ok(c) => c as u64,
            Err(_) => {
                println!("[usertests] t51: lookup {} FAILED", name);
                return false;
            }
        };
        match libr::peer_info(chan) {
            Ok(h) if h == expected => {}
            Ok(h) => {
                println!("[usertests] t51: peer_info({})={:#x} != manifest {:#x}", name, h, expected);
                return false;
            }
            Err(_) => {
                println!("[usertests] t51: peer_info({}) FAILED", name);
                return false;
            }
        }
    }
    // (B) due istanze dello stesso helper: stesso hash, entrambe vive.
    let (k1_chan, k1_pid) = match helpers::spawn_cfg(
        "/fat/test/testcli.bin", "utcli", 16, helpers::M_KILLME, 0,
    ) {
        Some(x) => x,
        None => {
            println!("[usertests] t51: spawn K1 FAILED");
            return false;
        }
    };
    let (k2_chan, k2_pid) = match helpers::spawn_cfg(
        "/fat/test/testcli.bin", "utcli", 16, helpers::M_KILLME, 0,
    ) {
        Some(x) => x,
        None => {
            println!("[usertests] t51: spawn K2 FAILED");
            let _ = libr::kill(k1_pid as i64, 0);
            let _ = helpers::wait_exit(k1_chan);
            return false;
        }
    };
    let (h1, h2) = (libr::peer_info(k1_chan), libr::peer_info(k2_chan));
    // spawn_cfg riporta in ack.w0 il pid (testcli risponde T_ACK con getpid,
    // come usa t50): kill diretto, nostre figlie.
    if h1.is_err() || h1 != h2 {
        println!("[usertests] t51: hash instabili tra istanze");
        let _ = libr::kill(k1_pid as i64, 0);
        let _ = helpers::wait_exit(k1_chan);
        let _ = libr::kill(k2_pid as i64, 0);
        let _ = helpers::wait_exit(k2_chan);
        return false;
    }
    let _ = libr::kill(k1_pid as i64, 0);
    let _ = helpers::wait_exit(k1_chan);
    let _ = libr::kill(k2_pid as i64, 0);
    let _ = helpers::wait_exit(k2_chan);
    // (C) same-image: X1 registra /dev/t51, X2 (stesso binario, non init-child)
    // lo rimpiazza da vivo. Kill X1 → il mount deve sopravvivere (driver X2).
    let (x1_chan, _) = match helpers::spawn_cfg(
        "/fat/test/testcli.bin", "utcli", 16, helpers::M_REG51, 0,
    ) {
        Some(x) => x,
        None => {
            println!("[usertests] t51: spawn X1 FAILED");
            return false;
        }
    };
    if helpers::recv_ready(x1_chan).is_none() {
        println!("[usertests] t51: T_READY X1 mancante");
        return false;
    }
    let (x2_chan, _) = match helpers::spawn_cfg(
        "/fat/test/testcli.bin", "utcli", 16, helpers::M_REG51, 0,
    ) {
        Some(x) => x,
        None => {
            println!("[usertests] t51: spawn X2 FAILED");
            let _ = libr::kill(libr::peer_pid(x1_chan).unwrap_or(-1), 0);
            let _ = helpers::wait_exit(x1_chan);
            return false;
        }
    };
    if helpers::recv_ready(x2_chan).is_none() {
        println!("[usertests] t51: T_READY X2 mancante");
        return false;
    }
    let x1_pid = libr::peer_pid(x1_chan).unwrap_or(-1);
    let x2_pid = libr::peer_pid(x2_chan).unwrap_or(-1);
    let fd = libr::open("/dev/t51/null", 0);
    if fd < 0 {
        println!("[usertests] t51: open pre-kill FAILED");
        let _ = libr::kill(x1_pid, 0);
        let _ = helpers::wait_exit(x1_chan);
        let _ = libr::kill(x2_pid, 0);
        let _ = helpers::wait_exit(x2_chan);
        return false;
    }
    let _ = libr::close(fd);
    let _ = libr::kill(x1_pid, 0);
    let _ = helpers::wait_exit(x1_chan);
    let fd = libr::open("/dev/t51/null", 0);
    if fd < 0 {
        println!("[usertests] t51: open post-kill X1 FAILED (replace same-image non passato?)");
        let _ = libr::kill(x2_pid, 0);
        let _ = helpers::wait_exit(x2_chan);
        return false;
    }
    let _ = libr::close(fd);
    // (D) squat: X2 resta vivo e proprietario; Y (binario diverso) tenta il
    // replace → rifiutato. Kill X2 → mount purgato → open deve FALLIRE (se il
    // replace fosse passato, Y servirebbe e l'open riuscirebbe).
    let (y_chan, _) = match helpers::spawn_cfg(
        "/fat/test/testspin.bin", "utspin", 16, helpers::SPIN_SQUAT_MAGIC, 0,
    ) {
        Some(x) => x,
        None => {
            println!("[usertests] t51: spawn Y FAILED");
            let _ = libr::kill(x2_pid, 0);
            let _ = helpers::wait_exit(x2_chan);
            return false;
        }
    };
    if helpers::recv_ready(y_chan).is_none() {
        println!("[usertests] t51: T_READY Y mancante");
        let _ = libr::kill(x2_pid, 0);
        let _ = helpers::wait_exit(x2_chan);
        return false;
    }
    let y_pid = libr::peer_pid(y_chan).unwrap_or(-1);
    let _ = libr::kill(x2_pid, 0);
    let _ = helpers::wait_exit(x2_chan);
    // (E) canale morto → Err (stesso canale di X2, peer reclamato).
    if libr::peer_info(x2_chan).is_ok() {
        println!("[usertests] t51: peer_info a canale morto NON rifiutato!");
        let _ = libr::kill(y_pid, 0);
        let _ = helpers::wait_exit(y_chan);
        return false;
    }
    let fd = libr::open("/dev/t51/null", 0);
    let _ = libr::kill(y_pid, 0);
    let _ = helpers::wait_exit(y_chan);
    if fd >= 0 {
        println!("[usertests] t51: open dopo purge RIUSCITO (squat passato?)");
        let _ = libr::close(fd);
        return false;
    }
    true
}
