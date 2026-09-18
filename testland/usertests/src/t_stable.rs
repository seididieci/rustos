use super::*;

/// t40 — detach + reparent a init (Fase 22). usertests spawna un MID (NEST)
/// che spawna due foglie KILLME parcheggiate (una detached via flag spawn,
/// una normale) e poi esce: la sua morte fa scattare cascata sulla normale e
/// reparent a init della detached. Osservazione SOLO via `ps` (usertests non
/// e' peer delle foglie, quindi niente EXIT_NOTIFY diretta): normale sparita,
/// detached viva con parent == init (pid 1); poi cleanup-kill della detached
/// e attesa sparizione. Bound 500 tick per fase, mai hang.
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
    // Cleanup: kill della detached + sparizione (nessuna EXIT_NOTIFY: non
    // siamo peer — si osserva solo via ps).
    if libr::kill(det as i64, 0).is_err() {
        println!("[usertests] t40: kill detached pid={} FAILED", det);
        return false;
    }
    if !helpers::poll_gone(det, 500) {
        println!("[usertests] t40: detached ancora viva dopo kill");
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

