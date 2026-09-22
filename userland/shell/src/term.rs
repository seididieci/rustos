use super::*;

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

pub(crate) fn term_init() -> bool {
    // Il mount /dev/input viene registrato da usertty al suo avvio:
    // ritenta se l'open iniziale fallisce (race di boot).
    for _ in 0..100 {
        if let Ok(fd) = libr::open(KEYBOARD_PATH, 0) {
            unsafe {
                TERM_FD = fd;
            }
            return true;
        }
        spin_brief();
    }
    false
}
/// Scrive byte sul terminale: userfs li inoltra al console server che li
/// disegna sulla VGA (DEV_WRITE). Echo dei tasti gestito dal console.
/// Mirror su seriale per debugging e per test automatici.
/// Hook B1 (Fase 40.4b): con stdout redirectato (`set_stdio`) i builtin
/// scrivono sul file invece che sul terminale — e' l'unico punto d'aggancio
/// (i builtin usano tutti `term_print`, non `println!`). Il mirror seriale
/// resta sempre (l'output debug non si perde mai).
pub(crate) fn term_write_bytes(data: &[u8]) {
    let _ = libr::print_string(data);
    let out = libr::stdout_fd();
    if out >= 0 {
        let _ = libr::write_fs(out, data, data.len());
    } else {
        unsafe {
            let _ = libr::write_fs(TERM_FD, data, data.len());
        }
    }
}

pub(crate) fn term_print(s: &str) {
    term_write_bytes(s.as_bytes());
}

/// Scrive un messaggio d'errore: stderr redirectato (`2>`, Fase 40.4d) o
/// terminale — MAI lo stdout redirectato (gli errori non devono inquinare il
/// file di `>`). Mirror su seriale come l'output normale.
pub(crate) fn term_err_bytes(data: &[u8]) {
    let _ = libr::print_string(data);
    let err = libr::stderr_fd();
    if err >= 0 {
        let _ = libr::write_fs(err, data, data.len());
    } else {
        unsafe {
            let _ = libr::write_fs(TERM_FD, data, data.len());
        }
    }
}

pub(crate) fn term_err(s: &str) {
    term_err_bytes(s.as_bytes());
}

/// Legge TUTTO lo stdin redirectato (`<`, Fase 40.4d) fino a EOF. Vuoto =
/// file vuoto; `None` interno (errore/byte mancante) chiude comunque come EOF
/// (file, mai device-a-caratteri qui: `<` su device puo' troncare — limite
/// documentato, come `cat /dev/zero` da file che non termina mai).
/// Chiamare solo con stdin redirectato (`stdin_fd() >= 0`).
pub(crate) fn term_read_stdin() -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        match libr::stdin_byte() {
            Some(b) => out.push(b),
            None => break,
        }
    }
    out
}

/// Legge un byte di input dal terminale. Quando il buffer e' vuoto attende un
/// breve spin (IF=1) prima di riprovare: niente busy-loop su syscall.
fn kbd_read_byte() -> Option<u8> {
    let mut buf = [0u8; 1];
    loop {
        let n = unsafe { libr::read_fs(TERM_FD, &mut buf, 1).unwrap_or(0) };
        if n > 0 {
            return Some(buf[0]);
        }
        spin_brief();
    }
}
// ── Line editing ────────────────────────────────────────────────────

/// Legge una riga. Il console server fa gia' l'echo: qui costruiamo solo la
/// stringa logica (backspace = pop, Enter = fine riga).
pub(crate) fn read_line(prompt: &str) -> String {
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
