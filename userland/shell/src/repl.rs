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
        let trimmed = line.trim();
        if trimmed.is_empty() { continue; }

        let args: Vec<&str> = trimmed.split_whitespace().collect();
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
            "run" => cmd_run::cmd_run(&args),
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
    }
}
