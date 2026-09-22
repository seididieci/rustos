use super::*;

// ── Fase 40.5 (t54): fd virtuali + redirect a livello libr/server ───────
// La shell e' gia' coperta da test-shell.py (19 check 40.4e); qui si fissa il
// meccanismo sotto: O_TRUNC/O_APPEND, lseek, codici errore esatti, handoff
// grant/claim modello B (happy + single-use + cancel + attestazione
// parentela), routing stdio diretto e diniego SEEK via diritti.
// Solo ramfs (FAT coperta da testfat); fixture /t54* con cleanup finale.
// Corre PRIMA di t34 (i drop dei diritti restano per ultimi).

/// Legge tutto un fd fino a EOF (0) o errore (false).
fn read_all_vec(fd: i64) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut chunk = [0u8; 2000];
    loop {
        match libr::read_fs(fd, &mut chunk, 2000) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&chunk[..n]),
            Err(_) => return None,
        }
    }
    Some(out)
}

fn write_all(fd: i64, data: &[u8]) -> bool {
    libr::write_fs(fd, data, data.len()) == Ok(data.len())
}

/// T_DONE custom con w0+w1 (il nonce viaggia in w1: `recv_done` lo scarta).
/// Come `recv_done` ma ritorna (w0, w1). Presuppone `drain_stray` chiamato.
fn recv_done2(chans: &[u64]) -> Option<(u64, u64)> {
    loop {
        match libr::recv() {
            Ok(m) => {
                let _ = libr::reply(helpers::T_ACK, 0, 0);
                if m.tag == helpers::T_DONE && chans.contains(&m.channel) {
                    return Some((m.w0, m.w1));
                }
            }
            Err(_) => return None,
        }
    }
}

fn t_trunc_append() -> bool {
    // O_TRUNC: azzera all'open (stat size 0), poi scrive da zero.
    let fd = match libr::open("/t54t.txt", libr::O_CREAT) {
        Ok(f) => f,
        Err(e) => {
            println!("[usertests] t54 trunc: create = {:?}", e);
            return false;
        }
    };
    if !write_all(fd, b"vecchio-contenuto-lungo") {
        println!("[usertests] t54 trunc: write iniziale");
        return false;
    }
    let _ = libr::close(fd);
    let fd = match libr::open("/t54t.txt", libr::O_CREAT | libr::O_TRUNC) {
        Ok(f) => f,
        Err(e) => {
            println!("[usertests] t54 trunc: reopen TRUNC = {:?}", e);
            return false;
        }
    };
    let mut st = libr::Stat { size: 99, kind: 0, readonly: false };
    if libr::stat("/t54t.txt", &mut st).is_err() || st.size != 0 {
        println!("[usertests] t54 trunc: size dopo TRUNC = {}", st.size);
        return false;
    }
    if !write_all(fd, b"nuovo") {
        println!("[usertests] t54 trunc: rewrite");
        return false;
    }
    let _ = libr::close(fd);
    // O_APPEND: l'offset e' ignorato (lseek(0) + write accoda comunque).
    let fd = match libr::open("/t54t.txt", libr::O_APPEND) {
        Ok(f) => f,
        Err(e) => {
            println!("[usertests] t54 append: open = {:?}", e);
            return false;
        }
    };
    if libr::lseek(fd, 0, libr::SEEK_SET).is_err() || !write_all(fd, b"++") {
        println!("[usertests] t54 append: seek+write");
        return false;
    }
    let _ = libr::close(fd);
    let fd = match libr::open("/t54t.txt", 0) {
        Ok(f) => f,
        Err(e) => {
            println!("[usertests] t54 append: reopen ro = {:?}", e);
            return false;
        }
    };
    let data = read_all_vec(fd);
    let _ = libr::close(fd);
    if data.as_deref() != Some(b"nuovo++".as_slice()) {
        println!("[usertests] t54 append: contenuto = {:?}", data);
        return false;
    }
    true
}

fn t_errcodes() -> bool {
    // Open senza O_CREAT su mancante = NotFound (niente creazione).
    match libr::open("/t54missing.txt", 0) {
        Err(libr::Error::NotFound) => {}
        other => {
            println!("[usertests] t54 codes: open missing = {:?} (atteso NotFound)", other);
            return false;
        }
    }
    // Open di una dir = IsDir.
    if libr::mkdir("/t54dir").is_err() {
        println!("[usertests] t54 codes: mkdir");
        return false;
    }
    match libr::open("/t54dir", 0) {
        Err(libr::Error::IsDir) => {}
        other => {
            println!("[usertests] t54 codes: open dir = {:?} (atteso IsDir)", other);
            return false;
        }
    }
    // Mkdir su esistente = Exists.
    match libr::mkdir("/t54dir") {
        Err(libr::Error::Exists) => {}
        other => {
            println!("[usertests] t54 codes: mkdir esistente = {:?} (atteso Exists)", other);
            return false;
        }
    }
    // Path vuoto = Invalid.
    match libr::open("", 0) {
        Err(libr::Error::Invalid) => {}
        other => {
            println!("[usertests] t54 codes: open vuoto = {:?} (atteso Invalid)", other);
            return false;
        }
    }
    // Grant su fd ignoto e su fd remoto = Invalid.
    match libr::dup_grant(9999) {
        Err(libr::Error::Invalid) => {}
        other => {
            println!("[usertests] t54 codes: grant badfd = {:?} (atteso Invalid)", other);
            return false;
        }
    }
    let rfd = match libr::open("/dev/null", 0) {
        Ok(f) => f,
        Err(e) => {
            println!("[usertests] t54 codes: open null = {:?}", e);
            return false;
        }
    };
    let gr = libr::dup_grant(rfd);
    let _ = libr::close(rfd);
    match gr {
        Err(libr::Error::Invalid) => {}
        other => {
            println!("[usertests] t54 codes: grant remoto = {:?} (atteso Invalid)", other);
            return false;
        }
    }
    // Claim di nonce ignoto (non-zero, fuori sentinelle ERR) = Invalid.
    match libr::dup_claim(0x5A5A_5A5A_5A5A_5A5A) {
        Err(libr::Error::Invalid) => {}
        other => {
            println!("[usertests] t54 codes: claim ignoto = {:?} (atteso Invalid)", other);
            return false;
        }
    }
    true
}

fn t_lseek() -> bool {
    let fd = match libr::open("/t54s.txt", libr::O_CREAT | libr::O_TRUNC) {
        Ok(f) => f,
        Err(e) => {
            println!("[usertests] t54 lseek: create = {:?}", e);
            return false;
        }
    };
    if !write_all(fd, b"0123456789") {
        println!("[usertests] t54 lseek: write");
        return false;
    }
    // SET/CUR/END con valori attesi.
    if libr::lseek(fd, 4, libr::SEEK_SET) != Ok(4) {
        println!("[usertests] t54 lseek: SET 4");
        return false;
    }
    let mut b = [0u8; 2];
    if libr::read_fs(fd, &mut b, 2) != Ok(2) || &b != b"45" {
        println!("[usertests] t54 lseek: read dopo SET");
        return false;
    }
    if libr::lseek(fd, 1, libr::SEEK_CUR) != Ok(7) {
        println!("[usertests] t54 lseek: CUR 1");
        return false;
    }
    let mut b1 = [0u8; 1];
    if libr::read_fs(fd, &mut b1, 1) != Ok(1) || b1 != [b'7'] {
        println!("[usertests] t54 lseek: read dopo CUR");
        return false;
    }
    if libr::lseek(fd, -3, libr::SEEK_END) != Ok(7) {
        println!("[usertests] t54 lseek: END -3");
        return false;
    }
    // Oltre EOF lecito (read torna 0); negativo e whence ignota = Invalid.
    if libr::lseek(fd, 5, libr::SEEK_END) != Ok(15) {
        println!("[usertests] t54 lseek: END +5");
        return false;
    }
    match libr::read_fs(fd, &mut b1, 1) {
        Ok(0) => {}
        other => {
            println!("[usertests] t54 lseek: read oltre EOF = {:?} (atteso 0)", other);
            return false;
        }
    }
    match libr::lseek(fd, -1, libr::SEEK_SET) {
        Err(libr::Error::Invalid) => {}
        other => {
            println!("[usertests] t54 lseek: SET -1 = {:?} (atteso Invalid)", other);
            return false;
        }
    }
    // Two-phase: a rifiuto l'offset resta quello di prima (15 → EOF).
    match libr::read_fs(fd, &mut b1, 1) {
        Ok(0) => {}
        other => {
            println!("[usertests] t54 lseek: offset mutato dal rifiuto: {:?}", other);
            return false;
        }
    }
    match libr::lseek(fd, 0, 99) {
        Err(libr::Error::Invalid) => {}
        other => {
            println!("[usertests] t54 lseek: whence 99 = {:?} (atteso Invalid)", other);
            return false;
        }
    }
    let _ = libr::close(fd);
    // Remoto (offset vive in userfs): Invalid.
    let rfd = match libr::open("/dev/null", 0) {
        Ok(f) => f,
        Err(e) => {
            println!("[usertests] t54 lseek: open null = {:?}", e);
            return false;
        }
    };
    let lr = libr::lseek(rfd, 0, libr::SEEK_SET);
    let _ = libr::close(rfd);
    match lr {
        Err(libr::Error::Invalid) => {}
        other => {
            println!("[usertests] t54 lseek: remoto = {:?} (atteso Invalid)", other);
            return false;
        }
    }
    true
}

fn t_dup() -> bool {
    // Happy path: fixture + offset 3 pre-grant → l'helper legge "DEF".
    let fd = match libr::open("/t54dup.txt", libr::O_CREAT | libr::O_TRUNC) {
        Ok(f) => f,
        Err(e) => {
            println!("[usertests] t54 dup: create = {:?}", e);
            return false;
        }
    };
    if !write_all(fd, b"ABCDEF") || libr::lseek(fd, 3, libr::SEEK_SET) != Ok(3) {
        println!("[usertests] t54 dup: setup");
        return false;
    }
    let nonce = match libr::dup_grant(fd) {
        Ok(n) if n != 0 => n,
        other => {
            println!("[usertests] t54 dup: grant = {:?} (atteso nonce != 0)", other);
            return false;
        }
    };
    let (c_chan, _) = match helpers::spawn_cfg(
        "/fat/test/testcli.bin", "utcli", 16, helpers::M_DUPCLAIM, nonce,
    ) {
        Some(x) => x,
        None => {
            println!("[usertests] t54 dup: spawn claimer FAILED");
            return false;
        }
    };
    let (w0, _) = match recv_done2(&[c_chan]) {
        Some(x) => x,
        None => {
            println!("[usertests] t54 dup: nessun T_DONE dal claimer");
            return false;
        }
    };
    let _ = helpers::wait_exit(c_chan);
    let _ = libr::close(fd);
    if w0 != 1 {
        println!("[usertests] t54 dup: claimer w0={} (atteso 1)", w0);
        return false;
    }
    // Single-use: grant consumato dal claim → cancel + reclaim = Invalid.
    let _ = libr::dup_cancel(nonce);
    match libr::dup_claim(nonce) {
        Err(libr::Error::Invalid) => {}
        other => {
            println!("[usertests] t54 dup: reclaim = {:?} (atteso Invalid)", other);
            return false;
        }
    }
    // Attestazione: A granta sul SUO canale, B (figlio nostro, non di A)
    // prova il claim → Invalid. Il nonce viaggia in T_DONE(w1).
    let sfd = match libr::open("/t54sib.txt", libr::O_CREAT | libr::O_TRUNC) {
        Ok(f) => f,
        Err(e) => {
            println!("[usertests] t54 dup: create sib = {:?}", e);
            return false;
        }
    };
    if !write_all(sfd, b"sib") {
        println!("[usertests] t54 dup: write sib");
        return false;
    }
    let _ = libr::close(sfd);
    let (a_chan, _) = match helpers::spawn_cfg(
        "/fat/test/testcli.bin", "utcli", 16, helpers::M_DUPGRANT, 0,
    ) {
        Some(x) => x,
        None => {
            println!("[usertests] t54 dup: spawn granter FAILED");
            return false;
        }
    };
    let (aw0, anonce) = match recv_done2(&[a_chan]) {
        Some(x) => x,
        None => {
            println!("[usertests] t54 dup: nessun T_DONE dal granter");
            return false;
        }
    };
    let _ = helpers::wait_exit(a_chan);
    if aw0 != 1 || anonce == 0 {
        println!("[usertests] t54 dup: granter w0={} nonce={} (atteso 1/!=0)", aw0, anonce);
        return false;
    }
    let (b_chan, _) = match helpers::spawn_cfg(
        "/fat/test/testcli.bin", "utcli", 16, helpers::M_DUPSIBCLAIM, anonce,
    ) {
        Some(x) => x,
        None => {
            println!("[usertests] t54 dup: spawn sib FAILED");
            return false;
        }
    };
    let (bw0, _) = match recv_done2(&[b_chan]) {
        Some(x) => x,
        None => {
            println!("[usertests] t54 dup: nessun T_DONE dal sib");
            return false;
        }
    };
    let _ = helpers::wait_exit(b_chan);
    if bw0 != 1 {
        println!("[usertests] t54 dup: sib w0={} (atteso 1 = Invalid osservato)", bw0);
        return false;
    }
    true
}

fn t_stdio() -> bool {
    // Routing stdout diretto (niente shell): println! nel file con stdio
    // attivo; clear SEMPRE (anche sui fail: l'output della suite non deve
    // sparire in un file).
    let fd = match libr::open("/t54out.txt", libr::O_CREAT | libr::O_TRUNC) {
        Ok(f) => f,
        Err(e) => {
            println!("[usertests] t54 stdio: create = {:?}", e);
            return false;
        }
    };
    libr::set_stdio([-1, fd, -1]);
    libr::println!("t54-stdio-marker");
    libr::clear_stdio();
    let _ = libr::close(fd);
    let fd = match libr::open("/t54out.txt", 0) {
        Ok(f) => f,
        Err(e) => {
            println!("[usertests] t54 stdio: reopen = {:?}", e);
            return false;
        }
    };
    let data = read_all_vec(fd);
    let _ = libr::close(fd);
    let data = match data {
        Some(d) => d,
        None => {
            println!("[usertests] t54 stdio: read");
            return false;
        }
    };
    if !data.starts_with(b"t54-stdio-marker\n") {
        println!("[usertests] t54 stdio: contenuto inatteso len={}", data.len());
        return false;
    }
    // Stdin diretto: drain byte == contenuto, poi None (EOF).
    let fd = match libr::open("/t54out.txt", 0) {
        Ok(f) => f,
        Err(e) => {
            println!("[usertests] t54 stdio: open stdin = {:?}", e);
            return false;
        }
    };
    libr::set_stdio([fd, -1, -1]);
    let mut got = Vec::new();
    loop {
        match libr::stdin_byte() {
            Some(b) => got.push(b),
            None => break,
        }
    }
    libr::clear_stdio();
    let _ = libr::close(fd);
    if got != data {
        println!("[usertests] t54 stdio: stdin len={} (atteso {})", got.len(), data.len());
        return false;
    }
    true
}

fn t_seekdeny() -> bool {
    // Diniego SEEK via diritti (canale dell'helper, mai suite): lseek = Failed.
    let (h_chan, _) = match helpers::spawn_cfg(
        "/fat/test/testcli.bin", "utcli", 16, helpers::M_SEEKDENY, 0,
    ) {
        Some(x) => x,
        None => {
            println!("[usertests] t54 seekdeny: spawn FAILED");
            return false;
        }
    };
    let (w0, detail) = match recv_done2(&[h_chan]) {
        Some(x) => x,
        None => {
            println!("[usertests] t54 seekdeny: nessun T_DONE");
            return false;
        }
    };
    let _ = helpers::wait_exit(h_chan);
    if w0 != 1 {
        println!("[usertests] t54 seekdeny: w0={} detail={} (atteso 1)", w0, detail);
        return false;
    }
    true
}

/// t54 — fd virtuali + redirect a livello libr/server (Fase 40.5).
pub fn t_fd_virtual_redirect() -> bool {
    helpers::drain_stray();
    let mut ok = true;
    if !t_trunc_append() {
        ok = false;
    }
    if !t_errcodes() {
        ok = false;
    }
    if !t_lseek() {
        ok = false;
    }
    if !t_dup() {
        ok = false;
    }
    if !t_stdio() {
        ok = false;
    }
    if !t_seekdeny() {
        ok = false;
    }
    // Cleanup fixture (ramfs condivisa con shell e altri test).
    let _ = libr::remove("/t54t.txt");
    let _ = libr::remove("/t54s.txt");
    let _ = libr::remove("/t54dir");
    let _ = libr::remove("/t54dup.txt");
    let _ = libr::remove("/t54sib.txt");
    let _ = libr::remove("/t54seek.txt");
    let _ = libr::remove("/t54out.txt");
    ok
}
