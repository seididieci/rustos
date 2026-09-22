use super::*;
use crate::*;

// ── Tabelle stdio vfd (Fase 40.2, P1) ─────────────────────────────────
// Redirect `>`/`>>`/`<`/`2>`/`2>&1`: quando attive, lo stdout dei programmi
// (`println!`/`print_str!`, via `print::flush`) va sul file invece che sul
// seriale, e stdin si legge dal file invece che dalla tastiera.
//
// Separazione dei sink (decisione 40.2): `print_string`/`write_stdout`/
// `sys::write(1,…)` restano SEMPRE seriale (sink debug: mirror shell, log
// init/server). Solo il percorso line-buffer e' instradabile — e' quello che
// usano i programmi reali. Con tabelle inattive (default) tutto e' seriale:
// zero behavior change per i server esistenti.
//
// Slot 2 (`2>`/`2>&1`) e' memorizzato ma non hookato in 40.2: niente in
// userland scrive fd 2 oggi; l'aggancio dei builtin viene con la shell 40.4
// (i builtin usano `term_print`, non `println!`).

/// fd reali di stdin/stdout/stderr quando il redirect e' attivo (-1 = seriale).
static STDIO: [AtomicI64; 3] =
    [AtomicI64::new(-1), AtomicI64::new(-1), AtomicI64::new(-1)];

/// Attiva il redirect: `fds` = fd reali di [stdin, stdout, stderr] (-1 per
/// slot = quello slot resta seriale). Chiamata dalla shell (builtin: temporaneo
/// con restore; figli `run`: prima dell'exec, ereditata via fork-COW).
pub fn set_stdio(fds: [i64; 3]) {
    STDIO[0].store(fds[0], Ordering::Relaxed);
    STDIO[1].store(fds[1], Ordering::Relaxed);
    STDIO[2].store(fds[2], Ordering::Relaxed);
}

/// Disattiva il redirect: tutto torna seriale (cleanup builtin/figli).
pub fn clear_stdio() {
    set_stdio([-1, -1, -1]);
}

/// True se almeno uno slot e' redirectato.
pub fn stdio_active() -> bool {
    STDIO[0].load(Ordering::Relaxed) >= 0
        || STDIO[1].load(Ordering::Relaxed) >= 0
        || STDIO[2].load(Ordering::Relaxed) >= 0
}

/// Instrada `bytes` (stdout) sul file redirectato. Ritorna true se "gestito":
/// tutto sul file, o prefisso sul file + coda sul seriale a write parziale
/// (mai duplicazione). False = non attivo o fallito del tutto: il chiamante
/// riversa tutto sul seriale (l'output debug non si perde mai).
/// Mai ricorsivo: `write_fs` non stampa (solo IPC+ring), il fallback e'
/// `sys::write` raw.
pub(crate) fn route_out(bytes: &[u8]) -> bool {
    let fd = STDIO[1].load(Ordering::Relaxed);
    if fd < 0 {
        return false;
    }
    match write_fs(fd, bytes, bytes.len()) {
        Ok(n) if n == bytes.len() => true,
        Ok(n) => {
            // Parziale (errore dopo progressi): la coda va sul seriale.
            let _ = sys::write(1, bytes.as_ptr().wrapping_add(n), bytes.len() - n);
            true
        }
        Err(_) => false,
    }
}

/// Legge un byte da stdin redirectato. None = non attivo, vuoto o errore.
/// NESSUNA semantica retry/EOF qui: a 0 byte il chiamante decide — la shell
/// sa se stdin e' un device (retry con spin, come la tastiera) o un file
/// (None = EOF). Condivide il guard 1-in-volo degli altri wrapper (via
/// `read_fs`: a op in volo → `Pending` → None, mai disallineamento ring).
pub fn stdin_byte() -> Option<u8> {
    let fd = STDIO[0].load(Ordering::Relaxed);
    if fd < 0 {
        return None;
    }
    let mut b = [0u8; 1];
    match read_fs(fd, &mut b, 1) {
        Ok(1) => Some(b[0]),
        _ => None,
    }
}
