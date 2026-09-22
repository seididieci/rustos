use super::*;

// ── Jobs (Fase 37.2) ────────────────────────────────────────────────
// La shell lancia programmi con fork+exec: il parent carica file+argv PRIMA
// del fork (il figlio post-fork ha l'FS avvelenato: `post_fork_child` —
// legge solo i byte COW-condivisi e chiama `exec_image_args`, mai il FS).
// Job = figlio diretto (non-detached: muore con la shell); l'uscita si
// osserva via EXIT_NOTIFY sul canale di nascita (nessun `wait` kernel in 37:
// la notifica unificata basta). Niente job control interattivo (foreground,
// segnali → posix-server futuro): un job foreground senza scampo blocca il
// prompt — i programmi longevi vanno lanciati con `&`.

struct Job {
    pid: i64,
    chan: u64,
    cmd: String,
    bg: bool,
    done: Option<i64>,
    /// Nonce dei grant redirect (Fase 40.4c/d, vuoto senza redirect):
    /// cancellati best-effort quando la morte e' osservata (mai prima: il
    /// claim dello startup avverrebbe dopo). Idempotenti (grant single-use:
    /// dopo il claim non esistono piu').
    redir_grants: Vec<u64>,
}

static mut JOBS: Vec<Job> = Vec::new();

fn jobs() -> &'static mut Vec<Job> {
    // Niente shared ref diretto allo static (hard error `static_mut_refs`,
    // vedi cwd.rs): raw pointer, single-threaded.
    unsafe { &mut *core::ptr::addr_of_mut!(JOBS) }
}

/// Attende la morte del job sul canale `chan` (bloccante): exit code o None
/// (recv fallita, mai in pratica). Le EXIT_NOTIFY altrui (es. servizi morti:
/// la shell ha canali verso Fs per i lookup) si scartano senza reply; altri
/// messaggi (nessuno dovrebbe scriverci) con reply difensiva.
fn wait_job(chan: u64) -> Option<i64> {
    loop {
        match libr::recv() {
            Ok(m) if m.channel == chan && libr::is_exit_notify(&m) => {
                return Some(m.w0 as i64);
            }
            Ok(m) if libr::is_exit_notify(&m) => {}
            Ok(_) => {
                let _ = libr::reply(0, 0, 0);
            }
            Err(_) => return None,
        }
    }
}

/// Drena senza bloccare: aggiorna i `done` dei job morti, senza stampare
/// (la stampa e' di `jobs`/`wait`). Notify altrui scartate senza reply.
/// A morte osservata cancella il grant redirect (40.4c: mai prima del claim).
fn poll_reap() {
    while let Some(m) = libr::recv_poll() {
        if libr::is_exit_notify(&m) {
            for j in jobs().iter_mut() {
                if j.chan == m.channel && j.done.is_none() {
                    j.done = Some(m.w0 as i64);
                    cancel_redir(&j.redir_grants);
                }
            }
        } else {
            let _ = libr::reply(0, 0, 0);
        }
    }
}

/// Cancella i grant redirect di un job morto (best-effort idempotenti).
fn cancel_redir(grants: &[u64]) {
    for &n in grants {
        let _ = libr::dup_cancel(n);
    }
}

fn print_job(idx: usize, j: &Job) {
    let mut s = String::from("[");
    cmd_info::push_u64(&mut s, idx as u64);
    s.push_str("] pid ");
    cmd_info::push_u64(&mut s, j.pid as u64);
    s.push(' ');
    match j.done {
        Some(c) => {
            s.push_str("done ");
            cmd_info::push_u64(&mut s, c as u64);
        }
        None => s.push_str("run"),
    }
    s.push(' ');
    s.push_str(&j.cmd);
    term::term_print(&s);
    term::term_print("\n");
}

/// `run <path> [args...] [&]`: lancia il programma (path relativo ammesso,
/// argv[0] = path come digitato). `&` finale = background (prompt subito,
/// `jobs`/`wait` dopo); senza = foreground (attende l'uscita; code != 0
/// stampato come `[exit N]`). Con redirect (Fase 40.4c/d): il parent apre
/// tutti i target in ordine + grant single-use per fd distinto e contrabbanda
/// la spec nell'ultimo argv (magic); lo startup del figlio fa claim +
/// `set_stdio` (tutti i programmi via `entry!`, zero codice per-target).
/// `2>&1` = alias (un grant solo, voce `Alias` nella spec).
pub(crate) fn cmd_run(args: &[&str], redirs: &[redirect::Redir]) {
    if args.len() < 2 {
        term::term_err("run: usage: run <path> [args...] [&]\n");
        return;
    }
    let bg = args.last() == Some(&"&");
    let end = if bg { args.len() - 1 } else { args.len() };
    if end < 2 {
        term::term_err("run: usage: run <path> [args...] [&]\n");
        return;
    }
    let path = cwd::resolve(args[1]);
    // Carica + serializza nel PARENT (il figlio non puo' piu' usare l'FS).
    let img = match libr::load_file(&path) {
        Some(b) if !b.is_empty() => b,
        _ => {
            term::term_err("run: cannot load ");
            term::term_err(args[1]);
            term::term_err("\n");
            return;
        }
    };
    let argv: Vec<&str> = args[1..end].to_vec();
    // Redirect: apri in ordine (ultimo vince per slot) + un grant per fd
    // distinto. Errori su stderr, comando non eseguito.
    let mut fds = [-1i64; 3];
    let mut spec: Vec<libr::RedirEntry> = Vec::new();
    let mut grants: Vec<u64> = Vec::new();
    if !redirs.is_empty() {
        fds = match redirect::open_all(redirs) {
            Ok(f) => f,
            Err((t, e)) => {
                redirect::report_open_error(&t, e);
                return;
            }
        };
        // Un grant per fd distinto, in ordine di slot; l'alias riusa l'fd
        // senza grant (lo startup copia dallo slot target).
        for slot in 0..3 {
            let fd = fds[slot];
            if fd < 0 {
                continue;
            }
            if slot == 2 && fd == fds[1] {
                spec.push(libr::RedirEntry::Alias { vfd: 2, target: 1 });
                continue;
            }
            match libr::dup_grant(fd) {
                Ok(n) => {
                    grants.push(n);
                    spec.push(libr::RedirEntry::Grant { vfd: slot as u8, nonce: n });
                }
                Err(_) => {
                    redirect::close_all(fds);
                    cancel_redir(&grants);
                    term::term_err("run: redirect grant failed\n");
                    return;
                }
            }
        }
    }
    let buf = match libr::serialize_argv_redir(&argv, &spec) {
        Some(b) => b,
        None => {
            redirect::close_all(fds);
            cancel_redir(&grants);
            term::term_err("run: argv troppo lunghi\n");
            return;
        }
    };
    let mut cmd = String::new();
    for (i, a) in args[1..end].iter().enumerate() {
        if i > 0 {
            cmd.push(' ');
        }
        cmd.push_str(a);
    }
    match libr::fork() {
        Err(_) => {
            // Fork fallita: nessun figlio, grant orfani da cancellare.
            redirect::close_all(fds);
            cancel_redir(&grants);
            term::term_err("run: fork failed\n");
        }
        Ok(libr::ForkResult::Child { .. }) => {
            // FS avvelenato qui: solo exec (byte COW-condivisi in lettura,
            // spec redirect gia' dentro `buf`). Fallimento = seriale diretta
            // (niente FS/terminale) + exit(1).
            match libr::exec_image_args(&img, &buf) {
                Ok(()) => libr::exit(1), // irraggiungibile
                Err(_) => {
                    let _ = libr::print_string(b"[shell] run: exec failed\n");
                    libr::exit(1);
                }
            }
        }
        Ok(libr::ForkResult::Parent { pid, chan }) => {
            // I grant vivono lato server (snapshot): le copie del parent si
            // chiudono subito; la cancellazione avviene a morte osservata
            // (wait_job/poll_reap: mai prima del claim dello startup).
            redirect::close_all(fds);
            jobs().push(Job {
                pid: pid as i64,
                chan,
                cmd,
                bg,
                done: None,
                redir_grants: grants,
            });
            if bg {
                let mut s = String::from("[bg pid ");
                cmd_info::push_u64(&mut s, pid);
                s.push(']');
                term::term_print(&s);
                term::term_print("\n");
                return;
            }
            let idx = jobs().len() - 1;
            match wait_job(chan) {
                Some(0) => {
                    cancel_redir(&jobs()[idx].redir_grants);
                    jobs().remove(idx);
                }
                Some(code) => {
                    cancel_redir(&jobs()[idx].redir_grants);
                    jobs().remove(idx);
                    let mut s = String::from("[exit ");
                    cmd_info::push_u64(&mut s, code as u64);
                    s.push(']');
                    term::term_print(&s);
                    term::term_print("\n");
                }
                None => {
                    term::term_err("run: wait failed\n");
                }
            }
        }
    }
}

/// `jobs`: tabella dei job (fresca: prima drena le morti senza bloccare).
/// I finiti restano finche' `wait` non li rimuove (stato `done` visibile).
pub(crate) fn cmd_jobs() {
    poll_reap();
    if jobs().is_empty() {
        term::term_print("no jobs\n");
        return;
    }
    for (i, j) in jobs().iter().enumerate() {
        print_job(i, j);
    }
}

/// `wait [pid]`: attende i job (tutti, o quello col pid) e li rimuove,
/// stampando `pid <P>: exit <C>` per ciascuno.
pub(crate) fn cmd_wait(args: &[&str]) {
    if args.len() >= 2 {
        let pid = match cmd_info::parse_i64(args[1]) {
            Some(p) => p,
            None => {
                term::term_err("wait: bad pid\n");
                return;
            }
        };
        let idx = match jobs().iter().position(|j| j.pid == pid) {
            Some(i) => i,
            None => {
                term::term_err("wait: no such job\n");
                return;
            }
        };
        // Se e' gia' done (visto da jobs), niente attesa: solo report+remove
        // (il grant e' gia' cancellato da poll_reap).
        poll_reap();
        if jobs()[idx].done.is_none() {
            let chan = jobs()[idx].chan;
            jobs()[idx].done = wait_job(chan);
            cancel_redir(&jobs()[idx].redir_grants);
        }
        let j = jobs().remove(idx);
        report_waited(&j);
        return;
    }
    // Tutti: in ordine di tabella (i done saltano l'attesa via poll).
    poll_reap();
    while !jobs().is_empty() {
        if jobs()[0].done.is_none() {
            let chan = jobs()[0].chan;
            jobs()[0].done = wait_job(chan);
            cancel_redir(&jobs()[0].redir_grants);
        }
        let j = jobs().remove(0);
        report_waited(&j);
    }
}

/// Stampa `pid <P>: exit <C>` (o `wait failed` se la wait non e' tornata).
fn report_waited(j: &Job) {
    let mut s = String::from("pid ");
    cmd_info::push_u64(&mut s, j.pid as u64);
    match j.done {
        Some(c) => {
            s.push_str(": exit ");
            cmd_info::push_u64(&mut s, c as u64);
        }
        None => s.push_str(": wait failed"),
    }
    term::term_print(&s);
    term::term_print("\n");
}
