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
        // Redirect (Fase 40.4a/b): parsing sempre; `<`/`2>`/`2>>`/`2>&1`
        // applicati in 40.4c/d; `run` con redirect in 40.4c (handoff grant).
        // I builtin `>`/`>>` aprono qui (40.4b): errori sul terminale.
        let (argv_owned, redirs) = match redirect::parse(&line) {
            Ok(v) => v,
            Err(redirect::ParseError::MissingTarget(op)) => {
                term::term_print("redirect: missing target after ");
                term::term_print(op);
                term::term_print("\n");
                continue;
            }
        };
        let args: Vec<&str> = argv_owned.iter().map(|s| s.as_str()).collect();
        if args.is_empty() {
            // `> /f` da sola crea/tronca (bash-like), senza eseguire nulla.
            if redirs.iter().any(|r| r.slot == 1) {
                match redirect::open_stdout(&redirs) {
                    Ok(fd) => {
                        if fd >= 0 {
                            let _ = libr::close(fd);
                        }
                    }
                    Err(e) => {
                        let t = redirs.iter().find(|r| r.slot == 1).unwrap().target.clone();
                        redirect::report_open_error(&t, e);
                    }
                }
            } else if redirs.iter().any(|r| r.slot != 1) {
                term::term_print("redirect not yet supported\n");
            }
            continue;
        }
        // `<`/`2>`/`2>>`/`2>&1` in 40.4d; stdout (`>`/`>>`) qui per i builtin
        // (40.4b) e per `run` (40.4c, handoff grant via argv-magic).
        if redirs.iter().any(|r| r.slot != 1) {
            term::term_print("redirect not yet supported\n");
            continue;
        }
        // Apre TUTTI i target stdout in ordine (ultimo vince); a fallimento
        // riporta sul terminale e salta il comando (mai nel file).
        // `run` non passa di qui: apre+grant da se' (40.4c, claim nel figlio).
        let mut out_fd: i64 = -1;
        let want_out = redirs.iter().any(|r| r.slot == 1);
        if want_out && args[0] != "run" {
            match redirect::open_stdout(&redirs) {
                Ok(fd) => out_fd = fd,
                Err(e) => {
                    let t = redirs.iter().find(|r| r.slot == 1).unwrap().target.clone();
                    redirect::report_open_error(&t, e);
                    continue;
                }
            }
            if out_fd >= 0 {
                libr::set_stdio([-1, out_fd, -1]);
            }
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
                term::term_print("unknown command: ");
                term::term_print(args[0]);
                term::term_print("\n");
            }
        }
        // Restore: il comando ha scritto sul file via hook B1; errori di
        // write restano best-effort (mirror seriale gia' emesso).
        if out_fd >= 0 {
            libr::clear_stdio();
            let _ = libr::close(out_fd);
        }
    }
}
