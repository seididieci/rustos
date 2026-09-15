//! usershell — Shell interattiva per Velordor (Fase 9.4, utility Fase 18).
//!
//! Client del terminale: NON mappa il VGA. Apre `/dev/input/keyboard` e usa lo
//! stesso fd per leggere i tasti (read) e per scrivere l'output (write): il
//! console server possiede la VGA, disegna l'output e fa l'echo dei tasti
//! (Opzione B). La shell gestisce solo la linea logica dei comandi.
//! Comandi: ls, cat, touch, mkdir, mount, umount, echo, clear, wc, hexdump,
//! kill, cd, pwd, cp, mv, rm, rmdir, exit, help. Tutti i path passano per
//! `resolve()`: la shell tiene una cwd client-side e accetta path relativi
//! (Fase 18.1).

#![no_std]
#![no_main]

extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;
use alloc::vec;
use libr;

// ── Terminale (device console) ──────────────────────────────────────

const KEYBOARD_PATH: &str = "/dev/input/keyboard";

static mut TERM_FD: i64 = -1;

/// Directory corrente, client-side (Fase 18.1): il FS non ha concetto di cwd,
/// la risoluzione e' tutta qui (`resolve()`). Sempre path assoluto normalizzato.
static mut CWD: Option<String> = None;

fn cwd_get() -> String {
    // Niente shared ref diretto allo static (hard error `static_mut_refs`):
    // si passa dal raw pointer (single-threaded, niente aliasing reale).
    unsafe {
        (*core::ptr::addr_of_mut!(CWD))
            .clone()
            .unwrap_or_else(|| String::from("/"))
    }
}

fn cwd_set(s: String) {
    unsafe {
        CWD = Some(s);
    }
}

/// Normalizza un path: collassa `//`, risolve `.`/`..` (mai sopra `/`).
fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for comp in path.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            c => parts.push(c),
        }
    }
    if parts.is_empty() {
        return String::from("/");
    }
    let mut s = String::new();
    for p in parts {
        s.push('/');
        s.push_str(p);
    }
    s
}

/// Risolve un path utente in assoluto normalizzato (relativo → contro cwd).
fn resolve(path: &str) -> String {
    if path.starts_with('/') {
        return normalize(path);
    }
    let mut s = cwd_get();
    if !s.ends_with('/') {
        s.push('/');
    }
    s.push_str(path);
    normalize(&s)
}

/// Breve attesa in SOLO spin (nessuna syscall): la CPU resta con IF=1, quindi
/// la preemption del timer funziona e non si affama il sistema (a differenza
/// di un busy-loop su `get_ticks`, che tiene gli interrupt mascherati).
fn spin_brief() {
    for _ in 0..1_000_000 {
        core::hint::spin_loop();
    }
}

fn term_init() -> bool {
    // Il mount /dev/input viene registrato dal console server al suo avvio:
    // ritenta se l'open iniziale fallisce (race di boot).
    for _ in 0..100 {
        unsafe {
            TERM_FD = libr::open(KEYBOARD_PATH, 0);
            if TERM_FD >= 0 {
                return true;
            }
        }
        spin_brief();
    }
    false
}

/// Scrive byte sul terminale: userfs li inoltra al console server che li
/// disegna sulla VGA (DEV_WRITE). Echo dei tasti gestito dal console.
/// Mirror su seriale per debugging e per test automatici.
fn term_write_bytes(data: &[u8]) {
    let _ = libr::print_string(data);
    unsafe {
        let _ = libr::write_fs(TERM_FD, data, data.len());
    }
}

fn term_print(s: &str) {
    term_write_bytes(s.as_bytes());
}

/// Legge un byte di input dal terminale. Quando il buffer e' vuoto attende un
/// breve spin (IF=1) prima di riprovare: niente busy-loop su syscall.
fn kbd_read_byte() -> Option<u8> {
    let mut buf = [0u8; 1];
    loop {
        let n = unsafe { libr::read_fs(TERM_FD, &mut buf, 1) };
        if n > 0 {
            return Some(buf[0]);
        }
        spin_brief();
    }
}

// ── Line editing ────────────────────────────────────────────────────

/// Legge una riga. Il console server fa gia' l'echo: qui costruiamo solo la
/// stringa logica (backspace = pop, Enter = fine riga).
fn read_line(prompt: &str) -> String {
    term_print(prompt);
    let mut line = String::new();
    loop {
        let Some(b) = kbd_read_byte() else { continue; };
        match b {
            b'\n' | b'\r' => return line,
            0x08 => {
                line.pop();
            }
            0x20..=0x7e => {
                line.push(b as char);
            }
            _ => {}
        }
    }
}

// ── Commands ────────────────────────────────────────────────────────

fn cmd_ls(args: &[&str]) {
    // `ls [-l] [path]`: senza flag elenca i nomi; con -l una riga per entry
    // "tipo size nome[ (ro)]" (stretch Fase 19.2: metadati via libr::stat,
    // 1 round trip per entry — ok per directory piccole; niente owner/mtime,
    // `Stat` non li ha). Una write per riga (convenzione shell: i pezzi
    // restano contigui nel log seriale).
    let (long, raw) = match args.get(1) {
        Some(&"-l") => (true, args.get(2).copied().unwrap_or(".")),
        _ => (false, args.get(1).copied().unwrap_or(".")),
    };
    let path = resolve(raw);
    let mut buf = vec![0u8; 4096];
    let n = libr::readdir(&path, &mut buf, 4096);
    if n < 0 {
        term_print("ls: error\n");
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
                if libr::stat(&full, &mut st) == 0 {
                    line.push(if st.is_dir() {
                        'd'
                    } else if st.is_device() {
                        'v'
                    } else {
                        '-'
                    });
                    line.push(' ');
                    push_u64(&mut line, st.size);
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
                term_print(&line);
                term_print("\n");
            } else {
                term_print(name);
                term_print("  ");
            }
            wrote = true;
        }
        i += 1; // skip null
    }
    if wrote && !long {
        term_print("\n");
    }
}

fn cmd_cat(args: &[&str]) {
    if args.len() < 2 {
        term_print("cat: missing file\n");
        return;
    }
    let path = resolve(args[1]);
    let fd = libr::open(&path, 0);
    if fd < 0 {
        term_print("cat: cannot open ");
        term_print(args[1]);
        term_print("\n");
        return;
    }
    let mut buf = vec![0u8; 4096];
    loop {
        let n =  libr::read_fs(fd, &mut buf, 4096);
        if n <= 0 { break; }
        if let Ok(s) = core::str::from_utf8(&buf[..n as usize]) {
            term_print(s);
        }
    }
    libr::close(fd);
    term_print("\n");
}

fn cmd_touch(args: &[&str]) {
    if args.len() < 2 {
        term_print("touch: missing file\n");
        return;
    }
    let path = resolve(args[1]);
    let fd = libr::open(&path, libr::O_CREAT);
    if fd < 0 {
        term_print("touch: failed\n");
        return;
    }
    libr::close(fd);
}

fn cmd_mkdir(args: &[&str]) {
    if args.len() < 2 {
        term_print("mkdir: missing directory\n");
        return;
    }
    let path = resolve(args[1]);
    let r = libr::mkdir(&path);
    if r < 0 {
        term_print("mkdir: failed\n");
    }
}

fn cmd_mount(args: &[&str]) {
    if args.len() < 3 {
        term_print("mount: usage: mount <source> <target>\n");
        return;
    }
    // La sorgente NON si risolve: puo' essere `UUID=`/`LABEL=` o un device.
    let target = resolve(args[2]);
    if libr::mount(args[1], &target) < 0 {
        term_print("mount: failed\n");
    }
}

fn cmd_umount(args: &[&str]) {
    if args.len() < 2 {
        term_print("umount: usage: umount <target>\n");
        return;
    }
    let target = resolve(args[1]);
    if libr::umount(&target) < 0 {
        term_print("umount: failed (busy or not mounted?)\n");
    }
}

fn cmd_help() {
    term_print("Commands: ls [-l] [path], cat <file>, touch <file>, mkdir <dir>, mount <src> <tgt>, umount <tgt>, echo [args], clear, wc <file>, hexdump <file>, kill <pid|service>, cd [dir], pwd, cp <src> <dst>, mv <src> <dst>, rm <file>, rmdir <dir>, ps, exit, help\n");
}

/// Accoda `s` paddata a `width` con spazi (colonne `ps`, niente format!).
fn push_padded(out: &mut String, s: &str, width: usize) {
    out.push_str(s);
    let mut n = s.len();
    while n < width {
        out.push(' ');
        n += 1;
    }
}

/// `ps` tabellare stile Linux (Fase 19.1): PID NAME PRIO STATE TIME PARENT.
/// STATE = run (se stesso) / ready / recv / reply / blocked; TIME = tick
/// consumati (10 ms); PARENT = pid del padre ("-" per init/idle).
fn cmd_ps() {
    let me = libr::getpid() as u32;
    let mut out = String::from("PID  NAME           PRIO STATE TIME PARENT\n");
    for pid in 0..libr::PS_SCAN_MAX {
        let Some(e) = libr::ps_info(pid) else { continue; };
        let mut cell = String::new();
        push_u64(&mut cell, pid as u64);
        push_padded(&mut out, &cell, 5);
        push_padded(&mut out, e.name_str(), 15);
        cell.clear();
        push_u64(&mut cell, e.prio as u64);
        push_padded(&mut out, &cell, 5);
        let state = if pid == me {
            "run"
        } else if e.state == 0 {
            "ready"
        } else if e.ipc == 1 {
            "recv"
        } else if e.ipc == 2 {
            "reply"
        } else {
            "blocked"
        };
        push_padded(&mut out, state, 6);
        cell.clear();
        push_u64(&mut cell, e.ticks);
        out.push_str(&cell);
        out.push(' ');
        match e.parent {
            Some(p) => {
                cell.clear();
                push_u64(&mut cell, p as u64);
                out.push_str(&cell);
            }
            None => out.push('-'),
        }
        out.push('\n');
    }
    term_print(&out);
}

// ── Utility Fase 18.1 ───────────────────────────────────────────────

fn cmd_echo(args: &[&str]) {
    // Una sola write per riga: ogni term_print e' un IPC + una riga di
    // seriale col timestamp — i pezzi non sarebbero mai contigui nel log.
    let mut s = String::new();
    for (i, a) in args.iter().skip(1).enumerate() {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(a);
    }
    term_print(&s);
    term_print("\n");
}

fn cmd_clear() {
    // Form feed: la console pulisce tutto e torna home (Fase 18.1).
    term_write_bytes(b"\x0c");
}

/// Accoda un u64 in decimale (niente `format!`: no_std minimale).
fn push_u64(s: &mut String, mut v: u64) {
    if v == 0 {
        s.push('0');
        return;
    }
    let mut digs = [0u8; 20];
    let mut n = 0;
    while v > 0 {
        digs[n] = b'0' + (v % 10) as u8;
        v /= 10;
        n += 1;
    }
    for i in (0..n).rev() {
        s.push(digs[i] as char);
    }
}

fn cmd_wc(args: &[&str]) {
    if args.len() < 2 {
        term_print("wc: missing file\n");
        return;
    }
    let path = resolve(args[1]);
    let fd = libr::open(&path, 0);
    if fd < 0 {
        term_print("wc: cannot open ");
        term_print(args[1]);
        term_print("\n");
        return;
    }
    let mut buf = vec![0u8; 4096];
    let (mut lines, mut words, mut bytes) = (0u64, 0u64, 0u64);
    let mut in_word = false;
    loop {
        let n = libr::read_fs(fd, &mut buf, 4096);
        if n <= 0 {
            break;
        }
        for &b in &buf[..n as usize] {
            bytes += 1;
            if b == b'\n' {
                lines += 1;
            }
            if b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' {
                in_word = false;
            } else if !in_word {
                in_word = true;
                words += 1;
            }
        }
    }
    libr::close(fd);
    let mut s = String::new();
    push_u64(&mut s, lines);
    s.push(' ');
    push_u64(&mut s, words);
    s.push(' ');
    push_u64(&mut s, bytes);
    s.push(' ');
    s.push_str(args[1]);
    term_print(&s);
    term_print("\n");
}

fn hex_of(nib: u8) -> u8 {
    b"0123456789abcdef"[(nib & 0x0f) as usize]
}

fn push_hex_byte(s: &mut String, b: u8) {
    s.push(hex_of(b >> 4) as char);
    s.push(hex_of(b) as char);
}

fn cmd_hexdump(args: &[&str]) {
    if args.len() < 2 {
        term_print("hexdump: missing file\n");
        return;
    }
    let path = resolve(args[1]);
    let fd = libr::open(&path, 0);
    if fd < 0 {
        term_print("hexdump: cannot open ");
        term_print(args[1]);
        term_print("\n");
        return;
    }
    let mut buf = vec![0u8; 16];
    let mut off = 0usize;
    loop {
        let n = libr::read_fs(fd, &mut buf, 16);
        if n <= 0 {
            break;
        }
        // Una sola write per riga (vedi cmd_echo: timestamp per write).
        let mut s = String::new();
        for shift in (0..8).rev() {
            s.push(hex_of((off >> (shift * 4)) as u8) as char);
        }
        s.push_str(": ");
        for i in 0..n as usize {
            push_hex_byte(&mut s, buf[i]);
            s.push(' ');
        }
        term_print(&s);
        term_print("\n");
        off += n as usize;
    }
    libr::close(fd);
}

fn parse_i64(s: &str) -> Option<i64> {
    if s.is_empty() {
        return None;
    }
    let bytes = s.as_bytes();
    let (neg, digs) = match bytes[0] {
        b'-' => (true, &bytes[1..]),
        b'+' => (false, &bytes[1..]),
        _ => (false, &bytes[..]),
    };
    if digs.is_empty() {
        return None;
    }
    let mut v: i64 = 0;
    for &b in digs {
        if !b.is_ascii_digit() {
            return None;
        }
        v = v.checked_mul(10)?.checked_add((b - b'0') as i64)?;
    }
    Some(if neg { -v } else { v })
}

fn service_by_name(name: &str) -> Option<libr::Service> {
    match name {
        "console" => Some(libr::Service::Console),
        "fs" => Some(libr::Service::Fs),
        "devfs" => Some(libr::Service::Devfs),
        "init" => Some(libr::Service::Init),
        "kbd" => Some(libr::Service::Kbd),
        "tty" => Some(libr::Service::Tty),
        "disk" => Some(libr::Service::Disk),
        _ => None,
    }
}

fn cmd_kill(args: &[&str]) {
    if args.len() < 2 {
        term_print("kill: usage: kill <pid|service>\n");
        return;
    }
    let pid = match parse_i64(args[1]) {
        Some(p) => p,
        // init e' sempre pid 1 (il kernel spawna solo lui) ma non registra
        // il servizio: niente lookup, diretto.
        None if args[1] == "init" => 1,
        None => match service_by_name(args[1]) {
            Some(svc) => match libr::service_pid(svc) {
                Ok(p) => p,
                Err(_) => {
                    term_print("kill: service not running\n");
                    return;
                }
            },
            None => {
                term_print("kill: unknown pid/service\n");
                return;
            }
        },
    };
    if libr::kill(pid, 1).is_err() {
        term_print("kill: failed (init/self/unknown?)\n");
    }
}

/// Copia file client-side (Fase 18.2): read a chunk + write. Usata da `cp`
/// e `mv`. Niente nuove op FS: su /fat la write rifiuta (read-only) e la
/// copia fallisce pulita senza toccare la sorgente.
fn copy_file(src: &str, dst: &str) -> bool {
    let from = resolve(src);
    let to = resolve(dst);
    let fd_in = libr::open(&from, 0);
    if fd_in < 0 {
        term_print("cp: cannot open ");
        term_print(src);
        term_print("\n");
        return false;
    }
    let fd_out = libr::open(&to, 0x200 /* O_CREAT */);
    if fd_out < 0 {
        term_print("cp: cannot create ");
        term_print(dst);
        term_print("\n");
        libr::close(fd_in);
        return false;
    }
    let mut buf = vec![0u8; 4096];
    let mut ok = true;
    loop {
        // Come `cat`: n<=0 chiude il loop. Nota: a EOF il server NON scrive
        // response frame (solo i driver lo fanno sempre) e la read torna -1:
        // trattarlo da fatale dopo una copia completa e' sbagliato.
        let n = libr::read_fs(fd_in, &mut buf, 4096);
        if n <= 0 {
            break;
        }
        let n = n as usize;
        if libr::write_fs(fd_out, &buf[..n], n) != n as i64 {
            ok = false;
            break;
        }
    }
    libr::close(fd_in);
    libr::close(fd_out);
    if !ok {
        term_print("cp: I/O error\n");
    }
    ok
}

fn cmd_cp(args: &[&str]) {
    if args.len() < 3 {
        term_print("cp: usage: cp <src> <dst>\n");
        return;
    }
    copy_file(args[1], args[2]);
}

fn cmd_mv(args: &[&str]) {
    if args.len() < 3 {
        term_print("mv: usage: mv <src> <dst>\n");
        return;
    }
    // mv = cp + rm client-side, zero nuove op (Fase 18.2): la sorgente si
    // rimuove SOLO a copia riuscita.
    if !copy_file(args[1], args[2]) {
        return;
    }
    let src = resolve(args[1]);
    if libr::remove(&src) < 0 {
        term_print("mv: copied but cannot remove source\n");
    }
}

fn cmd_rm(args: &[&str]) {
    if args.len() < 2 {
        term_print("rm: missing file\n");
        return;
    }
    let path = resolve(args[1]);
    if libr::remove(&path) < 0 {
        term_print("rm: cannot remove ");
        term_print(args[1]);
        term_print("\n");
    }
}

fn cmd_rmdir(args: &[&str]) {
    if args.len() < 2 {
        term_print("rmdir: missing directory\n");
        return;
    }
    // Stessa op del server (dir vuote): il server rifiuta le non vuote.
    let path = resolve(args[1]);
    if libr::remove(&path) < 0 {
        term_print("rmdir: failed (not empty or missing?)\n");
    }
}

fn cmd_cd(args: &[&str]) {    if args.len() < 2 {
        cwd_set(String::from("/"));
        return;
    }
    let path = resolve(args[1]);
    // Sonda senza effetti collaterali: readdir fallisce su file/inesistenti
    // (open creerebbe il file: mai usarlo come sonda).
    let mut probe = vec![0u8; 256];
    if libr::readdir(&path, &mut probe, 256) < 0 {
        term_print("cd: no such directory: ");
        term_print(args[1]);
        term_print("\n");
        return;
    }
    cwd_set(path);
}

fn cmd_pwd() {
    term_print(&cwd_get());
    term_print("\n");
}

// ── Entry point ─────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let _ = libr::print_string(b"[shell] starting\n");

    // Apri il terminale (tastiera + output VGA via console server).
    if !term_init() {
        let _ = libr::print_string(b"[shell] cannot open /dev/input/keyboard\n");
        libr::exit(1);
    }

    // Banner
    term_print("Velordor shell v0.1\n");
    term_print("Type 'help' for commands\n");
    term_print("\n");
    cwd_set(String::from("/"));

    // REPL
    loop {
        // Prompt dinamico con cwd (Fase 18.1-bis): "/" → "$ ", senno'
        // "<cwd>$ ". La cwd non passa mai da tty::emit: nessun impatto sul
        // floor del backspace (conta solo i digitati).
        let cwd = cwd_get();
        let prompt;
        if cwd == "/" {
            prompt = String::from("$ ");
        } else {
            prompt = cwd + "$ ";
        }
        let line = read_line(&prompt);
        let trimmed = line.trim();
        if trimmed.is_empty() { continue; }

        let args: Vec<&str> = trimmed.split_whitespace().collect();
        match args[0] {
            "ls" => cmd_ls(&args),
            "cat" => cmd_cat(&args),
            "touch" => cmd_touch(&args),
            "mkdir" => cmd_mkdir(&args),
            "mount" => cmd_mount(&args),
            "umount" => cmd_umount(&args),
            "echo" => cmd_echo(&args),
            "clear" => cmd_clear(),
            "wc" => cmd_wc(&args),
            "hexdump" => cmd_hexdump(&args),
            "kill" => cmd_kill(&args),
            "cd" => cmd_cd(&args),
            "pwd" => cmd_pwd(),
            "cp" => cmd_cp(&args),
            "mv" => cmd_mv(&args),
            "rm" => cmd_rm(&args),
            "rmdir" => cmd_rmdir(&args),
            "ps" => cmd_ps(),
            "exit" => libr::exit(0),
            "help" => cmd_help(),
            _ => {
                term_print("unknown command: ");
                term_print(args[0]);
                term_print("\n");
            }
        }
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    let _ = libr::print_string(b"[shell] panic\n");
    libr::exit(1)
}
