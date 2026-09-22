use super::*;

// ── Redirect: applicazione (Fase 40.4) ─────────────────────────────────
// Il parsing (quote-aware, Fase 41) vive in `parser.rs`: qui solo l'apertura
// e la chiusura degli fd. `open_all` apre TUTTI i target IN ORDINE (bash-like:
// a pari slot vince l'ULTIMO, i precedenti risultano comunque creati/troncati;
// `2>&1` aliasa lo slot 2 sull'fd corrente dello slot 1, zero open). `<` apre
// read-only (file deve esistere).

pub(crate) struct Redir {
    pub(crate) slot: u8, // 0 = stdin, 1 = stdout, 2 = stderr
    pub(crate) append: bool,
    pub(crate) dup_to_1: bool, // true solo per `2>&1` (alias, nessun target)
    pub(crate) target: String,
}

/// Chiude l'fd dello slot se non condiviso con altri slot (alias `2>&1`).
fn drop_slot(fds: &mut [i64; 3], slot: usize) {
    let fd = fds[slot];
    if fd >= 0 {
        fds[slot] = -1;
        if !fds.contains(&fd) {
            let _ = libr::close(fd);
        }
    }
}

/// Apre tutti i target IN ORDINE e ritorna gli fd per slot [stdin, stdout,
/// stderr] (-1 = assente). `2>&1` aliasa fds[2] = fds[1] del momento (come
/// bash: `> /o 2>&1` manda stderr nel file, `2>&1 > /o` lo lascia al
/// terminale). A fallimento chiude tutto e ritorna (target, errore): il
/// chiamante riporta sul terminale, mai nel file.
pub(crate) fn open_all(redirs: &[Redir]) -> Result<[i64; 3], (String, libr::Error)> {
    let mut fds = [-1i64; 3];
    for r in redirs {
        if r.dup_to_1 {
            drop_slot(&mut fds, 2);
            fds[2] = fds[1];
            continue;
        }
        let (slot, flags) = match r.slot {
            0 => (0usize, 0),
            1 => (
                1usize,
                libr::O_CREAT | if r.append { libr::O_APPEND } else { libr::O_TRUNC },
            ),
            _ => (
                2usize,
                libr::O_CREAT | if r.append { libr::O_APPEND } else { libr::O_TRUNC },
            ),
        };
        let path = cwd::resolve(r.target.as_str());
        match libr::open(&path, flags) {
            Ok(fd) => {
                drop_slot(&mut fds, slot);
                fds[slot] = fd;
            }
            Err(e) => {
                drop_slot(&mut fds, 0);
                drop_slot(&mut fds, 1);
                drop_slot(&mut fds, 2);
                return Err((r.target.clone(), e));
            }
        }
    }
    Ok(fds)
}

/// Chiude gli fd distinti (alias chiuso una volta sola).
pub(crate) fn close_all(fds: [i64; 3]) {
    let mut seen = [-1i64; 3];
    let mut n = 0usize;
    for &fd in &fds {
        if fd >= 0 && !seen[..n].contains(&fd) {
            seen[n] = fd;
            n += 1;
            let _ = libr::close(fd);
        }
    }
}

/// Frase d'errore per open di redirect fallita (sul terminale, mai nel file).
/// Distingue i codici di dominio Fase 40 (`NotFound` vs `ReadOnly`).
pub(crate) fn report_open_error(target: &str, e: libr::Error) {
    term::term_print("redirect: cannot open ");
    term::term_print(target);
    match e {
        libr::Error::NotFound => term::term_print(": no such file or directory\n"),
        libr::Error::ReadOnly => term::term_print(": read-only file system\n"),
        libr::Error::IsDir => term::term_print(": is a directory\n"),
        _ => term::term_print(": failed\n"),
    }
}
