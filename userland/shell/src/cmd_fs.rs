use super::*;

// ── Commands ────────────────────────────────────────────────────────

pub(crate) fn cmd_ls(args: &[&str]) {
    // `ls [-l] [path]`: senza flag elenca i nomi; con -l una riga per entry
    // "tipo size nome[ (ro)]" (stretch Fase 19.2: metadati via libr::stat,
    // 1 round trip per entry — ok per directory piccole; niente owner/mtime,
    // `Stat` non li ha). Una write per riga (convenzione shell: i pezzi
    // restano contigui nel log seriale).
    let (long, raw) = match args.get(1) {
        Some(&"-l") => (true, args.get(2).copied().unwrap_or(".")),
        _ => (false, args.get(1).copied().unwrap_or(".")),
    };
    let path = cwd::resolve(raw);
    let mut buf = vec![0u8; 4096];
    if libr::readdir(&path, &mut buf, 4096).is_err() {
        term::term_print("ls: error\n");
        return;
    }
    // Formato "name\0name\0...\0\0"
    let mut i = 0;
    let mut wrote = false;
    while i < buf.len() {
        if buf[i] == 0 { break; }
        let start = i;
        while i < buf.len() && buf[i] != 0 { i += 1; }
        if let Ok(name) = core::str::from_utf8(&buf[start..i]) {
            if long {
                // Path assoluto dell'entry per stat (attento a "/" root).
                let mut full = path.clone();
                if !full.ends_with('/') {
                    full.push('/');
                }
                full.push_str(name);
                let mut line = String::new();
                let mut st = libr::Stat { size: 0, kind: 0, readonly: false };
                if libr::stat(&full, &mut st).is_ok() {
                    line.push(if st.is_dir() {
                        'd'
                    } else if st.is_device() {
                        'v'
                    } else {
                        '-'
                    });
                    line.push(' ');
                    cmd_info::push_u64(&mut line, st.size);
                    line.push(' ');
                    line.push_str(name);
                    if st.readonly {
                        line.push_str(" (ro)");
                    }
                } else {
                    // Race (entry rimossa tra readdir e stat): mai abortire.
                    line.push_str("? ");
                    line.push_str(name);
                }
                term::term_print(&line);
                term::term_print("\n");
            } else {
                term::term_print(name);
                term::term_print("  ");
            }
            wrote = true;
        }
        i += 1; // skip null
    }
    if wrote && !long {
        term::term_print("\n");
    }
}

pub(crate) fn cmd_cat(args: &[&str]) {
    if args.len() < 2 {
        term::term_print("cat: missing file\n");
        return;
    }
    let path = cwd::resolve(args[1]);
    let Ok(fd) = libr::open(&path, 0) else {
        term::term_print("cat: cannot open ");
        term::term_print(args[1]);
        term::term_print("\n");
        return;
    };
    let mut buf = vec![0u8; 4096];
    loop {
        match libr::read_fs(fd, &mut buf, 4096) {
            Ok(0) => break, // EOF
            Ok(n) => {
                if let Ok(s) = core::str::from_utf8(&buf[..n]) {
                    term::term_print(s);
                }
            }
            Err(_) => break,
        }
    }
    let _ = libr::close(fd);
    term::term_print("\n");
}

pub(crate) fn cmd_touch(args: &[&str]) {
    if args.len() < 2 {
        term::term_print("touch: missing file\n");
        return;
    }
    let path = cwd::resolve(args[1]);
    let Ok(fd) = libr::open(&path, libr::O_CREAT) else {
        term::term_print("touch: failed\n");
        return;
    };
    let _ = libr::close(fd);
}

pub(crate) fn cmd_mkdir(args: &[&str]) {
    if args.len() < 2 {
        term::term_print("mkdir: missing directory\n");
        return;
    }
    let path = cwd::resolve(args[1]);
    if libr::mkdir(&path).is_err() {
        term::term_print("mkdir: failed\n");
    }
}

pub(crate) fn cmd_mount(args: &[&str]) {
    if args.len() < 3 {
        term::term_print("mount: usage: mount <source> <target>\n");
        return;
    }
    // La sorgente NON si risolve: puo' essere `UUID=`/`LABEL=` o un device.
    let target = cwd::resolve(args[2]);
    if libr::mount(args[1], &target).is_err() {
        term::term_print("mount: failed\n");
    }
}

pub(crate) fn cmd_umount(args: &[&str]) {
    if args.len() < 2 {
        term::term_print("umount: usage: umount <target>\n");
        return;
    }
    let target = cwd::resolve(args[1]);
    if libr::umount(&target).is_err() {
        term::term_print("umount: failed (busy or not mounted?)\n");
    }
}

/// Copia file client-side (Fase 18.2): read a chunk + write. Usata da `cp`
/// e `mv`. Niente nuove op FS: mkdir/rmdir/rm su /fat restano rifiutati
/// (niente unlink, fuori scope), la write funziona (Fase 20) e la copia
/// fallisce pulita senza toccare la sorgente.
fn copy_file(src: &str, dst: &str) -> bool {
    let from = cwd::resolve(src);
    let to = cwd::resolve(dst);
    let Ok(fd_in) = libr::open(&from, 0) else {
        term::term_print("cp: cannot open ");
        term::term_print(src);
        term::term_print("\n");
        return false;
    };
    let Ok(fd_out) = libr::open(&to, 0x200 /* O_CREAT */) else {
        term::term_print("cp: cannot create ");
        term::term_print(dst);
        term::term_print("\n");
        let _ = libr::close(fd_in);
        return false;
    };
    let mut buf = vec![0u8; 4096];
    let mut ok = true;
    loop {
        // Come `cat`: EOF (Ok(0)) o errore chiudono il loop.
        let n = match libr::read_fs(fd_in, &mut buf, 4096) {
            Ok(n) => n,
            Err(_) => break,
        };
        if n == 0 {
            break;
        }
        if libr::write_fs(fd_out, &buf[..n], n) != Ok(n) {
            ok = false;
            break;
        }
    }
    let _ = libr::close(fd_in);
    let _ = libr::close(fd_out);
    if !ok {
        term::term_print("cp: I/O error\n");
    }
    ok
}
pub(crate) fn cmd_cp(args: &[&str]) {
    if args.len() < 3 {
        term::term_print("cp: usage: cp <src> <dst>\n");
        return;
    }
    copy_file(args[1], args[2]);
}

pub(crate) fn cmd_mv(args: &[&str]) {
    if args.len() < 3 {
        term::term_print("mv: usage: mv <src> <dst>\n");
        return;
    }
    // mv = cp + rm client-side, zero nuove op (Fase 18.2): la sorgente si
    // rimuove SOLO a copia riuscita.
    if !copy_file(args[1], args[2]) {
        return;
    }
    let src = cwd::resolve(args[1]);
    if libr::remove(&src).is_err() {
        term::term_print("mv: copied but cannot remove source\n");
    }
}

pub(crate) fn cmd_rm(args: &[&str]) {
    if args.len() < 2 {
        term::term_print("rm: missing file\n");
        return;
    }
    let path = cwd::resolve(args[1]);
    if libr::remove(&path).is_err() {
        term::term_print("rm: cannot remove ");
        term::term_print(args[1]);
        term::term_print("\n");
    }
}

pub(crate) fn cmd_rmdir(args: &[&str]) {
    if args.len() < 2 {
        term::term_print("rmdir: missing directory\n");
        return;
    }
    // Stessa op del server (dir vuote): il server rifiuta le non vuote.
    let path = cwd::resolve(args[1]);
    if libr::remove(&path).is_err() {
        term::term_print("rmdir: failed (not empty or missing?)\n");
    }
}
