use super::*;

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
        // Parser Fase 41: quote/escape/commenti, ; && || & |, redirect 40.4,
        // $VAR ${VAR} $? $$, ~, glob. Comandi gia' espansi con connettori.
        let seq = match parser::parse_line(&line, status) {
            Ok(s) => s,
            Err(e) => {
                match e {
                    parser::ParseError::MissingTarget(op) => {
                        term::term_err("redirect: missing target after ");
                        term::term_err(op);
                    }
                    parser::ParseError::PipeUnsupported => {
                        term::term_err("pipe non supportata (Fase 42)");
                    }
                    parser::ParseError::BadSubst => {
                        term::term_err("sostituzione errata");
                    }
                    parser::ParseError::EnvPrefix => {
                        term::term_err("VAR=v comando non supportato (Fase 43)");
                    }
                }
                term::term_err("\n");
                status = 2;
                continue;
            }
        };
        for (i, cmd) in seq.cmds.iter().enumerate() {
            // Short-circuit: && corre solo a status 0, || solo a != 0.
            if i > 0 {
                let go = match seq.cons.get(i - 1) {
                    Some(parser::Conn::And) => status == 0,
                    Some(parser::Conn::Or) => status != 0,
                    _ => true,
                };
                if !go {
                    continue;
                }
            }
            status = exec_command(cmd);
        }
    }
}

/// Esegue un comando parsato (redirect + dispatch + restore) e ritorna
/// l'exit code per `$?`/`&&`/`||` (0 ok, 1 errore, 127 ignoto, 2 parse).
fn exec_command(cmd: &parser::Command) -> i64 {
    // Assegnazione persistente (senza comando): prima gli effetti dei
    // redirect (bash), poi il set. A open fallita: niente set.
    if let Some((name, val)) = &cmd.assign {
        if !cmd.redirs.is_empty() {
            match redirect::open_all(&cmd.redirs) {
                Ok(fds) => redirect::close_all(fds),
                Err((t, e)) => {
                    redirect::report_open_error(&t, e);
                    return 1;
                }
            }
        }
        parser::vars_set(name, val);
        return 0;
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
                    return 1;
                }
            }
        }
        return 0;
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
                return 1;
            }
        }
        libr::set_stdio(fds);
    }
    let code = match args[0] {
        "ls" => cmd_fs::cmd_ls(&args),
        "cat" => cmd_fs::cmd_cat(&args),
        "touch" => cmd_fs::cmd_touch(&args),
        "mkdir" => cmd_fs::cmd_mkdir(&args),
        "mount" => cmd_fs::cmd_mount(&args),
        "umount" => cmd_fs::cmd_umount(&args),
        "echo" => cmd_info::cmd_echo(&args),
        "clear" => cmd_info::cmd_clear(),
        "wc" => cmd_info::cmd_wc(&args),
        "hexdump" => cmd_info::cmd_hexdump(&args),
        "kill" => cmd_info::cmd_kill(&args),
        "cd" => cwd::cmd_cd(&args),
        "pwd" => cwd::cmd_pwd(),
        "cp" => cmd_fs::cmd_cp(&args),
        "mv" => cmd_fs::cmd_mv(&args),
        "rm" => cmd_fs::cmd_rm(&args),
        "rmdir" => cmd_fs::cmd_rmdir(&args),
        "ps" => cmd_info::cmd_ps(),
        "export" => cmd_info::cmd_export(&args),
        "run" => cmd_run::cmd_run(&args, &cmd.redirs, cmd.bg),
        "jobs" => cmd_run::cmd_jobs(),
        "wait" => cmd_run::cmd_wait(&args),
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
    };
    // Restore: output/errori giá instradati (hook B1 / term_err); le write
    // restano best-effort (mirror seriale gia' emesso).
    if fds != [-1i64; 3] {
        libr::clear_stdio();
        redirect::close_all(fds);
    }
    code
}
