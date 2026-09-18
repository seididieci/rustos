use super::*;

/// Fase 13 — t20: FS async 1-in-volo. Apre hello.txt (sync), lancia una
/// `read_async` (non bloccante), fa lavoro utile, poi `fs_collect`. Verifica
/// che i dati letti in modo async combacino con il contenuto atteso.
pub fn t_fs_async() -> bool {
    helpers::drain_stray();
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
    if !(n as usize >= helpers::HELLO.len() && buf[..helpers::HELLO.len()] == *helpers::HELLO) {
        println!("[usertests] t_fs_async: collect n={} (atteso >= {})", n, helpers::HELLO.len());
        return false;
    }
    // ADR-0019 Passo 3: stessa lettura via wrapper async `FsRead` (stesso
    // file, fd riaperto perche' la prima lettura ha avanzato la posizione).
    // Deve coincidere byte per byte con la collect manuale sopra.
    let fd2 = libr::open("hello.txt", 0);
    if fd2 < 0 {
        println!("[usertests] t_fs_async: reopen hello.txt FAILED");
        return false;
    }
    let mut buf2 = [0u8; 128];
    let f = match libr::task::FsRead::new(fd2, &mut buf2, 128) {
        Ok(f) => f,
        Err(_) => {
            println!("[usertests] t_fs_async: FsRead::new FAILED");
            let _ = libr::close(fd2);
            return false;
        }
    };
    let n2 = libr::task::block_on(f);
    let _ = libr::close(fd2);
    if n2 == n && buf2[..helpers::HELLO.len()] == buf[..helpers::HELLO.len()] {
        true
    } else {
        println!("[usertests] t_fs_async: wrapper n2={} n={} (atteso uguali)", n2, n);
        false
    }
}

/// Fase 13 — t21: IPC async su IPC puro (canale di nascita verso un helper
/// server echo MODE_SRV). Due sotto-casi:
///   1) K richieste `send_async` in volo sullo stesso canale, raccolte FIFO con
///      `wait_reply` in ordine → ogni reply vale 2*payload.
///   2) Backpressure: spamma `send_async` finche' la coda del server (cap 8) si
///      riempie → -1 osservato; poi drena le reply in volo e chiude con T_STOP.
pub fn t_ipc_async() -> bool {
    helpers::drain_stray();
    // Helper server echo (modalita' 3): risponde a ogni T_REQ con 2*w0.
    // (srv_pid serve a distinguere la morte DEL server dalle EXIT_NOTIFY
    // tardive di helper precedenti: il parent le riceve tutte.)
    let (chan, srv_pid) = match helpers::spawn_cfg("/fat/test/testcli.bin", "utcli", 16, 3, 0) {
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
        match libr::send_async(chan, helpers::T_REQ, payload, 0) {
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
        let _ = libr::send(chan, helpers::T_STOP, 0, 0);
        let _ = helpers::recv_expect(chan, helpers::T_DONE);
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
        match libr::send_async(chan, helpers::T_REQ, payload, 0) {
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
        let _ = libr::send(chan, helpers::T_STOP, 0, 0);
        let _ = helpers::recv_expect(chan, helpers::T_DONE);
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
        let _ = libr::send(chan, helpers::T_STOP, 0, 0);
        let _ = helpers::recv_expect(chan, helpers::T_DONE);
        return false;
    }

    // Chiude il server e attende il suo T_DONE.
    if libr::send(chan, helpers::T_STOP, 0, 0).is_err() {
        return false;
    }
    helpers::recv_expect(chan, helpers::T_DONE)
}

/// ADR-0019 Passo 2, t41 — `block_on` + echo async verso helper MODE_SRV.
/// Stesso scenario di t21-sottocaso-1 ma con `WaitReply` + router invece di
/// `wait_reply`: 1 send_async, raccolta con `block_on`, teardown identico
/// (T_STOP + T_DONE). Filtro canale (`on_chan`): le EXIT_NOTIFY stale di
/// helper precedenti (altri canali) sono scartate dal router come
/// `wait_reply_chan` — mai Died spurio; la morte del NOSTRO server e' FAIL.
pub fn t_task_block_on() -> bool {
    helpers::drain_stray();
    let (chan, _srv_pid) = match helpers::spawn_cfg("/fat/test/testcli.bin", "utcli", 16, 3, 0) {
        Some(x) => x,
        None => {
            println!("[usertests] t41: spawn MODE_SRV FAILED");
            return false;
        }
    };
    let payload = 4242u64;
    let req = match libr::send_async(chan, helpers::T_REQ, payload, 0) {
        Ok(r) => r,
        Err(_) => {
            println!("[usertests] t41: send_async FAILED");
            let _ = libr::send(chan, helpers::T_STOP, 0, 0);
            let _ = helpers::recv_expect(chan, helpers::T_DONE);
            return false;
        }
    };
    let ok = match libr::task::block_on(libr::task::WaitReply::on_chan(req, chan)) {
        Ok(m) => m.req_id == req && m.w0 == 2 * payload,
        Err(libr::WaitReplyError::ServerDied { pid, code }) => {
            println!("[usertests] t41: echo server died (pid={}, code={})", pid, code);
            false
        }
        Err(_) => false,
    };
    if !ok {
        println!("[usertests] t41: reply MISMATCH");
    }
    // Teardown come t21 (anche a FAIL: niente helper appeso).
    if libr::send(chan, helpers::T_STOP, 0, 0).is_err() {
        return false;
    }
    ok && helpers::recv_expect(chan, helpers::T_DONE)
}

/// ADR-0019 Passo 2, t42 — `run` con 2 task concorrenti + morte server.
/// Parte A (routing): DUE helper MODE_SRV, una send_async ciascuno (alla B
/// prima, per mescolare l'ordine di arrivo), raccolta con `run([..])`: ogni
/// risultato deve matchare il PROPRIO req (non FIFO) — la prova che il router
/// correla per req_id, cosa che `wait_reply` non puo' fare.
/// Parte B (morte): helper SRVDIE (non risponde mai, come t24) + kill →
/// `block_on` deve tornare `ServerDied{pid}` esatto.
pub fn t_task_run() -> bool {
    helpers::drain_stray();
    // ── Parte A: due server, due attese, un run.
    let (chan_a, _) = match helpers::spawn_cfg("/fat/test/testcli.bin", "utcli", 16, 3, 0) {
        Some(x) => x,
        None => {
            println!("[usertests] t42: spawn MODE_SRV(A) FAILED");
            return false;
        }
    };
    let (chan_b, _) = match helpers::spawn_cfg("/fat/test/testcli.bin", "utcli", 16, 3, 0) {
        Some(x) => x,
        None => {
            println!("[usertests] t42: spawn MODE_SRV(B) FAILED");
            let _ = libr::send(chan_a, helpers::T_STOP, 0, 0);
            let _ = helpers::recv_expect(chan_a, helpers::T_DONE);
            return false;
        }
    };
    let (pa, pb) = (7101u64, 7202u64);
    let req_b = match libr::send_async(chan_b, helpers::T_REQ, pb, 0) {
        Ok(r) => r,
        Err(_) => {
            println!("[usertests] t42: send_async(B) FAILED");
            return false;
        }
    };
    let req_a = match libr::send_async(chan_a, helpers::T_REQ, pa, 0) {
        Ok(r) => r,
        Err(_) => {
            println!("[usertests] t42: send_async(A) FAILED");
            return false;
        }
    };
    let [ra, rb] = libr::task::run([
        libr::task::WaitReply::on_chan(req_a, chan_a),
        libr::task::WaitReply::on_chan(req_b, chan_b),
    ]);
    let ok_a = matches!(ra, Ok(m) if m.req_id == req_a && m.w0 == 2 * pa);
    let ok_b = matches!(rb, Ok(m) if m.req_id == req_b && m.w0 == 2 * pb);
    // Teardown A (anche a FAIL): T_STOP + T_DONE per entrambi, come t21.
    let stop_a = libr::send(chan_a, helpers::T_STOP, 0, 0).is_ok() && helpers::recv_expect(chan_a, helpers::T_DONE);
    let stop_b = libr::send(chan_b, helpers::T_STOP, 0, 0).is_ok() && helpers::recv_expect(chan_b, helpers::T_DONE);
    if !(ok_a && ok_b && stop_a && stop_b) {
        println!("[usertests] t42: routing MISMATCH (a={} b={})", ok_a, ok_b);
        return false;
    }
    // ── Parte B: morte durante l'attesa (pattern t24, via router).
    let (chan_c, pid_c) = match helpers::spawn_cfg("/fat/test/testcli.bin", "utcli", 16, helpers::M_SRVDIE, 0) {
        Some(x) => x,
        None => {
            println!("[usertests] t42: spawn SRVDIE FAILED");
            return false;
        }
    };
    let req_c = match libr::send_async(chan_c, helpers::T_REQ, 0, 0) {
        Ok(r) => r,
        Err(_) => {
            println!("[usertests] t42: send_async(C) FAILED");
            return false;
        }
    };
    let code = -9i64;
    if libr::kill(pid_c as i64, code).is_err() {
        println!("[usertests] t42: kill(pid={}) FAILED", pid_c);
        return false;
    }
    // Filtro canale: le EXIT_NOTIFY di A/B (usciti sopra, altri canali) sono
    // stale e il router le scarta; solo la morte di C arriva qui.
    match libr::task::block_on(libr::task::WaitReply::on_chan(req_c, chan_c)) {
        Err(libr::WaitReplyError::ServerDied { pid, code: c }) if pid == pid_c && c == code => true,
        Err(libr::WaitReplyError::ServerDied { pid, code: c }) => {
            println!("[usertests] t42: ServerDied errato (pid={}, code={})", pid, c);
            false
        }
        other => {
            println!("[usertests] t42: atteso ServerDied, ottenuto {:?}", other);
            false
        }
    }
}

/// ADR-0019 Passo 4, t43 — composizione annidata `Join<Join<W,W>,W>` su TRE
/// helper MODE_SRV, guidata da `block_on`. Gli invii sono in ordine INVERSO
/// all'albero dei task (C, B, A) per mescolare l'arrivo: ogni risultato deve
/// comunque matchare il PROPRIO req — prova di routing multi-livello (il
/// router esterno vede i waiter foglia attraverso i `Join`, mai i messaggi
/// altrui). Teardown T_STOP+T_DONE per tutti e tre.
pub fn t_task_join_nested() -> bool {
    helpers::drain_stray();
    let (chan_a, _) = match helpers::spawn_cfg("/fat/test/testcli.bin", "utcli", 16, 3, 0) {
        Some(x) => x,
        None => {
            println!("[usertests] t43: spawn MODE_SRV(A) FAILED");
            return false;
        }
    };
    let (chan_b, _) = match helpers::spawn_cfg("/fat/test/testcli.bin", "utcli", 16, 3, 0) {
        Some(x) => x,
        None => {
            println!("[usertests] t43: spawn MODE_SRV(B) FAILED");
            return false;
        }
    };
    let (chan_c, _) = match helpers::spawn_cfg("/fat/test/testcli.bin", "utcli", 16, 3, 0) {
        Some(x) => x,
        None => {
            println!("[usertests] t43: spawn MODE_SRV(C) FAILED");
            return false;
        }
    };
    let (pa, pb, pc) = (8101u64, 8202u64, 8303u64);
    let mut reqs = [0i64; 3];
    let chans = [chan_a, chan_b, chan_c];
    let pays = [pa, pb, pc];
    // Invii in ordine inverso (C, B, A): l'arrivo non segue l'albero.
    for &ci in &[2usize, 1, 0] {
        match libr::send_async(chans[ci], helpers::T_REQ, pays[ci], 0) {
            Ok(r) => reqs[ci] = r,
            Err(_) => {
                println!("[usertests] t43: send_async FAILED");
                return false;
            }
        }
    }
    let nested = libr::task::join(
        libr::task::join(
            libr::task::WaitReply::on_chan(reqs[0], chan_a),
            libr::task::WaitReply::on_chan(reqs[1], chan_b),
        ),
        libr::task::WaitReply::on_chan(reqs[2], chan_c),
    );
    let ((ra, rb), rc) = libr::task::block_on(nested);
    let ok = matches!(ra, Ok(m) if m.req_id == reqs[0] && m.w0 == 2 * pa)
        && matches!(rb, Ok(m) if m.req_id == reqs[1] && m.w0 == 2 * pb)
        && matches!(rc, Ok(m) if m.req_id == reqs[2] && m.w0 == 2 * pc);
    // Teardown (anche a FAIL): T_STOP + T_DONE per tutti, come t21.
    let mut stop_ok = true;
    for &ch in &chans {
        stop_ok &= libr::send(ch, helpers::T_STOP, 0, 0).is_ok() && helpers::recv_expect(ch, helpers::T_DONE);
    }
    if !ok {
        println!("[usertests] t43: nested routing MISMATCH");
    }
    ok && stop_ok
}

