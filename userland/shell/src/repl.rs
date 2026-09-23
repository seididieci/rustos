use super::*;

// ── Sorgente righe (tastiera vs script) ─────────────────────────────
// Il riempimento heredoc e il driver degli script pescano dalla stessa
// sorgente: un heredoc in script consuma le righe dopo la sua (come bash).

pub(crate) trait LineSrc {
    fn next_line(&mut self, prompt: &str) -> Option<String>;
}

pub(crate) struct TtySrc;

impl LineSrc for TtySrc {
    fn next_line(&mut self, prompt: &str) -> Option<String> {
        Some(term::read_line(prompt))
    }
}

/// Script in memoria: righe logiche e corpi heredoc condividono il cursore.
pub(crate) struct ScriptSrc {
    lines: Vec<String>,
    pos: usize,
}

impl ScriptSrc {
    pub(crate) fn new(text: &str) -> Self {
        let mut lines = Vec::new();
        for l in text.split('\n') {
            // Tolleranza CRLF (file iniettati da host via mtools).
            let mut s = String::from(l);
            if s.ends_with('\r') {
                s.pop();
            }
            lines.push(s);
        }
        Self { lines, pos: 0 }
    }
}

impl LineSrc for ScriptSrc {
    fn next_line(&mut self, _prompt: &str) -> Option<String> {
        if self.pos < self.lines.len() {
            let l = self.lines[self.pos].clone();
            self.pos += 1;
            Some(l)
        } else {
            None
        }
    }
}

/// Esito di una riga logica: `Continue` aggiorna `$?`, `Exit` termina lo
/// script col code (mai la shell: `exit` in script = fine script).
/// `None` = riga vuota (niente da eseguire, `$?` invariato).
pub(crate) enum LineOutcome {
    Continue(i64),
    Exit(i64),
}

// ── Entry point ─────────────────────────────────────────────────────

libr::entry!(real_main);
fn real_main(_sp: u64) -> ! {
    let _ = libr::print_string(b"[shell] starting\n");

    // Apri il terminale (tastiera + output VGA via console server).
    if !term::term_init() {
        let _ = libr::print_string(b"[shell] cannot open /dev/input/keyboard\n");
        libr::exit(1);
    }

    // Banner
    term::term_print("Velordor shell v0.1\n");
    term::term_print("Type 'help' for commands\n");
    term::term_print("\n");
    cwd::cwd_set(String::from("/"));
    let mut status: i64 = 0; // `$?`: exit code dell'ultimo comando eseguito

    // REPL
    loop {
        // Prompt dinamico con cwd (Fase 18.1-bis): "/" → "$ ", senno'
        // "<cwd>$ ". La cwd non passa mai da tty::emit: nessun impatto sul
        // floor del backspace (conta solo i digitati).
        let cwd = cwd::cwd_get();
        let prompt;
        if cwd == "/" {
            prompt = String::from("$ ");
        } else {
            prompt = cwd + "$ ";
        }
        let line = term::read_line(&prompt);
        // In REPL `Exit` non accade mai (script_mode=false: `exit` diverge
        // via libr::exit); l'arm resta per totalita'.
        match run_one_line(&line, status, &mut TtySrc, false) {
            None => {}
            Some(LineOutcome::Continue(s)) => status = s,
            Some(LineOutcome::Exit(s)) => status = s,
        }
    }
}

/// Esegue una riga logica: parse con `$?` + riempimento heredoc dalla
/// sorgente + esecuzione con short-circuit. `status` = `$?` in ingresso.
pub(crate) fn run_one_line(
    line: &str,
    status: i64,
    src: &mut dyn LineSrc,
    script_mode: bool,
) -> Option<LineOutcome> {
    // Parser Fase 41+42: quote/escape/commenti, ; && || & | <<, redirect
    // 40.4, $VAR ${VAR} $? $$, ~, glob. Comandi gia' espansi con connettori.
    let mut seq = match parser::parse_line(line, status) {
        Ok(s) => s,
        Err(e) => {
            match e {
                parser::ParseError::MissingTarget(op) => {
                    term::term_err("redirect: missing target after ");
                    term::term_err(op);
                }
                parser::ParseError::BadSubst => {
                    term::term_err("sostituzione errata");
                }
                parser::ParseError::EnvPrefix => {
                    term::term_err("VAR=v comando non supportato (Fase 43)");
                }
            }
            term::term_err("\n");
            return Some(LineOutcome::Continue(2));
        }
    };
    // Heredoc (Fase 42): per ogni `<<DELIM` senza corpo, leggi righe fino
    // alla riga col solo delimitatore (corpo sempre letterale: niente
    // parsing/espansione dentro). Come bash, si legge tutto PRIMA di
    // eseguire (anche per gli stadi dopo la pipe). In script le righe
    // vengono dal file, non dalla tastiera.
    if !fill_heredocs(&mut seq, src, script_mode) {
        return Some(LineOutcome::Continue(1));
    }
    exec_seq(&seq, status, script_mode)
}

/// Riempie i corpi heredoc mancanti dalla sorgente. False = EOF prima del
/// delimitatore (solo script: la tastiera non da' mai EOF).
fn fill_heredocs(seq: &mut parser::Seq, src: &mut dyn LineSrc, script: bool) -> bool {
    for cmd in seq.cmds.iter_mut() {
        for r in cmd.redirs.iter_mut() {
            if r.heredoc && r.heredoc_body.is_none() {
                let delim = r.target.clone();
                let mut body = String::new();
                loop {
                    match src.next_line("> ") {
                        Some(l) if l == delim => break,
                        Some(l) => {
                            body.push_str(&l);
                            body.push('\n');
                        }
                        None => {
                            if script {
                                term::term_err("source: unexpected EOF in heredoc\n");
                            }
                            return false;
                        }
                    }
                }
                r.heredoc_body = Some(body);
            }
        }
    }
    true
}

/// Esecuzione di una Seq: gruppi di stadi legati da Pipe girano concorrenti
/// (una pipeline); il resto resta sequenziale con short-circuit.
/// None = Seq vuota (`$?` invariato).
fn exec_seq(seq: &parser::Seq, status: i64, script_mode: bool) -> Option<LineOutcome> {
    if seq.cmds.is_empty() {
        return None;
    }
    let mut st = status;
    let mut i = 0;
    while i < seq.cmds.len() {
        let mut j = i;
        while j + 1 < seq.cmds.len()
            && matches!(seq.cons.get(j), Some(parser::Conn::Pipe))
        {
            j += 1;
        }
        if j > i {
            // Pipeline: short-circuit sul connettore prima del gruppo.
            if i > 0 {
                let go = match seq.cons.get(i - 1) {
                    Some(parser::Conn::And) => st == 0,
                    Some(parser::Conn::Or) => st != 0,
                    _ => true,
                };
                if !go {
                    i = j + 1;
                    continue;
                }
            }
            st = cmd_run::cmd_pipeline(&seq.cmds[i..=j]);
            i = j + 1;
            continue;
        }
        // Comando singolo: short-circuit come prima.
        if i > 0 {
            let go = match seq.cons.get(i - 1) {
                Some(parser::Conn::And) => st == 0,
                Some(parser::Conn::Or) => st != 0,
                _ => true,
            };
            if !go {
                i += 1;
                continue;
            }
        }
        let cmd = &seq.cmds[i];
        // Singolo con heredoc ma senza pipe: stessa via pipeline a 1
        // stadio (il percorso exec_single non sa i corpi heredoc).
        if cmd.redirs.iter().any(|r| r.heredoc) {
            st = cmd_run::cmd_pipeline(&seq.cmds[i..=i]);
        } else {
            match exec_single(cmd, st, script_mode) {
                LineOutcome::Continue(c) => st = c,
                LineOutcome::Exit(c) => return Some(LineOutcome::Exit(c)),
            }
        }
        i += 1;
    }
    Some(LineOutcome::Continue(st))
}

/// Esegue un comando parsato (redirect + dispatch + restore). `status` = `$?`
/// in ingresso (serve a `source`: la prima riga dello script espande il `$?`
/// esterno). In script_mode `exit` non uccide la shell ma termina lo script.
fn exec_single(cmd: &parser::Command, status: i64, script_mode: bool) -> LineOutcome {
    // Assegnazione persistente (senza comando): prima gli effetti dei
    // redirect (bash), poi il set. A open fallita: niente set.
    if let Some((name, val)) = &cmd.assign {
        if !cmd.redirs.is_empty() {
            match redirect::open_all(&cmd.redirs) {
                Ok(fds) => redirect::close_all(fds),
                Err((t, e)) => {
                    redirect::report_open_error(&t, e);
                    return LineOutcome::Continue(1);
                }
            }
        }
        parser::vars_set(name, val);
        return LineOutcome::Continue(0);
    }
    let args: Vec<&str> = cmd.argv.iter().map(|s| s.as_str()).collect();
    if args.is_empty() {
        // Solo redirect (`> /f`, `< /f`, ...): applica per gli effetti
        // collaterali (crea/tronca, verifica leggibilita'), senza eseguire
        // nulla. Errori su stderr.
        if !cmd.redirs.is_empty() {
            match redirect::open_all(&cmd.redirs) {
                Ok(fds) => redirect::close_all(fds),
                Err((t, e)) => {
                    redirect::report_open_error(&t, e);
                    return LineOutcome::Continue(1);
                }
            }
        }
        return LineOutcome::Continue(0);
    }
    // In script `exit [code]` termina lo script (mai la shell).
    if script_mode && args[0] == "exit" {
        let code = match args.get(1) {
            None => 0,
            Some(s) => match cmd_info::parse_i64(s) {
                Some(n) => n,
                None => {
                    term::term_err("exit: bad code\n");
                    1
                }
            },
        };
        return LineOutcome::Exit(code);
    }
    // Apre TUTTI i target in ordine; a fallimento riporta su stderr e
    // salta il comando (mai nel file).
    // `run` non passa di qui: apre+grant da se'.
    let mut fds = [-1i64; 3];
    if !cmd.redirs.is_empty() && args[0] != "run" {
        match redirect::open_all(&cmd.redirs) {
            Ok(f) => fds = f,
            Err((t, e)) => {
                redirect::report_open_error(&t, e);
                return LineOutcome::Continue(1);
            }
        }
        libr::set_stdio(fds);
    }
    let code = match args[0] {
        "run" => cmd_run::cmd_run(&args, &cmd.redirs, cmd.bg),
        "source" => cmd_source::cmd_source(&args, status),
        _ => dispatch_builtin(&args),
    };
    // Restore: output/errori giá instradati (hook B1 / term_err); le write
    // restano best-effort (mirror seriale gia' emesso).
    if fds != [-1i64; 3] {
        libr::clear_stdio();
        redirect::close_all(fds);
    }
    LineOutcome::Continue(code)
}

/// Dispatch dei builtin (Fase 42: condiviso tra esecuzione in-processo e
/// stadi builtin delle pipeline, che girano in figli fork con stdio proprio).
/// `run` resta fuori (fork+exec dedicata in `cmd_run`); ignoto = 127.
/// `source` in pipeline gira nel figlio (effetti scoped, `$?` iniziale 0).
pub(crate) fn dispatch_builtin(args: &[&str]) -> i64 {
    match args[0] {
        "ls" => cmd_fs::cmd_ls(args),
        "cat" => cmd_fs::cmd_cat(args),
        "touch" => cmd_fs::cmd_touch(args),
        "mkdir" => cmd_fs::cmd_mkdir(args),
        "mount" => cmd_fs::cmd_mount(args),
        "umount" => cmd_fs::cmd_umount(args),
        "echo" => cmd_info::cmd_echo(args),
        "clear" => cmd_info::cmd_clear(),
        "wc" => cmd_info::cmd_wc(args),
        "hexdump" => cmd_info::cmd_hexdump(args),
        "kill" => cmd_info::cmd_kill(args),
        "cd" => cwd::cmd_cd(args),
        "pwd" => cwd::cmd_pwd(),
        "cp" => cmd_fs::cmd_cp(args),
        "mv" => cmd_fs::cmd_mv(args),
        "rm" => cmd_fs::cmd_rm(args),
        "rmdir" => cmd_fs::cmd_rmdir(args),
        "ps" => cmd_info::cmd_ps(),
        "export" => cmd_info::cmd_export(args),
        "source" => cmd_source::cmd_source(args, 0),
        "jobs" => cmd_run::cmd_jobs(),
        "wait" => cmd_run::cmd_wait(args),
        "exit" => match args.get(1) {
            None => libr::exit(0),
            Some(s) => match cmd_info::parse_i64(s) {
                Some(n) => libr::exit(n),
                None => {
                    term::term_err("exit: bad code\n");
                    1
                }
            },
        },
        "help" => cmd_info::cmd_help(),
        _ => {
            term::term_err("unknown command: ");
            term::term_err(args[0]);
            term::term_err("\n");
            127
        }
    }
}
