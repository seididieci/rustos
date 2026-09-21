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
fn poll_reap() {
    while let Some(m) = libr::recv_poll() {
        if libr::is_exit_notify(&m) {
            for j in jobs().iter_mut() {
                if j.chan == m.channel && j.done.is_none() {
                    j.done = Some(m.w0 as i64);
                }
            }
        } else {
            let _ = libr::reply(0, 0, 0);
        }
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
/// stampato come `[exit N]`).
pub(crate) fn cmd_run(args: &[&str]) {
    if args.len() < 2 {
        term::term_print("run: usage: run <path> [args...] [&]\n");
        return;
    }
    let bg = args.last() == Some(&"&");
    let end = if bg { args.len() - 1 } else { args.len() };
    if end < 2 {
        term::term_print("run: usage: run <path> [args...] [&]\n");
        return;
    }
    let path = cwd::resolve(args[1]);
    // Carica + serializza nel PARENT (il figlio non puo' piu' usare l'FS).
    let img = match libr::load_file(&path) {
        Some(b) if !b.is_empty() => b,
        _ => {
            term::term_print("run: cannot load ");
            term::term_print(args[1]);
            term::term_print("\n");
            return;
        }
    };
    let argv: Vec<&str> = args[1..end].to_vec();
    let buf = match libr::serialize_argv(&argv) {
        Some(b) => b,
        None => {
            term::term_print("run: argv troppo lunghi\n");
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
            term::term_print("run: fork failed\n");
        }
        Ok(libr::ForkResult::Child { .. }) => {
            // FS avvelenato qui: solo exec (byte COW-condivisi in lettura).
            // Fallimento = seriale diretta (niente FS/terminale) + exit(1).
            match libr::exec_image_args(&img, &buf) {
                Ok(()) => libr::exit(1), // irraggiungibile
                Err(()) => {
                    let _ = libr::print_string(b"[shell] run: exec failed\n");
                    libr::exit(1);
                }
            }
        }
        Ok(libr::ForkResult::Parent { pid, chan }) => {
            jobs().push(Job {
                pid: pid as i64,
                chan,
                cmd,
                bg,
                done: None,
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
                    jobs().remove(idx);
                }
                Some(code) => {
                    jobs().remove(idx);
                    let mut s = String::from("[exit ");
                    cmd_info::push_u64(&mut s, code as u64);
                    s.push(']');
                    term::term_print(&s);
                    term::term_print("\n");
                }
                None => {
                    term::term_print("run: wait failed\n");
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
                term::term_print("wait: bad pid\n");
                return;
            }
        };
        let idx = match jobs().iter().position(|j| j.pid == pid) {
            Some(i) => i,
            None => {
                term::term_print("wait: no such job\n");
                return;
            }
        };
        // Se e' gia' done (visto da jobs), niente attesa: solo report+remove.
        poll_reap();
        if jobs()[idx].done.is_none() {
            let chan = jobs()[idx].chan;
            jobs()[idx].done = wait_job(chan);
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
