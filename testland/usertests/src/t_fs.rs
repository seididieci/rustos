use super::*;

/// t33 — mount/umount espliciti (Fase 16b).
/// mkdir /mnt (ramfs) → mount /dev/sda /mnt → /mnt/HELLO.TXT col contenuto
/// FAT → umount busy rifiutato con fd aperto → close → umount ok → /mnt
/// torna ramfs (readdir senza entry FAT). Error paths: sorgente inesistente
/// o senza disco, target invalido, doppio mount (idempotente: ok e ancora
/// operativo), umount di non-montato e di `/`. Ultimo test: dopo solo shell.
pub fn t_mount() -> bool {
    helpers::drain_stray();
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
    let n = helpers::t33_read_all(fd, &mut hb);
    if n != helpers::FAT_HELLO.len() || hb[..helpers::FAT_HELLO.len()] != *helpers::FAT_HELLO {
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
pub fn t_resolve() -> bool {
    helpers::drain_stray();
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
    let n = helpers::t33_read_all(fd, &mut hb);
    let _ = libr::close(fd);
    if n != helpers::FAT_HELLO.len() || hb[..helpers::FAT_HELLO.len()] != *helpers::FAT_HELLO {
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
pub fn t_rights() -> bool {
    helpers::drain_stray();
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
    let n = helpers::t33_read_all(fd, &mut hb);
    let _ = libr::close(fd);
    if n != helpers::FAT_HELLO.len() || hb[..helpers::FAT_HELLO.len()] != *helpers::FAT_HELLO {
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
pub fn t_ps() -> bool {
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
pub fn t_stat() -> bool {
    let mut st = libr::Stat { size: 0, kind: 0, readonly: false };
    // File ramfs: size esatta, non readonly.
    if libr::stat("hello.txt", &mut st) != 0
        || !st.is_file()
        || st.size as usize != helpers::HELLO.len()
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

/// t39 — servizi da disco (Fase 21): i binari in `/bin` e `/test` (iniettati
/// a build via mcopy) esistono e sono non vuoti, e tutti i servizi sono up
/// per nome (= il boot da disco ha funzionato: console/kbd/tty/shell non
/// sono piu' embedded ma girano).
pub fn t_diskboot() -> bool {
    let mut st = libr::Stat { size: 0, kind: 0, readonly: false };
    for path in [
        "/fat/bin/console.bin",
        "/fat/bin/uptime.bin",
        "/fat/bin/devfs.bin",
        "/fat/bin/kbd.bin",
        "/fat/bin/tty.bin",
        "/fat/bin/shell.bin",
        "/fat/test/testfs.bin",
        "/fat/test/testfat.bin",
        "/fat/test/tests.bin",
    ] {
        if libr::stat(path, &mut st) != 0 || !st.is_file() || st.size == 0 {
            println!("[usertests] t39: {} mancante/vuoto", path);
            return false;
        }
    }
    for svc in [
        libr::Service::Console,
        libr::Service::Fs,
        libr::Service::Devfs,
        libr::Service::Kbd,
        libr::Service::Tty,
        libr::Service::Disk,
    ] {
        if libr::service_lookup(svc).is_err() {
            println!("[usertests] t39: servizio {:?} non registrato", svc as u64);
            return false;
        }
    }
    true
}

