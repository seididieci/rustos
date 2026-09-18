use super::*;

/// t29 — map-flap isolation (Fase 14, diagnosi t28): martella `map_physical`
/// di P1 su VA_X verificando marker, prima da solo (3000 iter) poi con un
/// helper che fa lo stesso su P2 (stessa VA, altre tabelle, altri frame).
/// Qualunque mismatch = cross-talk di mapping/TLB/aliasing. Non tocca il FS:
/// robusto a qualunque stato userfs. VA_X in zona libera, P1/P2 nello scratch
/// kernel riservato (MAP_TEST_PHYS).
pub fn t_mapflap() -> bool {
    helpers::drain_stray();
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
    let (h_chan, _) = match helpers::spawn_cfg("/fat/test/testcli.bin", "utcli", 16, helpers::M_MAPHAMMER, N as u64) {
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
    let h_ok = helpers::recv_expect(h_chan, helpers::T_DONE);
    match helpers::wait_exit(h_chan) {
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
pub fn t_neighbor() -> bool {
    helpers::drain_stray();
    let fb = libr::open_wait("/dev/null", 0, 1000, libr::POLL_PERIOD_TICKS);
    if fb < 0 {
        println!("[usertests] t30: baseline open /dev/null FAILED");
        return false;
    }
    let _ = libr::close(fb);
    // Helper "cattivo vicino" (nessun T_DONE atteso prima di T_STOP).
    let (fchan, _) = match helpers::spawn_cfg("/fat/test/testcli.bin", "utcli", 16, helpers::M_FLOOD, 0) {
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
                if m.tag == helpers::T_READY && m.channel == fchan {
                    let _ = libr::reply(helpers::T_ACK, 0, 0);
                    break m.w0 == 1;
                }
                if libr::is_exit_notify(&m) && m.channel == fchan {
                    break false;
                }
                if !libr::is_exit_notify(&m) {
                    let _ = libr::reply(helpers::T_ACK, 0, 0);
                }
            }
            Err(_) => break false,
        }
    };
    if !warmed {
        println!("[usertests] t30: flooder mai pronto (timeout)");
        helpers::stop_flooder(fchan);
        return false;
    }
    let p1 = match libr::init_bounce(libr::Service::Devfs) {
        Ok(p) => p,
        Err(_) => {
            println!("[usertests] t30: bounce devfs FAILED");
            helpers::stop_flooder(fchan);
            return false;
        }
    };
    // Sparizione + ricomparsa (poll throttled, come t27; il riuso PID puo'
    // ridare lo stesso numero: si osserva sparizione → ricomparsa).
    if !libr::poll_wait(1000, libr::POLL_PERIOD_TICKS, || {
        libr::service_pid(libr::Service::Devfs).is_err()
    }) {
        println!("[usertests] t30: devfs mai sparito (timeout)");
        helpers::stop_flooder(fchan);
        return false;
    }
    match libr::poll_value(1000, libr::POLL_PERIOD_TICKS, || {
        libr::service_pid(libr::Service::Devfs).ok()
    }) {
        Some(p2) => println!("[usertests] t30: devfs riavviato (pid {} -> {})", p1, p2),
        None => {
            println!("[usertests] t30: devfs mai riapparso (timeout)");
            helpers::stop_flooder(fchan);
            return false;
        }
    }
    // Misura: operativita' sotto flood.
    let t_start = libr::get_ticks();
    let fd = libr::open_wait("/dev/null", 0, 1000, libr::POLL_PERIOD_TICKS);
    let elapsed = libr::get_ticks() - t_start;
    helpers::stop_flooder(fchan);
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
    if n as usize >= helpers::HELLO.len() && hb[..helpers::HELLO.len()] != *helpers::HELLO {
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
    n as usize >= helpers::HELLO.len() && hb[..helpers::HELLO.len()] == *helpers::HELLO
}

/// t31 — presenza keyboard stack in userspace (Fase 15, gate leggero).
/// Verifica che i servizi Kbd/Tty siano registrati e i device apribili:
/// /dev/kbd (scancode raw da userkbd) e /dev/input/keyboard (byte cotti da
/// usertty, stesso path di prima). Niente digitazione reale (serve QMP).
pub fn t_kbd_presence() -> bool {
    helpers::drain_stray();
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

