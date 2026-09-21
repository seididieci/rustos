use super::*;

pub(crate) fn cmd_help() {
    term::term_print("Commands: ls [-l] [path], cat <file>, touch <file>, mkdir <dir>, mount <src> <tgt>, umount <tgt>, echo [args], clear, wc <file>, hexdump <file>, kill <pid|service>, cd [dir], pwd, cp <src> <dst>, mv <src> <dst>, rm <file>, rmdir <dir>, ps, run <path> [args...] [&], jobs, wait [pid], exit, help\n");
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
pub(crate) fn cmd_ps() {
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
    term::term_print(&out);
}

// ── Utility Fase 18.1 ───────────────────────────────────────────────

pub(crate) fn cmd_echo(args: &[&str]) {
    // Una sola write per riga: ogni term_print e' un IPC + una riga di
    // seriale col timestamp — i pezzi non sarebbero mai contigui nel log.
    let mut s = String::new();
    for (i, a) in args.iter().skip(1).enumerate() {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(a);
    }
    term::term_print(&s);
    term::term_print("\n");
}

pub(crate) fn cmd_clear() {
    // Form feed: la console pulisce tutto e torna home (Fase 18.1).
    term::term_write_bytes(b"\x0c");
}
/// Accoda un u64 in decimale (niente `format!`: no_std minimale).
pub(crate) fn push_u64(s: &mut String, mut v: u64) {
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

pub(crate) fn cmd_wc(args: &[&str]) {
    if args.len() < 2 {
        term::term_print("wc: missing file\n");
        return;
    }
    let path = cwd::resolve(args[1]);
    let fd = libr::open(&path, 0);
    if fd < 0 {
        term::term_print("wc: cannot open ");
        term::term_print(args[1]);
        term::term_print("\n");
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
    term::term_print(&s);
    term::term_print("\n");
}

fn hex_of(nib: u8) -> u8 {
    b"0123456789abcdef"[(nib & 0x0f) as usize]
}
fn push_hex_byte(s: &mut String, b: u8) {
    s.push(hex_of(b >> 4) as char);
    s.push(hex_of(b) as char);
}

pub(crate) fn cmd_hexdump(args: &[&str]) {
    if args.len() < 2 {
        term::term_print("hexdump: missing file\n");
        return;
    }
    let path = cwd::resolve(args[1]);
    let fd = libr::open(&path, 0);
    if fd < 0 {
        term::term_print("hexdump: cannot open ");
        term::term_print(args[1]);
        term::term_print("\n");
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
        term::term_print(&s);
        term::term_print("\n");
        off += n as usize;
    }
    libr::close(fd);
}

/// Parsa un intero decimale (usato anche da `wait` in cmd_run).
pub(crate) fn parse_i64(s: &str) -> Option<i64> {
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
pub(crate) fn cmd_kill(args: &[&str]) {
    if args.len() < 2 {
        term::term_print("kill: usage: kill <pid|service>\n");
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
                    term::term_print("kill: service not running\n");
                    return;
                }
            },
            None => {
                term::term_print("kill: unknown pid/service\n");
                return;
            }
        },
    };
    if libr::kill(pid, 1).is_err() {
        term::term_print("kill: failed (parent/init only, or init/self/unknown?)\n");
    }
}
