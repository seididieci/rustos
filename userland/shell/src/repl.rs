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
        // Redirect (Fase 40.4): `> >> < 2> 2>> 2>&1`, bash-like (ultimo vince
        // per slot, `2>&1` aliasa). I builtin aprono qui; `run` apre+grant da
        // se' (handoff via argv-magic, claim nello startup del figlio).
        let (argv_owned, redirs) = match redirect::parse(&line) {
            Ok(v) => v,
            Err(redirect::ParseError::MissingTarget(op)) => {
                term::term_err("redirect: missing target after ");
                term::term_err(op);
                term::term_err("\n");
                continue;
            }
        };
        let args: Vec<&str> = argv_owned.iter().map(|s| s.as_str()).collect();
        if args.is_empty() {
            // Solo redirect (`> /f`, `< /f`, ...): applica per gli effetti
            // collaterali (crea/tronca, verifica leggibilita'), senza eseguire
            // nulla. Errori su stderr.
            if !redirs.is_empty() {
                match redirect::open_all(&redirs) {
                    Ok(fds) => redirect::close_all(fds),
                    Err((t, e)) => redirect::report_open_error(&t, e),
                }
            }
            continue;
        }
        // Apre TUTTI i target in ordine; a fallimento riporta su stderr e
        // salta il comando (mai nel file).
        // `run` non passa di qui: apre+grant da se'.
        let mut fds = [-1i64; 3];
        if !redirs.is_empty() && args[0] != "run" {
            match redirect::open_all(&redirs) {
                Ok(f) => fds = f,
                Err((t, e)) => {
                    redirect::report_open_error(&t, e);
                    continue;
                }
            }
            libr::set_stdio(fds);
        }
        match args[0] {
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
            "run" => cmd_run::cmd_run(&args, &redirs),
            "jobs" => cmd_run::cmd_jobs(),
            "wait" => cmd_run::cmd_wait(&args),
            "exit" => libr::exit(0),
            "help" => cmd_info::cmd_help(),
            _ => {
                term::term_err("unknown command: ");
                term::term_err(args[0]);
                term::term_err("\n");
            }
        }
        // Restore: output/errori giá instradati (hook B1 / term_err); le write
        // restano best-effort (mirror seriale gia' emesso).
        if fds != [-1i64; 3] {
            libr::clear_stdio();
            redirect::close_all(fds);
        }
    }
}
