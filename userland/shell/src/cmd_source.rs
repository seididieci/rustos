use super::*;
use super::repl::LineSrc;

// ── `source <file>` (builtin permanente, anticipa Fase 43) ───────────
// Esegue uno script riga-per-riga riusando parse+exec del REPL: heredoc,
// `; && ||`, pipe, redirect, `$VAR/$?`, glob e `run` funzionano come da
// tastiera. Pensato per i test veloci (1 riga digitata invece di N comandi),
// resta come feature utente (script `.sh` della Fase 43).
//
// Semantica:
// - `$?` iniziale = quello esterno; l'exit dello script = ultimo comando.
// - `exit [code]` termina lo script (mai la shell).
// - Esecuzione silenziosa: le righe NON sono ecoeggiate (l'eco tastiera non
//   va sul seriale comunque; cosi' gli assert di assenza restano validi).
// - File vuoto = no-op riuscita; directory/device = errore.
// - Annidamento max 4 (anti-loop); `source` in pipeline gira nel figlio
//   (effetti scoped, `$?` iniziale 0).
// Limiti onesti:
// - Redirect esterno + redirect interni annidati: il restore interno
//   (`clear_stdio`) cancella anche quello esterno (no stack stdio in libr).
//   Usare l'uno o gli altri, non entrambi.
// - `&` su pipeline resta Fase 44 anche in script.

/// Profondita' di annidamento corrente (single-threaded, come VARS/JOBS).
static mut SOURCE_DEPTH: u8 = 0;

/// Annidamento massimo (lo script di livello 5 e' rifiutato).
const SOURCE_MAX_DEPTH: u8 = 4;

pub(crate) fn cmd_source(args: &[&str], status: i64) -> i64 {
    if args.len() < 2 {
        term::term_err("source: usage: source <file>\n");
        return 1;
    }
    let depth = unsafe { SOURCE_DEPTH };
    if depth >= SOURCE_MAX_DEPTH {
        term::term_err("source: nesting too deep\n");
        return 1;
    }
    let path = cwd::resolve(args[1]);
    // Sonda il tipo: directory/device non si eseguono; file vuoto = no-op.
    let mut st = libr::Stat {
        size: 0,
        kind: 0,
        readonly: false,
    };
    match libr::stat(&path, &mut st) {
        Ok(()) if st.is_file() && st.size == 0 => return 0,
        Ok(()) if !st.is_file() => {
            term::term_err("source: cannot load ");
            term::term_err(args[1]);
            term::term_err("\n");
            return 1;
        }
        Ok(()) => {}
        Err(_) => {
            term::term_err("source: cannot load ");
            term::term_err(args[1]);
            term::term_err("\n");
            return 1;
        }
    }
    let bytes = match libr::load_file(&path) {
        Some(b) => b,
        None => {
            term::term_err("source: cannot load ");
            term::term_err(args[1]);
            term::term_err("\n");
            return 1;
        }
    };
    let text = match core::str::from_utf8(&bytes) {
        Ok(s) => s,
        Err(_) => {
            term::term_err("source: not text\n");
            return 1;
        }
    };
    unsafe {
        SOURCE_DEPTH = depth + 1;
    }
    // Righe logiche e corpi heredoc condividono il cursore di `src`.
    let mut src = repl::ScriptSrc::new(text);
    let mut cur = status;
    while let Some(line) = src.next_line("") {
        match repl::run_one_line(&line, cur, &mut src, true) {
            None => {}
            Some(repl::LineOutcome::Continue(s)) => cur = s,
            Some(repl::LineOutcome::Exit(c)) => {
                cur = c;
                break;
            }
        }
    }
    unsafe {
        SOURCE_DEPTH = depth;
    }
    cur
}
