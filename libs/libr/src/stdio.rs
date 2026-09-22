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

/// fd reale dello stdout redirectato (-1 = seriale/terminale).
/// Usato dalla shell (hook B1 in `term_write_bytes`, Fase 40.4b): i builtin
/// scrivono su terminale via `TERM_FD`, non via `println!`, quindi il routing
/// di `route_out` non li copre — la shell instrada esplicitamente qui.
pub fn stdout_fd() -> i64 {
    STDIO[1].load(Ordering::Relaxed)
}

/// Magic della spec redirect contrabbandata nell'ultimo argv (Fase 40.4c):
/// `\x7fVELORDOR_REDIR\x1f` + voci `vfd:noncehex` separate da `;` (una per slot,
/// max 3). Hex minuscolo: niente byte NUL (il kernel spezza le stringhe argv
/// al primo NUL e rifiuta code extra — `exec.rs::parse_args`). Il primo byte
/// 0x7f e' impossibile da digitare (`read_line` accetta 0x20..=0x7e): zero
/// collisioni con argomenti veri.
pub(crate) const REDIR_MAGIC: &[u8] = b"\x7fVELORDOR_REDIR\x1f";

/// Max voci nella spec (un vfd per slot: stdin/stdout/stderr).
pub(crate) const REDIR_MAX_ENTRIES: usize = 3;

/// Parsa le voci dopo `REDIR_MAGIC`: `out[i] = (vfd, nonce)`. `None` a
/// qualunque deviazione (vfd fuori 0..2, hex malformato, troppe voci, coda).
/// Strict: una spec malformata si ignora intera (fail-open loud), mai a meta'.
pub(crate) fn parse_redir_entries(arg: &[u8], out: &mut [(u8, u64); 3]) -> Option<usize> {
    let body = arg.strip_prefix(REDIR_MAGIC)?;
    if body.is_empty() {
        return None;
    }
    let mut n = 0usize;
    let mut i = 0usize;
    while i < body.len() {
        if n >= REDIR_MAX_ENTRIES {
            return None;
        }
        // vfd: un char '0'..'2', poi ':'.
        if i + 1 >= body.len() {
            return None;
        }
        let vfd = body[i];
        if !(b'0'..=b'2').contains(&vfd) || body[i + 1] != b':' {
            return None;
        }
        i += 2;
        // nonce: 1..16 nibble hex minuscoli, poi ';' o fine.
        let start = i;
        while i < body.len() && body[i] != b';' {
            if !body[i].is_ascii_hexdigit() || body[i].is_ascii_uppercase() {
                return None;
            }
            i += 1;
        }
        let digits = i - start;
        if digits == 0 || digits > 16 {
            return None;
        }
        let mut nonce: u64 = 0;
        for &d in &body[start..i] {
            let v = match d {
                b'0'..=b'9' => (d - b'0') as u64,
                b'a'..=b'f' => (d - b'a' + 10) as u64,
                _ => return None,
            };
            nonce = (nonce << 4) | v;
        }
        out[n] = (vfd - b'0', nonce);
        n += 1;
        if i < body.len() {
            i += 1; // skip ';'
        }
    }
    if n == 0 { None } else { Some(n) }
}

/// Ripristina i redirect all'avvio da una spec contrabbandata nell'ultimo argv
/// (Fase 40.4c). Chiamata dal wrapper `entry!` prima di `real_main`: senza
/// magic nessuno effetto (costo = una scansione dello stack); con magic fa
/// claim dei grant + `set_stdio`. A claim fallita chiude i gia' riscossi e
/// prosegue SENZA redirect (fail-open loud su seriale: l'output resta visibile,
/// mai perso in silenzio). Mai fatale: lo startup non deve morire per un
/// redirect (il programma gira, solo non redirectato).
/// CRT-internal (pub perche' l'espansione `entry!` vive nei crate utenti).
pub fn stdio_restore(sp: u64) {
    let Some(arg) = crate::args::redir_spec_arg(sp) else {
        return;
    };
    let mut entries = [(0u8, 0u64); 3];
    let Some(n) = parse_redir_entries(arg, &mut entries) else {
        let _ = crate::print_string(b"[libr] stdio_restore: spec malformata, ignoro\n");
        return;
    };
    let mut fds = [-1i64; 3];
    for &(vfd, nonce) in &entries[..n] {
        match crate::dup_claim(nonce) {
            Ok(fd) => fds[vfd as usize] = fd,
            Err(_) => {
                for &f in &fds {
                    if f >= 0 {
                        let _ = crate::close(f);
                    }
                }
                let _ = crate::print_string(b"[libr] stdio_restore: claim fallita, senza redirect\n");
                return;
            }
        }
    }
    set_stdio(fds);
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
