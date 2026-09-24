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

/// Costruisce la spec grant/alias dagli fd dello stadio (Fase 40.4c/d, riusato
/// dalle pipeline in Fase 42): un grant per fd distinto in ordine di slot,
/// `Alias` per `2>&1` (slot 2 == slot 1, zero grant). A fallimento cancella i
/// grant parziali e ritorna Err (il chiamante chiude gli fd).
fn grant_fds(fds: [i64; 3]) -> Result<(Vec<libr::RedirEntry>, Vec<u64>), ()> {
    let mut spec: Vec<libr::RedirEntry> = Vec::new();
    let mut grants: Vec<u64> = Vec::new();
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
                cancel_redir(&grants);
                return Err(());
            }
        }
    }
    Ok((spec, grants))
}

/// Figlio spawnato (run o builtin): handle per l'attesa di gruppo.
struct Spawned {
    pid: u64,
    chan: u64,
    grants: Vec<u64>,
}

/// Fork+exec per uno stadio `run` (Fase 42: condivide il core con `cmd_run`).
/// `exec_argv[0]` = path (come digitato), `img` precaricato dal parent,
/// `buf` = argv+spec serializzati, `fds` = copie parent (chiuse qui subito:
/// i grant vivono snapshot server-side). Il figlio ha l'FS avvelenato e fa
/// solo exec. A fork fallita chiude tutto e cancella i grant.
fn spawn_run_exec(
    exec_argv: &[&str],
    img: &[u8],
    buf: &[u8],
    fds: [i64; 3],
    grants: Vec<u64>,
) -> Result<Spawned, ()> {
    match libr::fork() {
        Err(_) => {
            redirect::close_all(fds);
            cancel_redir(&grants);
            Err(())
        }
        Ok(libr::ForkResult::Child { .. }) => {
            // FS avvelenato qui: solo exec (byte COW-condivisi in lettura,
            // spec redirect gia' dentro `buf`). Fallimento = seriale diretta
            // (niente FS/terminale) + exit(1).
            match libr::exec_image_args(img, buf) {
                Ok(()) => libr::exit(1), // irraggiungibile
                Err(_) => {
                    let _ = libr::print_string(b"[shell] run: exec failed\n");
                    libr::exit(1);
                }
            }
        }
        Ok(libr::ForkResult::Parent { pid, chan }) => {
            redirect::close_all(fds);
            Ok(Spawned { pid, chan, grants })
        }
    }
}

/// Attende TUTTI i canali (Fase 42, waitpid di gruppo): EXIT_NOTIFY altrui
/// (servizi morti: la shell ha canali verso Fs) scartate senza reply come in
/// `wait_job`; altri messaggi con reply difensiva. Ritorna i codici in ordine
/// di stadio (None = wait fallita, mai in pratica).
fn wait_all(chans: &[u64]) -> Vec<Option<i64>> {
    let mut done: Vec<Option<i64>> = Vec::new();
    for _ in chans {
        done.push(None);
    }
    let mut remaining = chans.len();
    while remaining > 0 {
        match libr::recv() {
            Ok(m) if libr::is_exit_notify(&m) => {
                for (i, c) in chans.iter().enumerate() {
                    if *c == m.channel && done[i].is_none() {
                        done[i] = Some(m.w0 as i64);
                        remaining -= 1;
                    }
                }
            }
            Ok(_) => {
                let _ = libr::reply(0, 0, 0);
            }
            Err(_) => return done,
        }
    }
    done
}
/// PATH di default (Fase 43a): i binari da disco stanno in `/fat/bin`.
const DEFAULT_PATH: &str = "/fat/bin";

/// Risolve un nome programma (43a): con `/` e' path diretto (`cwd::resolve`),
/// senza e' cercato nelle dir di `$PATH` (`:`-separate, default `/fat/bin`).
/// Per dir si prova il nome esatto e poi con suffisso `.bin` (gli eseguibili
/// su FAT 8.3 sono `.bin`: `runhello` trova `/fat/bin/runhello.bin`).
/// La sonda e' via `stat` (1 round trip, niente dati); il load resta al
/// chiamante (TOCTOU → `cannot load` loud). `None` = non trovato.
pub(crate) fn resolve_prog(name: &str) -> Option<String> {
    if name.contains('/') {
        return Some(cwd::resolve(name));
    }
    let path = parser::vars_get("PATH").unwrap_or_else(|| String::from(DEFAULT_PATH));
    for dir in path.split(':') {
        if dir.is_empty() {
            continue;
        }
        let mut cand = String::from(dir);
        if !cand.ends_with('/') {
            cand.push('/');
        }
        cand.push_str(name);
        if is_reg_file(&cand) {
            return Some(cand);
        }
        let mut with_ext = cand;
        with_ext.push_str(".bin");
        if is_reg_file(&with_ext) {
            return Some(with_ext);
        }
    }
    None
}

/// Sonda esistenza file regolare via `stat` (niente dati, niente effetti).
fn is_reg_file(path: &str) -> bool {
    let mut st = libr::Stat { size: 0, kind: 0, readonly: false };
    match libr::stat(path, &mut st) {
        Ok(()) => st.is_file(),
        Err(_) => false,
    }
}

/// Environment del figlio (43a): prefissi del comando (ultimo vince, bash),
/// poi VARS persistenti, poi `PWD=cwd` se assente. `Env::get` vede la prima
/// occorrenza: l'ordine e' la priorita'. Tutte le VARS passano (niente flag
/// export: eredita' totale deterministica, serve al self-hosting).
pub(crate) fn child_env(extra: &[(String, String)]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    // Ultimo vince tra duplicati: scansione inversa, primo inserimento.
    for (k, v) in extra.iter().rev() {
        if !seen.iter().any(|s| s.as_str() == k.as_str()) {
            seen.push(k.clone());
            out.push((k.clone(), v.clone()));
        }
    }
    for (k, v) in parser::vars_list() {
        if !seen.iter().any(|s| s.as_str() == k.as_str()) {
            seen.push(k.clone());
            out.push((k, v));
        }
    }
    if !seen.iter().any(|s| s == "PWD") {
        out.push((String::from("PWD"), cwd::cwd_get()));
    }
    out
}

/// Profondita' shebang (come `SOURCE_DEPTH`: loop `a→b→a` mai infiniti).
static mut SHEBANG_DEPTH: u8 = 0;
/// Annidamento shebang massimo (lo script di livello 5 e' rifiutato).
const SHEBANG_MAX_DEPTH: u8 = 4;

/// Programma caricato e pronto al fork: immagine + argv figlio + riga jobs.
struct LoadedProg {
    img: Vec<u8>,
    argv: Vec<String>,
    cmdline: String,
}

/// Carica `prog` (come digitato) con `rest` come argv coda (43a): risoluzione
/// PATH (`resolve_prog`), shebang (`#!interp [arg]` → argv =
/// `[interp, script, args...]`, ricorsione bound — il kernel resta ELF-puro,
/// ADR-0005). L'env viaggia separato (il chiamante serializza). Errori su
/// stderr, `Err(())`. Carica nel PARENT (il figlio post-fork ha l'FS
/// avvelenato e non puo' piu' caricare).
fn load_prog(prog: &str, rest: &[&str]) -> Result<LoadedProg, ()> {
    let depth = unsafe { SHEBANG_DEPTH };
    if depth >= SHEBANG_MAX_DEPTH {
        term::term_err("run: shebang too deep\n");
        return Err(());
    }
    let path = if prog.contains('/') {
        cwd::resolve(prog)
    } else {
        match resolve_prog(prog) {
            Some(p) => p,
            None => {
                term::term_err("run: cannot load ");
                term::term_err(prog);
                term::term_err("\n");
                return Err(());
            }
        }
    };
    let img = match libr::load_file(&path) {
        Some(b) if !b.is_empty() => b,
        _ => {
            term::term_err("run: cannot load ");
            term::term_err(prog);
            term::term_err("\n");
            return Err(());
        }
    };
    // Shebang: solo testo con `#!` in testa (l'ELF passa oltre: magic diverso).
    // Convenzione classica: `#!interp [arg]` su una riga (`\r` tollerato).
    if img.len() > 2 && img[0] == b'#' && img[1] == b'!' {
        let line_end = img.iter().position(|&b| b == b'\n').unwrap_or(img.len());
        let line = core::str::from_utf8(&img[2..line_end]).unwrap_or("");
        let line = line.trim().trim_end_matches('\r').trim();
        let mut parts = line.splitn(2, |c| c == ' ' || c == '\t');
        let interp = parts.next().unwrap_or("").trim();
        let arg = parts.next().map(|s| s.trim()).filter(|s| !s.is_empty());
        if interp.is_empty() {
            term::term_err("run: bad interpreter\n");
            return Err(());
        }
        // Ricorsione sull'interprete (puo' essere a sua volta script):
        // argv = [interp, arg?, script, rest...].
        let mut sub: Vec<String> = Vec::new();
        if let Some(a) = arg {
            sub.push(String::from(a));
        }
        sub.push(path);
        for r in rest {
            sub.push(String::from(*r));
        }
        let sub_ref: Vec<&str> = sub.iter().map(|s| s.as_str()).collect();
        unsafe {
            SHEBANG_DEPTH = depth + 1;
        }
        let r = load_prog(interp, &sub_ref);
        unsafe {
            SHEBANG_DEPTH = depth;
        }
        return r;
    }
    let mut argv: Vec<String> = Vec::new();
    argv.push(path);
    for r in rest {
        argv.push(String::from(*r));
    }
    let mut cmdline = String::new();
    for (i, a) in argv.iter().enumerate() {
        if i > 0 {
            cmdline.push(' ');
        }
        cmdline.push_str(a);
    }
    Ok(LoadedProg { img, argv, cmdline })
}

/// `run <path> [args...] [&]`: lancia il programma (path cercato in `PATH`
/// se senza `/`, 43a; shebang rieseguito sull'interprete; `argv[0]` = path
/// risolto). `&` finale = background (prompt subito, `jobs`/`wait` dopo);
/// senza = foreground (attende l'uscita; code != 0 stampato come `[exit N]`).
/// `env` = prefissi `VAR=v` del comando (piu' VARS+PWD via `child_env`).
/// Con redirect (Fase 40.4c/d): il parent apre tutti i target in ordine +
/// grant single-use per fd distinto e contrabbanda la spec nell'ultimo argv
/// (magic); lo startup del figlio fa claim + `set_stdio` (tutti i programmi
/// via `entry!`, zero codice per-target). `2>&1` = alias (un grant solo).
pub(crate) fn cmd_run(
    args: &[&str],
    redirs: &[redirect::Redir],
    bg: bool,
    env: &[(String, String)],
) -> i64 {
    if args.len() < 2 {
        term::term_err("run: usage: run <path> [args...] [&]\n");
        return 1;
    }
    // Carica nel PARENT (il figlio non puo' piu' usare l'FS).
    let loaded = match load_prog(args[1], &args[2..]) {
        Ok(l) => l,
        Err(()) => return 1,
    };
    let full_env = child_env(env);
    let env_ref: Vec<(&str, &str)> =
        full_env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
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
                return 1;
            }
        };
        match grant_fds(fds) {
            Ok((s, g)) => {
                spec = s;
                grants = g;
            }
            Err(()) => {
                redirect::close_all(fds);
                term::term_err("run: redirect grant failed\n");
                return 1;
            }
        }
    }
    let argv_ref: Vec<&str> = loaded.argv.iter().map(|s| s.as_str()).collect();
    let buf = match libr::serialize_argv_redir_env(&argv_ref, &env_ref, &spec) {
        Some(b) => b,
        None => {
            redirect::close_all(fds);
            cancel_redir(&grants);
            term::term_err("run: argv troppo lunghi\n");
            return 1;
        }
    };
    let cmd = loaded.cmdline;
    match spawn_run_exec(&argv_ref, &loaded.img, &buf, fds, grants) {
        Err(()) => {
            term::term_err("run: fork failed\n");
            1
        }
        Ok(sp) => {
            jobs().push(Job {
                pid: sp.pid as i64,
                chan: sp.chan,
                cmd,
                bg,
                done: None,
                redir_grants: sp.grants,
            });
            if bg {
                let mut s = String::from("[bg pid ");
                cmd_info::push_u64(&mut s, sp.pid);
                s.push(']');
                term::term_print(&s);
                term::term_print("\n");
                return 0;
            }
            let idx = jobs().len() - 1;
            let chan = sp.chan;
            match wait_job(chan) {
                Some(0) => {
                    cancel_redir(&jobs()[idx].redir_grants);
                    jobs().remove(idx);
                    0
                }
                Some(code) => {
                    cancel_redir(&jobs()[idx].redir_grants);
                    jobs().remove(idx);
                    let mut s = String::from("[exit ");
                    cmd_info::push_u64(&mut s, code as u64);
                    s.push(']');
                    term::term_print(&s);
                    term::term_print("\n");
                    code
                }
                None => {
                    term::term_err("run: wait failed\n");
                    1
                }
            }
        }
    }
}

/// `jobs`: tabella dei job (fresca: prima drena le morti senza bloccare).
/// I finiti restano finche' `wait` non li rimuove (stato `done` visibile).
pub(crate) fn cmd_jobs() -> i64 {
    poll_reap();
    if jobs().is_empty() {
        term::term_print("no jobs\n");
        return 0;
    }
    for (i, j) in jobs().iter().enumerate() {
        print_job(i, j);
    }
    0
}

/// `wait [pid]`: attende i job (tutti, o quello col pid) e li rimuove,
/// stampando `pid <P>: exit <C>` per ciascuno.
pub(crate) fn cmd_wait(args: &[&str]) -> i64 {
    if args.len() >= 2 {
        let pid = match cmd_info::parse_i64(args[1]) {
            Some(p) => p,
            None => {
                term::term_err("wait: bad pid\n");
                return 1;
            }
        };
        let idx = match jobs().iter().position(|j| j.pid == pid) {
            Some(i) => i,
            None => {
                term::term_err("wait: no such job\n");
                return 1;
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
        return 0;
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
    0
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

// ── Pipeline `a | b | ...` + heredoc (Fase 42) ───────────────────────
// Ogni stadio gira nel proprio processo figlio (concorrenti, come bash):
// - stadio `run`: fork + exec come `cmd_run` (immagine precaricata dal parent);
// - stadio builtin: fork + re-init FS con ring freschi (`fs_child_reinit`,
//   niente aliasing coi ring del parent) + claim dei grant + builtin + exit.
// Gli stadi comunicano su pipe server-side (`libr::pipe`); gli espliciti
// (file/heredoc) vincono sui pipe-link per-slot. Status del gruppo = ultimo
// stadio (bash); `$?` threadato dal chiamante. Solo foreground: `&` su
// pipeline multi-stadio e' Fase 44. Gli stadi fg NON entrano nella tabella
// job (gruppo atteso inline e rimosso subito); `kill <pid>` resta per pid.

/// Breve attesa IF=1 tra retry di scrittura corpo heredoc (mai busy su syscall).
fn pipe_spin() {
    for _ in 0..200_000 {
        core::hint::spin_loop();
    }
}

/// Scrive un corpo heredoc nell'estremita' di scrittura e la chiude (Fase 42,
/// parent dopo tutti i fork: i lettori drenano in concorrenza). A pipe chiusa
/// o errore si ferma (mai hang); bound anti-loop su stallo senza progressi.
fn write_heredoc_body(w: i64, body: &str) {
    let bytes = body.as_bytes();
    let mut done = 0usize;
    let mut idle = 0u32;
    while done < bytes.len() {
        match libr::write_fs(w, &bytes[done..], bytes.len() - done) {
            Ok(n) if n > 0 => {
                done += n;
                idle = 0;
            }
            Ok(_) => {
                // Zero progressi (pipe piena, lettore lento): throttled.
                idle += 1;
                if idle > 100_000 {
                    break;
                }
                pipe_spin();
            }
            Err(_) => break, // Closed (lettore morto) o server morto: stop
        }
    }
    let _ = libr::close(w);
}

/// Fork per uno stadio builtin (Fase 42): il figlio re-inizializza l'FS con
/// ring freschi (mai i ring COW del parent), riscuote i grant della spec in
/// ordine (Alias replicato), imposta lo stdio, applica l'env dello stadio
/// alle proprie VARS (scoped: muoiono col figlio, 43a), esegue il builtin e
/// muore col suo codice (chiude prima gli fd: l'EOF si propaga subito, senza
/// aspettare il reclaim kernel). `fds`/`grants` del parent chiusi in ogni caso.
fn spawn_builtin_stage(
    argv: &[&str],
    env: &[(String, String)],
    fds: [i64; 3],
    spec: Vec<libr::RedirEntry>,
    grants: Vec<u64>,
) -> Result<Spawned, ()> {
    match libr::fork() {
        Err(_) => {
            redirect::close_all(fds);
            cancel_redir(&grants);
            Err(())
        }
        Ok(libr::ForkResult::Child { .. }) => {
            if !libr::fs_child_reinit() {
                let _ = libr::print_string(b"[shell] pipe: fs reinit failed\n");
                libr::exit(1);
            }
            let mut cfds = [-1i64; 3];
            for e in spec.iter() {
                match e {
                    libr::RedirEntry::Grant { vfd, nonce } => {
                        match libr::dup_claim(*nonce) {
                            Ok(fd) => cfds[*vfd as usize] = fd,
                            Err(_) => {
                                let _ = libr::print_string(b"[shell] pipe: claim failed\n");
                                libr::exit(1);
                            }
                        }
                    }
                    libr::RedirEntry::Alias { vfd, target } => {
                        cfds[*vfd as usize] = cfds[*target as usize];
                    }
                }
            }
            libr::set_stdio(cfds);
            for (k, v) in env.iter() {
                parser::vars_set(k, v);
            }
            let code = repl::dispatch_builtin(argv);
            // Chiudi gli fd rivendicati (l'EOF va agli altri stadi subito).
            let mut seen = [-1i64; 3];
            let mut n = 0usize;
            for &fd in cfds.iter() {
                if fd >= 0 && !seen[..n].contains(&fd) {
                    seen[n] = fd;
                    n += 1;
                    let _ = libr::close(fd);
                }
            }
            libr::exit(code);
        }
        Ok(libr::ForkResult::Parent { pid, chan }) => {
            redirect::close_all(fds);
            Ok(Spawned { pid, chan, grants })
        }
    }
}

/// Esegue una pipeline (stadi legati da `Conn::Pipe`, o comando singolo con
/// heredoc). Ritorna lo status dell'ultimo stadio (bash), 1 a setup fallito.
pub(crate) fn cmd_pipeline(stages: &[parser::Command]) -> i64 {
    let n = stages.len();
    if n == 0 {
        return 0;
    }
    // Background su pipeline: job control (Fase 44). I singoli `run ... &`
    // non passano di qui (via vecchia in exec_command).
    if stages.iter().any(|s| s.bg) {
        term::term_err("background pipeline: Fase 44 (usare `run ... &`)\n");
        return 1;
    }
    // 1. Link tra stadi: n-1 pipe (lettura allo stadio dopo, scrittura a prima).
    let mut links: Vec<(i64, i64)> = Vec::new();
    for _ in 0..n.saturating_sub(1) {
        match libr::pipe() {
            Ok((r, w)) => links.push((r, w)),
            Err(_) => {
                for (r, w) in links.iter() {
                    let _ = libr::close(*r);
                    let _ = libr::close(*w);
                }
                term::term_err("pipe: creazione fallita\n");
                return 1;
            }
        }
    }
    // Accumulatori per il cleanup a fallimento (close idempotenti server-side).
    let mut all_grants: Vec<u64> = Vec::new();
    let mut bodies: Vec<(i64, String)> = Vec::new();
    let mut spawned: Vec<Spawned> = Vec::new();
    // Indice di stadio per ogni figlio spawnato (gli stadi senza processo
    // — solo redirect — non partecipano alla wait ma hanno il loro codice).
    let mut spawned_idx: Vec<usize> = Vec::new();
    let mut codes: Vec<Option<i64>> = Vec::new();
    for _ in 0..n {
        codes.push(None);
    }
    let mut failed = false;
    for (i, st) in stages.iter().enumerate() {
        if failed {
            break;
        }
        // Env in pipeline: semantica subshell (bash) = nessun effetto sul
        // parent (builtin: VARS nel figlio; esterni: blocco envp; vuoti:
        // set scartati); argv vuoto (solo redirect/env): soli effetti
        // collaterali.
        let args: Vec<&str> = st.argv.iter().map(|s| s.as_str()).collect();
        // 2. Seed dai link + open dei file in ordine (ultimo vince per slot;
        // gli espliciti vincono sui link, come bash: prima la pipe, poi i
        // redirect). Le voci heredoc sono saltate da open_all (vedi sotto).
        let seed = [
            if i > 0 { links[i - 1].0 } else { -1 },
            if i + 1 < n { links[i].1 } else { -1 },
            -1,
        ];
        let mut fds = match redirect::open_all_seed(&st.redirs, seed) {
            Ok(f) => f,
            Err((t, e)) => {
                redirect::report_open_error(&t, e);
                failed = true;
                continue;
            }
        };
        // 3. Heredoc: vincono sullo slot se ultima voce (slot_source); il
        // corpo si scrive DOPO tutti i fork (i lettori drenano in concorrenza).
        // Si accumula prima nel vec di stadio: a stadio senza processo i corpi
        // si scartano (nessun lettore), altrimenti passano a `bodies`.
        let mut stage_bodies: Vec<(i64, String)> = Vec::new();
        for slot in 0..3u8 {
            match redirect::slot_source(&st.redirs, slot) {
                Some(r) if r.heredoc => {
                    let body = r.heredoc_body.clone().unwrap_or_default();
                    match libr::pipe() {
                        Ok((br, bw)) => {
                            let old = fds[slot as usize];
                            if old >= 0 && !links.iter().any(|(a, b)| *a == old || *b == old) {
                                let _ = libr::close(old);
                            }
                            fds[slot as usize] = br;
                            stage_bodies.push((bw, body));
                        }
                        Err(_) => {
                            redirect::report_open_error(&r.target, libr::Error::NoMemory);
                            failed = true;
                            break;
                        }
                    }
                }
                _ => {}
            }
        }
        if failed {
            redirect::close_all(fds);
            continue;
        }
        if failed {
            // Fallimento heredoc a meta': chiudi fd e corpi parziali.
            redirect::close_all(fds);
            for (w, _) in stage_bodies.iter() {
                let _ = libr::close(*w);
            }
            continue;
        }
        // 4. Grant dagli fd finali (stessa spec dei run singoli).
        let (spec, grants) = match grant_fds(fds) {
            Ok(v) => v,
            Err(()) => {
                redirect::close_all(fds);
                for (w, _) in stage_bodies.iter() {
                    let _ = libr::close(*w);
                }
                term::term_err("pipe: redirect grant failed\n");
                failed = true;
                continue;
            }
        };
        if args.is_empty() {
            // Solo redirect (o assign ignorata): effetti applicati, niente
            // processo; i corpi heredoc senza lettore si scartano subito.
            redirect::close_all(fds);
            cancel_redir(&grants);
            for (w, _) in stage_bodies.iter() {
                let _ = libr::close(*w);
            }
            codes[i] = Some(0);
            continue;
        }
        // Stadio esterno (`run` esplicito o bare word via PATH, 43a) oppure
        // builtin: `source`/`exit`/ignoti restano a `dispatch_builtin` nel
        // figlio (come prima); `run` senza path resta usage.
        let want_external = args[0] == "run" || !repl::is_builtin(args[0]);
        if want_external {
            // 5a. Stadio esterno: immagine precaricata + fork + exec.
            let prog = if args[0] == "run" {
                if args.len() < 2 {
                    redirect::close_all(fds);
                    cancel_redir(&grants);
                    for (w, _) in stage_bodies.iter() {
                        let _ = libr::close(*w);
                    }
                    term::term_err("run: usage: run <path> [args...]\n");
                    failed = true;
                    continue;
                }
                args[1]
            } else {
                args[0]
            };
            let rest: Vec<&str> =
                if args[0] == "run" { args[2..].to_vec() } else { args[1..].to_vec() };
            let loaded = match load_prog(prog, &rest) {
                Ok(l) => l,
                Err(()) => {
                    redirect::close_all(fds);
                    cancel_redir(&grants);
                    for (w, _) in stage_bodies.iter() {
                        let _ = libr::close(*w);
                    }
                    // `load_prog` ha gia' riportato (`cannot load`/shebang);
                    // la bare word ignota senza `/` merita il 127 classico
                    // (con `/` resta l'errore path, come nel singolo).
                    if args[0] != "run" && !prog.contains('/') {
                        term::term_err("unknown command: ");
                        term::term_err(args[0]);
                        term::term_err("\n");
                    }
                    failed = true;
                    continue;
                }
            };
            let full_env = child_env(&st.env);
            let env_ref: Vec<(&str, &str)> =
                full_env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
            let argv_ref: Vec<&str> =
                loaded.argv.iter().map(|s| s.as_str()).collect();
            let buf = match libr::serialize_argv_redir_env(&argv_ref, &env_ref, &spec) {
                Some(b) => b,
                None => {
                    redirect::close_all(fds);
                    cancel_redir(&grants);
                    for (w, _) in stage_bodies.iter() {
                        let _ = libr::close(*w);
                    }
                    term::term_err("run: argv troppo lunghi\n");
                    failed = true;
                    continue;
                }
            };
            match spawn_run_exec(&argv_ref, &loaded.img, &buf, fds, grants) {
                Ok(sp) => {
                    bodies.extend(stage_bodies);
                    all_grants.extend(sp.grants.iter());
                    spawned_idx.push(i);
                    spawned.push(Spawned { pid: sp.pid, chan: sp.chan, grants: Vec::new() });
                }
                Err(()) => {
                    for (w, _) in stage_bodies.iter() {
                        let _ = libr::close(*w);
                    }
                    term::term_err("run: fork failed\n");
                    failed = true;
                    continue;
                }
            }
        } else {
            // 5b. Stadio builtin: fork + re-init + claim + dispatch + exit
            // (l'env dello stadio vive nelle VARS del figlio: scoped).
            match spawn_builtin_stage(&args, &st.env, fds, spec, grants) {
                Ok(sp) => {
                    bodies.extend(stage_bodies);
                    all_grants.extend(sp.grants.iter());
                    spawned_idx.push(i);
                    spawned.push(Spawned { pid: sp.pid, chan: sp.chan, grants: Vec::new() });
                }
                Err(()) => {
                    for (w, _) in stage_bodies.iter() {
                        let _ = libr::close(*w);
                    }
                    term::term_err("pipe: fork failed\n");
                    failed = true;
                    continue;
                }
            }
        }
    }
    // 6. Fallimento a meta': chiudi tutto (link, corpi) e cancella i grant;
    // gli stadi gia' partiti muoiono da soli (EOF/Closed ai peer + la shell
    // non li attende: le loro EXIT_NOTIFY restano orfane e scartate dai
    // prossimi recv — mai wait infinta qui).
    if failed {
        for (r, w) in links.iter() {
            let _ = libr::close(*r);
            let _ = libr::close(*w);
        }
        for (w, _) in bodies.iter() {
            let _ = libr::close(*w);
        }
        cancel_redir(&all_grants);
        drain_spawned(&spawned);
        return 1;
    }
    // 7. Le copie parent di link ed estremita' si chiudono (i grant vivono
    // snapshot server-side; i figli hanno gia' le loro). Dopodiche' i corpi
    // heredoc (i lettori esistono tutti).
    for (r, w) in links.iter() {
        let _ = libr::close(*r);
        let _ = libr::close(*w);
    }
    for (w, body) in bodies.iter() {
        write_heredoc_body(*w, body);
    }
    // 8. waitpid di gruppo (nessuna syscall nuova: EXIT_NOTIFY sui canali di
    // nascita, come wait_job). Status = ultimo stadio (bash).
    let chans: Vec<u64> = spawned.iter().map(|s| s.chan).collect();
    let got = wait_all(&chans);
    for (k, c) in got.iter().enumerate() {
        if let Some(&si) = spawned_idx.get(k) {
            codes[si] = *c;
        }
    }
    cancel_redir(&all_grants);
    let last = codes[n - 1].unwrap_or(1);
    if last != 0 {
        let mut s = String::from("[exit ");
        cmd_info::push_u64(&mut s, last as u64);
        s.push(']');
        term::term_print(&s);
        term::term_print("\n");
    }
    last
}

/// A fallimento dopo fork parziali: drena senza bloccare le morti gia'
// arrivate (mai wait infinita in un percorso d'errore).
fn drain_spawned(spawned: &[Spawned]) {
    for _ in 0..32 {
        let mut any = false;
        while let Some(m) = libr::recv_poll() {
            any = true;
            if !libr::is_exit_notify(&m) {
                let _ = libr::reply(0, 0, 0);
            }
        }
        if !any {
            break;
        }
    }
    let _ = spawned;
}
