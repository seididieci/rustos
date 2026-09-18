use super::*;

// ── singoli test (ritornano true = pass) ─────────────────────────────

pub fn t_getpid() -> bool {
    libr::getpid() > 0
}

pub fn t_ticks() -> bool {
    let t1 = libr::get_ticks();
    libr::spin_ticks(1);
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
pub fn t_heap_fresh_zero() -> bool {
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

pub fn t_heap_reuse() -> bool {
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

pub fn t_spawn_identity() -> bool {
    // spawn ritorna un canale verso il figlio (ADR-0008); il figlio conferma
    // con ACK portando il proprio pid. Il parent non conosce il pid (identita'
    // interna al kernel): verifica che il canale sia valido e che il figlio
    // abbia risposto (ack > 0) e completato (DONE).
    match helpers::spawn_cfg("/fat/test/testcli.bin", "utcli", 16, helpers::M_ECHO, 0) {
        Some((chan, ack_pid)) => {
            // rounds=0 → nessun REQ, solo DONE. Risponde/scarta eventuali
            // residui finche' non arriva il DONE del figlio.
            chan > 0 && ack_pid > 0 && helpers::recv_expect(chan, helpers::T_DONE)
        }
        None => false,
    }
}

pub fn t_hello() -> bool {
    let fd = libr::open("hello.txt", 0);
    if fd < 0 {
        return false;
    }
    let mut buf = [0u8; 64];
    let n = libr::read_fs(fd, &mut buf, 64);
    if !(n as usize >= helpers::HELLO.len() && buf[..helpers::HELLO.len()] == *helpers::HELLO) {
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

pub fn t_ramfs_write_chunk() -> bool {
    let fd = libr::open("utdata.bin", libr::O_CREAT);
    if fd < 0 {
        return false;
    }
    // 3 chunk da 3000 (9 KiB totali > 1 pagina ring da 4088 B): multi-call
    // write con chunking client (Fase 10.2).
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
    let ok = helpers::read_all(fd2, &mut all, 9000);
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

pub fn t_ramfs_mkdir() -> bool {
    if libr::mkdir("utdir") < 0 {
        return false;
    }
    helpers::dir_contains("/", "utdir")
}

pub fn t_fs_errors() -> bool {
    // open di path vuoto → -1 (path_len 0).
    let a = libr::open("", 0) < 0;
    // read/write/close su fd inesistente → -1 (fd non nella tabella del server).
    let b = libr::read_fs(-1, &mut [0u8; 8], 8) < 0;
    let c = libr::write_fs(-1, &[0u8; 8], 8) < 0;
    let d = libr::close(-1) < 0;
    a && b && c && d
}

pub fn t_dev_null() -> bool {
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

pub fn t_dev_zero() -> bool {
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

/// t44 — mmap anonimo nel basso canonico (Fase M0): pattern R/W, multi-PT,
/// fixed/overlap, munmap intero + riuso, integrazione syscall (write da
/// buffer mappato). Solo path suite-safe (rifiuti = -1, mai fault): i
/// negativi-con-fault fermerebbero il sistema, come il NULL test kernel.
pub fn t_mmap() -> bool {
    // 1. Anonima 3 pagine: base bassa + allineata, pattern oltre le pagine.
    let a = match libr::mmap(0, 3 * 4096) {
        Ok(a) => a,
        Err(_) => return false,
    };
    if a < 0x10_0000 || a & 0xFFF != 0 {
        return false;
    }
    for i in 0..3 * 4096usize {
        unsafe { core::ptr::write_volatile((a + i) as *mut u8, i.wrapping_mul(7) as u8); }
    }
    for i in 0..3 * 4096usize {
        if unsafe { core::ptr::read_volatile((a + i) as *const u8) } != i.wrapping_mul(7) as u8 {
            return false;
        }
    }
    // 2. Multi-PT: 3 MiB, spot-check per pagina (1536 fault demand-zero).
    let big = match libr::mmap(0, 3 * 1024 * 1024) {
        Ok(a) => a,
        Err(_) => return false,
    };
    if big == a {
        return false; // basi distinte (no alias)
    }
    let npages = 3 * 1024 * 1024 / 4096;
    for p in 0..npages {
        unsafe { core::ptr::write_volatile((big + p * 4096) as *mut u8, (p & 0xFF) as u8); }
    }
    for p in 0..npages {
        if unsafe { core::ptr::read_volatile((big + p * 4096) as *const u8) } != (p & 0xFF) as u8 {
            return false;
        }
    }
    // 3. Fixed + overlap: libero ok, occupati/disallineati/len-0 rifiutati.
    let f = match libr::mmap_fixed(0x50_0000, 8192) {
        Ok(x) => x,
        Err(_) => return false,
    };
    if f != 0x50_0000 {
        return false;
    }
    if libr::mmap_fixed(a, 4096).is_ok() {
        return false; // dentro `a`
    }
    if libr::mmap(big + 4096, 4096).is_ok() {
        return false; // hint dentro `big` (strict: niente fallback)
    }
    if libr::mmap(0, 0).is_ok() {
        return false; // len 0
    }
    if libr::mmap(a + 1, 4096).is_ok() {
        return false; // hint disallineato
    }
    // 4. Munmap: parziale rifiutato senza stato, interi ok + riuso fixed.
    if libr::munmap(a + 4096, 4096).is_ok() {
        return false; // split = Err in M0
    }
    if unsafe { core::ptr::read_volatile(a as *const u8) } != 0 {
        return false; // ancora intatta (pattern[0] = 0)
    }
    if libr::munmap(a, 3 * 4096).is_err() {
        return false;
    }
    if libr::munmap(big, 3 * 1024 * 1024).is_err() {
        return false;
    }
    if libr::munmap(f, 8192).is_err() {
        return false;
    }
    let a2 = match libr::mmap_fixed(a, 3 * 4096) {
        Ok(x) => x,
        Err(_) => return false,
    };
    if a2 != a {
        return false;
    }
    for i in 0..64usize {
        if unsafe { core::ptr::read_volatile((a2 + i) as *const u8) } != 0 {
            return false; // fresca = zeri (frame nuovi, mai stale)
        }
    }
    let _ = libr::munmap(a2, 3 * 4096);
    // 5. Integrazione syscall: sys_write (fd 2, seriale) da buffer mappato —
    // is_user_range accetta le VMA (rifiuto = -1, non fault).
    let m = match libr::mmap(0, 4096) {
        Ok(x) => x,
        Err(_) => return false,
    };
    let msg = b"mmap-write-ok\n";
    for (i, &b) in msg.iter().enumerate() {
        unsafe { core::ptr::write_volatile((m + i) as *mut u8, b); }
    }
    let n = libr::write(2, m as *const u8, msg.len());
    let _ = libr::munmap(m, 4096);
    if n != msg.len() as i64 {
        return false;
    }
    true
}

pub fn t_map_alias() -> bool {    if libr::map_physical(libr::MAP_TEST_PHYS, helpers::VA_A, 1).is_err() {
        return false;
    }
    if libr::map_physical(libr::MAP_TEST_PHYS, helpers::VA_B, 1).is_err() {
        return false;
    }
    let pa = helpers::VA_A as *mut u8;
    let pb = helpers::VA_B as *const u8;
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

