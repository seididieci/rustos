use super::*;

pub fn t_ipc_echo() -> bool {
    helpers::drain_stray();
    let (chan, _) = match helpers::spawn_cfg("/fat/test/testcli.bin", "utcli", 16, helpers::M_ECHO, 8) {
        Some(x) => x,
        None => return false,
    };
    // 8 REQ + 1 DONE. Il server risponde a ogni REQ col payload raddoppiato:
    // il client verifica che la risposta sia proprio la SUA request *2.
    let mut got = 0usize;
    loop {
        match libr::recv() {
            Ok(m) => {
                if m.tag == helpers::T_REQ && m.channel == chan {
                    let _ = libr::reply(helpers::T_ACK, m.w0 * 2, 0);
                    got += 1;
                    if got == 8 {
                        break;
                    }
                } else {
                    // Estraneo: rispondi e scarta (residuo di test precedente).
                    let _ = libr::reply(helpers::T_ACK, 0, 0);
                }
            }
            Err(_) => return false,
        }
    }
    helpers::recv_done(&[chan]).0
}

pub fn t_ipc_multiclient() -> bool {
    helpers::drain_stray();
    let n_clients = 3usize;
    let rounds = 50usize;
    let mut chans = Vec::new();
    for _ in 0..n_clients {
        match helpers::spawn_cfg("/fat/test/testcli.bin", "utcli", 16, helpers::M_ECHO, rounds as u64) {
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
                helpers::T_REQ => {
                    let _ = libr::reply(helpers::T_ACK, m.w0 * 2, 0);
                    reqs += 1;
                }
                helpers::T_DONE => {
                    let _ = libr::reply(helpers::T_ACK, 0, 0);
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

pub fn t_devfs_concurrent_churn() -> bool {
    helpers::drain_stray();
    let mut chans = Vec::new();
    // Buffer FS per-processo (Fase 9.6): ogni client ha la propria pagina, non
    // serve serializzare le OPEN. L'handshake T_OPENED resta come barriera.
    for _ in 0..3 {
        match helpers::spawn_cfg("/fat/test/testcli.bin", "utcli", 16, helpers::M_ZERO, 30) {
            Some((c, _)) => {
                // Il client apre /dev/zero e notifica; reply per sbloccarlo.
                // `recv_expect` ignora gli estranei (es. EXIT_NOTIFY di helper
                // dei test precedenti, Fase 14) finche' non arriva l'OPENED.
                if !helpers::recv_expect(c, helpers::T_OPENED) {
                    return false;
                }
                chans.push(c);
            }
            None => return false,
        }
    }
    // Tutti hanno aperto: GO rilascia le read insieme.
    for &c in &chans {
        let _ = libr::send(c, helpers::T_GO, 0, 0);
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
        if !helpers::recv_done(&chans).0 {
            churn_ok = false;
        }
    }
    churn_ok
}

pub fn t_sched_preempt() -> bool {
    helpers::drain_stray();
    let mapped = libr::map_physical(libr::MAP_TEST_PHYS, helpers::SPIN_VA, 1).is_ok();
    let ctr = helpers::SPIN_VA as *mut u64;
    unsafe { core::ptr::write_volatile(ctr, 0) };

    let (chan, _) = match helpers::spawn_cfg("/fat/test/testspin.bin", "utspin", 16, 25, 1) {
        Some(x) => x,
        None => return false,
    };

    // Parent spinge in ring 3 SENZA mai bloccare: il figlio (Normal) può
    // avanzare solo se il timer lo preempta (RR tra Normal).
    libr::spin_ticks(110);
    let progress = unsafe { core::ptr::read_volatile(ctr) };

    let (done, _dchan) = helpers::recv_done(&[chan]);
    mapped && progress >= 5 && done
}

pub fn t_sched_priority() -> bool {
    // High (budget 20) vs Normal (budget 8): entrambi runnable → l'High deve
    // terminare per primo (una fascia Normal resta affamata finche' c'e' un
    // High). Niente Low qui: i server Normal idle (fs/shell) girano in recv-loop
    // sempre-Ready e affamerebbero una fascia Low.
    helpers::drain_stray();
    let (high, _) = match helpers::spawn_cfg("/fat/test/testspin.bin", "utspin", 31, 20, 0) {
        Some(x) => x,
        None => return false,
    };
    let (norm, _) = match helpers::spawn_cfg("/fat/test/testspin.bin", "utspin", 16, 8, 0) {
        Some(x) => x,
        None => return false,
    };
    let (ok1, first) = helpers::recv_done(&[high, norm]);
    let (ok2, _second) = helpers::recv_done(&[high, norm]);
    ok1 && ok2 && first == high
}

// ── CBS tests (Fase 11.5) ─────────────────────────────────────────────

/// Test admission control CBS: una richiesta oltre il cap (~70%) deve
/// essere rifiutata, una entro il cap deve essere accettata.
pub fn t_cbs_admission() -> bool {
    // 80% bandwidth → supera il cap → deve fallire.
    if libr::cbs_create(8, 10).is_ok() {
        println!("[usertests] t_cbs_admission: 80% should have been rejected");
        return false;
    }
    // 5% → dentro il cap → deve riuscire.
    let s1 = match libr::cbs_create(1, 20) {
        Ok(id) => id,
        Err(_) => {
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
pub fn t_cbs_bandwidth() -> bool {
    helpers::drain_stray();
    // Precondizione: il CBS e' sempre attivo (scheduler RT unico). Se la
    // creazione di un server fallisce il test e' FAILED.
    if libr::cbs_create(1, 10).is_err() {
        println!("[usertests] t_cbs_bandwidth: cbs_create FAILED");
        return false;
    }

    // Audio: utcbstest crea server Q=3 P=10 e si attacha; busy-loop di 200
    // tick wall-clock contando i tick osservati (~60 attesi a 30%).
    let (audio_chan, _) = match helpers::spawn_cfg("/fat/test/cbstest.bin", "utcbs", 16, 3, 10) {
        Some(x) => x,
        None => {
            println!("[usertests] t_cbs_bandwidth: spawn utcbstest FAILED");
            return false;
        }
    };

    // Hog: utspin_norm senza CBS, budget 300 tick wall-clock: resta attivo
    // per l'intera finestra dell'audio (200) e contende la CPU.
    let (hog_chan, _) = match helpers::spawn_cfg("/fat/test/testspin.bin", "utspin", 16, 300, 0) {
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
                let _ = libr::reply(helpers::T_ACK, 0, 0);
                if m.tag == helpers::T_DONE && m.channel == audio_chan && audio_obs < 0 {
                    audio_obs = m.w1 as i64;
                    received += 1;
                } else if m.tag == helpers::T_DONE && m.channel == hog_chan && hog_obs < 0 {
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

