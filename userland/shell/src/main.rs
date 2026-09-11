//! usershell — Shell interattiva per Velordor (Fase 9.4).
//!
//! Client del terminale: NON mappa il VGA. Apre `/dev/input/keyboard` e usa lo
//! stesso fd per leggere i tasti (read) e per scrivere l'output (write): il
//! console server possiede la VGA, disegna l'output e fa l'echo dei tasti
//! (Opzione B). La shell gestisce solo la linea logica dei comandi.
//! Comandi: ls, cat, touch, mkdir, exit, help.

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
    let path = if args.len() > 1 { args[1] } else { "/" };
    let mut buf = vec![0u8; 4096];
    let n = libr::readdir(path, &mut buf, 4096);
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
            term_print(name);
            term_print("  ");
            wrote = true;
        }
        i += 1; // skip null
    }
    if wrote {
        term_print("\n");
    }
}

fn cmd_cat(args: &[&str]) {
    if args.len() < 2 {
        term_print("cat: missing file\n");
        return;
    }
    let fd = libr::open(args[1], 0);
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
    let fd = libr::open(args[1], 0x200 /* O_CREAT */);
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
    let r = libr::mkdir(args[1]);
    if r < 0 {
        term_print("mkdir: failed\n");
    }
}

fn cmd_help() {
    term_print("Commands: ls [path], cat <file>, touch <file>, mkdir <dir>, exit, help\n");
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

    // REPL
    loop {
        let line = read_line("$ ");
        let trimmed = line.trim();
        if trimmed.is_empty() { continue; }

        let args: Vec<&str> = trimmed.split_whitespace().collect();
        match args[0] {
            "ls" => cmd_ls(&args),
            "cat" => cmd_cat(&args),
            "touch" => cmd_touch(&args),
            "mkdir" => cmd_mkdir(&args),
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
