use super::*;

// ── Job control tests (Fase 44a) ─────────────────────────────────────
// Meccanismo kernel neutro (`SYS_SUSPEND`/`SYS_RESUME`, ADR-0025): la
// semantica POSIX (SIGTSTP/SIGCONT, fg/bg) vive nella shell.

/// Attende al piu' `bound` tick un messaggio con `req_id == req` (risposta
/// async attesa): `Some(msg)` se arriva, `None` a timeout. Disciplina come
/// `recv_done` (EXIT_NOTIFY saltate senza reply, altri estranei con reply
/// difensiva) + throttle come `poll_gone` (mai busy su syscall).
fn wait_req(req: i64, bound: i64) -> Option<libr::IpcMsg> {
    let t0 = libr::get_ticks();
    loop {
        while let Some(m) = libr::recv_poll() {
            if m.req_id == req {
                return Some(m);
            }
            if !libr::is_exit_notify(&m) {
                let _ = libr::reply(helpers::T_ACK, 0, 0);
            }
        }
        if libr::get_ticks() - t0 > bound {
            return None;
        }
        for _ in 0..512 {
            core::hint::spin_loop();
        }
    }
}

/// t55 — suspend/resume (Fase 44a, job control). Verifica:
///   1. gate: suspend di se' e di pid morto = Err; suspend idempotente ok;
///      resume di running = ok (no-op);
///   2. figlio RUNNING (testspin): suspend → `ps` Stopped + TIME congelato
///      su ~40 tick; resume → completa (T_DONE + exit 0);
///   3. figlio BLOCCATO in recv (SRV): suspend → Stopped; `send_async`
///      accodata SENZA svegliare (nessuna reply in ~30 tick); resume →
///      la reply arriva (w0 == 2*payload); stop pulito (T_STOP/T_DONE/exit);
///   4. hardening: helper non-parent che sospende/riprende un fratello =
///      entrambi rifiutati, vittima viva.
pub fn t_suspend_resume() -> bool {
    helpers::drain_stray();
    // 0. Gate: se stessi rifiutati (coerenza con kill, che rifiuta se' —
    // per se' usare `exit`; non esiste un "self-stop" sensato).
    if libr::suspend(libr::getpid()).is_ok() || libr::resume(libr::getpid()).is_ok() {
        println!("[usertests] t55: suspend/resume di se' NON rifiutati!");
        return false;
    }
    // Pid morto: spawna, killa, attende, poi suspend/resume = Err.
    let (d_chan, d_pid) = match helpers::spawn_cfg(
        "/fat/test/testcli.bin", "utcli", 16, helpers::M_KILLME, 0,
    ) {
        Some(x) => x,
        None => {
            println!("[usertests] t55: spawn morto-probe FAILED");
            return false;
        }
    };
    if libr::kill(d_pid as i64, 0).is_err() {
        println!("[usertests] t55: kill morto-probe FAILED");
        return false;
    }
    if helpers::wait_exit(d_chan).is_none() {
        println!("[usertests] t55: wait morto-probe FAILED");
        return false;
    }
    if libr::suspend(d_pid as i64).is_ok() || libr::resume(d_pid as i64).is_ok() {
        println!("[usertests] t55: suspend/resume di morto NON rifiutati!");
        return false;
    }

    // 1. Figlio RUNNING: testspin con budget lungo.
    let (s_chan, s_pid) = match helpers::spawn_cfg(
        "/fat/test/testspin.bin", "utspin", 16, 200, 0,
    ) {
        Some(x) => x,
        None => {
            println!("[usertests] t55: spawn spin FAILED");
            return false;
        }
    };
    // Lascialo girare un po', poi sospendi (idempotente x2).
    libr::spin_ticks(30);
    if libr::suspend(s_pid as i64).is_err() {
        println!("[usertests] t55: suspend spin FAILED");
        let _ = libr::kill(s_pid as i64, 0);
        let _ = helpers::wait_exit(s_chan);
        return false;
    }
    if libr::suspend(s_pid as i64).is_err() {
        println!("[usertests] t55: doppio suspend FAILED (non idempotente?)");
        let _ = libr::kill(s_pid as i64, 0);
        let _ = helpers::wait_exit(s_chan);
        return false;
    }
    // ps: Stopped + TIME t0; dopo ~40 tick TIME identico (congelato).
    let t0 = match libr::ps_info(s_pid as u32) {
        Some(e) if e.stopped() => e.ticks,
        Some(_) => {
            println!("[usertests] t55: spin non Stopped in ps!");
            let _ = libr::kill(s_pid as i64, 0);
            let _ = helpers::wait_exit(s_chan);
            return false;
        }
        None => {
            println!("[usertests] t55: spin sparito da ps!");
            return false;
        }
    };
    libr::spin_ticks(40);
    match libr::ps_info(s_pid as u32) {
        Some(e) if e.stopped() && e.ticks == t0 => {}
        Some(e) => {
            println!(
                "[usertests] t55: TIME non congelato ({} -> {}) o non Stopped",
                t0, e.ticks
            );
            let _ = libr::kill(s_pid as i64, 0);
            let _ = helpers::wait_exit(s_chan);
            return false;
        }
        None => {
            println!("[usertests] t55: spin sparito da ps durante stop!");
            return false;
        }
    }
    // Resume: completa (T_DONE via send sync + exit 0).
    if libr::resume(s_pid as i64).is_err() {
        println!("[usertests] t55: resume spin FAILED");
        let _ = libr::kill(s_pid as i64, 0);
        let _ = helpers::wait_exit(s_chan);
        return false;
    }
    let (ok, _) = helpers::recv_done(&[s_chan]);
    if !ok {
        println!("[usertests] t55: T_DONE spin mancante post-resume");
        let _ = libr::kill(s_pid as i64, 0);
        let _ = helpers::wait_exit(s_chan);
        return false;
    }
    match helpers::wait_exit(s_chan) {
        Some((0, _)) => {}
        _ => {
            println!("[usertests] t55: exit spin post-resume non 0");
            return false;
        }
    }

    // 2. Figlio BLOCCATO in recv (SRV): suspend → coda senza sveglia.
    let (v_chan, v_pid) = match helpers::spawn_cfg(
        "/fat/test/testcli.bin", "utcli", 16, helpers::M_SRV, 0,
    ) {
        Some(x) => x,
        None => {
            println!("[usertests] t55: spawn srv FAILED");
            return false;
        }
    };
    if libr::suspend(v_pid as i64).is_err() {
        println!("[usertests] t55: suspend srv FAILED");
        let _ = libr::kill(v_pid as i64, 0);
        let _ = helpers::wait_exit(v_chan);
        return false;
    }
    match libr::ps_info(v_pid as u32) {
        Some(e) if e.stopped() => {}
        _ => {
            println!("[usertests] t55: srv non Stopped in ps!");
            let _ = libr::kill(v_pid as i64, 0);
            let _ = helpers::wait_exit(v_chan);
            return false;
        }
    }
    // Richiesta async mentre e' sospeso: accodata, NESSUNA reply in ~30 tick
    // (prova che il wake non lo sveglia).
    let req = match libr::send_async(v_chan, helpers::T_REQ, 0xBEEF, 0) {
        Ok(r) => r,
        Err(_) => {
            println!("[usertests] t55: send_async a sospeso FAILED");
            let _ = libr::kill(v_pid as i64, 0);
            let _ = helpers::wait_exit(v_chan);
            return false;
        }
    };
    if wait_req(req, 30).is_some() {
        println!("[usertests] t55: reply arrivata DA SOSPESO (wake non rispettato?)");
        let _ = libr::kill(v_pid as i64, 0);
        let _ = helpers::wait_exit(v_chan);
        return false;
    }
    // Ancora Stopped dopo la richiesta?
    match libr::ps_info(v_pid as u32) {
        Some(e) if e.stopped() => {}
        _ => {
            println!("[usertests] t55: srv svegliato dalla send_async!");
            let _ = libr::kill(v_pid as i64, 0);
            let _ = helpers::wait_exit(v_chan);
            return false;
        }
    }
    // Resume: la reply arriva (w0 == 2*payload, come run_srv).
    if libr::resume(v_pid as i64).is_err() {
        println!("[usertests] t55: resume srv FAILED");
        let _ = libr::kill(v_pid as i64, 0);
        let _ = helpers::wait_exit(v_chan);
        return false;
    }
    match wait_req(req, 200) {
        Some(m) if m.w0 == 2 * 0xBEEF => {}
        Some(m) => {
            println!("[usertests] t55: reply post-resume errata (w0={})", m.w0);
            let _ = libr::kill(v_pid as i64, 0);
            let _ = helpers::wait_exit(v_chan);
            return false;
        }
        None => {
            println!("[usertests] t55: reply post-resume MAI arrivata");
            let _ = libr::kill(v_pid as i64, 0);
            let _ = helpers::wait_exit(v_chan);
            return false;
        }
    }
    // Stop pulito: T_STOP sync, T_DONE, exit 0.
    if libr::send(v_chan, helpers::T_STOP, 0, 0).is_err() {
        println!("[usertests] t55: T_STOP srv FAILED");
        let _ = libr::kill(v_pid as i64, 0);
        let _ = helpers::wait_exit(v_chan);
        return false;
    }
    let (ok, _) = helpers::recv_done(&[v_chan]);
    if !ok {
        println!("[usertests] t55: T_DONE srv mancante");
        let _ = libr::kill(v_pid as i64, 0);
        let _ = helpers::wait_exit(v_chan);
        return false;
    }
    match helpers::wait_exit(v_chan) {
        Some((0, _)) => {}
        _ => {
            println!("[usertests] t55: exit srv non 0");
            return false;
        }
    }

    // 3. Hardening: helper non-parent che sospende/riprende un fratello.
    let (b_chan, b_pid) = match helpers::spawn_cfg(
        "/fat/test/testcli.bin", "utcli", 16, helpers::M_KILLME, 0,
    ) {
        Some(x) => x,
        None => {
            println!("[usertests] t55: spawn vittima FAILED");
            return false;
        }
    };
    let (h_chan, _) = match helpers::spawn_cfg(
        "/fat/test/testcli.bin", "utcli", 16, helpers::M_SUSPENDENY, b_pid,
    ) {
        Some(x) => x,
        None => {
            println!("[usertests] t55: spawn probe FAILED");
            let _ = libr::kill(b_pid as i64, 0);
            let _ = helpers::wait_exit(b_chan);
            return false;
        }
    };
    let (ok, detail) = helpers::recv_done(&[h_chan]);
    if !ok {
        println!("[usertests] t55: probe suspend ostile NON rifiutato (detail={})", detail);
        let _ = libr::kill(b_pid as i64, 0);
        let _ = helpers::wait_exit(b_chan);
        let _ = helpers::wait_exit(h_chan);
        return false;
    }
    // La vittima deve essere ancora viva E non sospesa.
    match libr::ps_info(b_pid as u32) {
        Some(e) if !e.stopped() => {}
        Some(_) => {
            println!("[usertests] t55: vittima sospesa da non-parent!");
            let _ = libr::kill(b_pid as i64, 0);
            let _ = helpers::wait_exit(b_chan);
            let _ = helpers::wait_exit(h_chan);
            return false;
        }
        None => {
            println!("[usertests] t55: vittima sparita!");
            let _ = helpers::wait_exit(h_chan);
            return false;
        }
    }
    // Resume no-op su running (nostro figlio, consentito ma inerte).
    if libr::resume(b_pid as i64).is_err() {
        println!("[usertests] t55: resume no-op su running FAILED");
        let _ = libr::kill(b_pid as i64, 0);
        let _ = helpers::wait_exit(b_chan);
        let _ = helpers::wait_exit(h_chan);
        return false;
    }
    // Cleanup: entrambi nostri figli (kill consentito).
    let _ = libr::kill(b_pid as i64, 0);
    let _ = helpers::wait_exit(b_chan);
    let _ = helpers::wait_exit(h_chan);
    true
}
