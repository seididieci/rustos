use super::*;

// ── Parser riga di comando (Fase 41) ───────────────────────────────────
// Quote-aware (bash-like, subset): '...' letterali, "..." con $, \ escape,
// # commenti, ; && || & (background), redirect 40.4 (solo fuori quote),
// $VAR ${VAR} $? $$, ~. Pipe singola rifiutata (Fase 42).
// Produce comandi gia' espansi (tilde + variabili + glob + quote-removal):
// il REPL esegue in sequenza con short-circuit su status.

// Flag per char (quote/backslash consumati in tokenize, mai nell'output).
const Q_PLAIN: u8 = 0; // digitato diretto, non quotato
const Q_SINGLE: u8 = 1; // da '...' (mai espansione/glob/split)
const Q_DOUBLE: u8 = 2; // da "..." (solo $)
const Q_ESC: u8 = 4; // escapato con \ (mai speciale)
const Q_EXP: u8 = 5; // da espansione NON quotata (split + glob si)
const Q_EXPQ: u8 = 6; // da espansione in "..." (mai split/glob)

#[derive(Clone, Copy)]
struct Ch {
    b: u8,
    q: u8,
}

pub(crate) enum Conn {
    Seq,
    And,
    Or,
}

pub(crate) struct Command {
    pub(crate) argv: Vec<String>,
    pub(crate) redirs: Vec<redirect::Redir>,
    pub(crate) bg: bool,
    /// Assegnazione persistente (`NAME=valore`, senza comando): applicata
    /// dall'esecutore dopo gli effetti dei redirect.
    pub(crate) assign: Option<(String, String)>,
}

pub(crate) struct Seq {
    pub(crate) cmds: Vec<Command>,
    pub(crate) cons: Vec<Conn>,
}

pub(crate) enum ParseError {
    MissingTarget(&'static str),
    PipeUnsupported,
    BadSubst,
    EnvPrefix,
}

// ── Variabili shell (solo client-side, mai nel kernel/FS) ─────────────

static mut VARS: Vec<(String, String)> = Vec::new();

fn vars() -> &'static mut Vec<(String, String)> {
    // Come cwd.rs/JOBS: raw pointer, single-threaded.
    unsafe { &mut *core::ptr::addr_of_mut!(VARS) }
}

/// Nome variabile valido: [A-Za-z_][A-Za-z0-9_]* (non vuoto).
pub(crate) fn valid_name(s: &str) -> bool {
    let b = s.as_bytes();
    if b.is_empty() || !(b[0].is_ascii_alphabetic() || b[0] == b'_') {
        return false;
    }
    b[1..].iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_')
}

pub(crate) fn vars_set(name: &str, val: &str) {
    for (k, v) in vars().iter_mut() {
        if k == name {
            *v = String::from(val);
            return;
        }
    }
    vars().push((String::from(name), String::from(val)));
}

pub(crate) fn vars_get(name: &str) -> Option<String> {
    vars().iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
}

pub(crate) fn vars_list() -> Vec<(String, String)> {
    vars().clone()
}

// ── Tokenizer ─────────────────────────────────────────────────────────

enum Piece {
    Word(Vec<Ch>),
    Op(Op),
}

enum Op {
    Gt,
    GtGt,
    Lt,
    E2Gt,
    E2GtGt,
    Dup21,
    Semi,
    And,
    Or,
    Bg,
}

/// Tokenizza la riga in parole (con flag di quoting) e operatori. Le quote e
/// i backslash sono consumati qui (mai nei token). `#` non quotato a inizio
/// parola = commento (resto riga scartato). `|` singolo = errore (Fase 42).
/// Virgolette non chiuse = resto riga letterale (documentato, niente
/// continuazione: `read_line` e' single-line).
fn tokenize(s: &str) -> Result<Vec<Piece>, ParseError> {
    let b = s.as_bytes();
    let mut out: Vec<Piece> = Vec::new();
    let mut word: Vec<Ch> = Vec::new();
    let mut at_start = true;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        // Commento: # non quotato a inizio parola.
        if c == b'#' && at_start {
            break;
        }
        if c.is_ascii_whitespace() {
            if !word.is_empty() {
                out.push(Piece::Word(core::mem::take(&mut word)));
            }
            at_start = true;
            i += 1;
            continue;
        }
        // Virgoletta singola: tutto letterale fino alla chiusura.
        if c == b'\'' {
            i += 1;
            while i < b.len() && b[i] != b'\'' {
                word.push(Ch { b: b[i], q: Q_SINGLE });
                i += 1;
            }
            if i < b.len() {
                i += 1; // chiusura
            }
            at_start = false;
            continue;
        }
        // Virgoletta doppia: letterale, ma \ solo prima di $ " \.
        // `\$` e' Q_ESC (mai riespanso: senno' `$B` diventerebbe variabile);
        // `\"`/`\\` restano Q_DOUBLE (char inerti, mai trigger di espansione).
        if c == b'"' {
            i += 1;
            while i < b.len() && b[i] != b'"' {
                if b[i] == b'\\' && i + 1 < b.len() && matches!(b[i + 1], b'$' | b'"' | b'\\') {
                    let q = if b[i + 1] == b'$' { Q_ESC } else { Q_DOUBLE };
                    word.push(Ch { b: b[i + 1], q });
                    i += 2;
                } else {
                    word.push(Ch { b: b[i], q: Q_DOUBLE });
                    i += 1;
                }
            }
            if i < b.len() {
                i += 1; // chiusura
            }
            at_start = false;
            continue;
        }
        // Escape fuori quote: prossimo char letterale (qualunque sia).
        if c == b'\\' {
            if i + 1 < b.len() {
                word.push(Ch { b: b[i + 1], q: Q_ESC });
                i += 2;
            } else {
                i += 1; // \ finale: continuazione impossibile, scartato
            }
            at_start = false;
            continue;
        }
        // Operatori (sempre non quotati qui).
        if c == b'>' {
            // Prefisso 2> / 2>> / 2>&1: parola corrente == "2" plain.
            if word.len() == 1 && word[0].b == b'2' && word[0].q == Q_PLAIN {
                word.clear();
                if i + 1 < b.len() && b[i + 1] == b'>' {
                    out.push(Piece::Op(Op::E2GtGt));
                    i += 2;
                } else if i + 2 < b.len() && b[i + 1] == b'&' && b[i + 2] == b'1' {
                    out.push(Piece::Op(Op::Dup21));
                    i += 3;
                } else {
                    out.push(Piece::Op(Op::E2Gt));
                    i += 1;
                }
            } else {
                if !word.is_empty() {
                    out.push(Piece::Word(core::mem::take(&mut word)));
                }
                if i + 1 < b.len() && b[i + 1] == b'>' {
                    out.push(Piece::Op(Op::GtGt));
                    i += 2;
                } else {
                    out.push(Piece::Op(Op::Gt));
                    i += 1;
                }
            }
            at_start = true;
            continue;
        }
        if c == b'<' {
            if !word.is_empty() {
                out.push(Piece::Word(core::mem::take(&mut word)));
            }
            out.push(Piece::Op(Op::Lt));
            i += 1;
            at_start = true;
            continue;
        }
        if c == b';' {
            if !word.is_empty() {
                out.push(Piece::Word(core::mem::take(&mut word)));
            }
            out.push(Piece::Op(Op::Semi));
            i += 1;
            at_start = true;
            continue;
        }
        if c == b'&' {
            if !word.is_empty() {
                out.push(Piece::Word(core::mem::take(&mut word)));
            }
            if i + 1 < b.len() && b[i + 1] == b'&' {
                out.push(Piece::Op(Op::And));
                i += 2;
            } else {
                out.push(Piece::Op(Op::Bg));
                i += 1;
            }
            at_start = true;
            continue;
        }
        if c == b'|' {
            if !word.is_empty() {
                out.push(Piece::Word(core::mem::take(&mut word)));
            }
            if i + 1 < b.len() && b[i + 1] == b'|' {
                out.push(Piece::Op(Op::Or));
                i += 2;
                at_start = true;
                continue;
            }
            return Err(ParseError::PipeUnsupported);
        }
        word.push(Ch { b: c, q: Q_PLAIN });
        at_start = false;
        i += 1;
    }
    if !word.is_empty() {
        out.push(Piece::Word(word));
    }
    Ok(out)
}

// ── Espansioni ────────────────────────────────────────────────────────

/// Riconosce `NAME=...` con = non quotato: ritorna (nome, valore-grezzo).
/// Il valore si espande senza split/glob (bash).
fn split_assign(w: &[Ch]) -> Option<(String, Vec<Ch>)> {
    let mut j = 0;
    while j < w.len() && !(w[j].b == b'=' && w[j].q == Q_PLAIN) {
        j += 1;
    }
    if j >= w.len() {
        return None;
    }
    let name: Vec<u8> = w[..j].iter().map(|c| c.b).collect();
    let name = String::from(core::str::from_utf8(&name).unwrap_or(""));
    if !valid_name(&name) {
        return None;
    }
    Some((name, w[j + 1..].to_vec()))
}

/// Espande tilde (solo `~`/`~/` a inizio parola, non quotato) + variabili
/// (`$V`, `${V}`, `$?`, `$$`; unset = stringa vuota). Se `split`, i risultati
/// di espansioni non quotate si spezzano su whitespace (una parola → N,
/// pezzi vuoti scartati come bash).
fn expand_word(w: &[Ch], status: i64, split: bool) -> Result<Vec<Vec<Ch>>, ParseError> {
    let mut v: Vec<Ch> = Vec::new();
    let mut k = 0;
    if !w.is_empty() && w[0].b == b'~' && w[0].q == Q_PLAIN {
        v.push(Ch { b: b'/', q: Q_PLAIN });
        k = 1;
    }
    let pid = libr::getpid();
    let mut status_buf = [0u8; 20];
    let mut pid_buf = [0u8; 20];
    while k < w.len() {
        let c = &w[k];
        if !(c.b == b'$' && (c.q == Q_PLAIN || c.q == Q_DOUBLE)) {
            v.push(Ch { b: c.b, q: c.q });
            k += 1;
            continue;
        }
        let qq = if c.q == Q_DOUBLE { Q_EXPQ } else { Q_EXP };
        if k + 1 < w.len() && w[k + 1].b == b'{' {
            let mut j = k + 2;
            while j < w.len() && w[j].b != b'}' {
                j += 1;
            }
            if j >= w.len() {
                return Err(ParseError::BadSubst);
            }
            let name: Vec<u8> = w[k + 2..j].iter().map(|c| c.b).collect();
            let name = core::str::from_utf8(&name).unwrap_or("");
            if !valid_name(name) {
                return Err(ParseError::BadSubst);
            }
            push_value(&mut v, vars_get(name).unwrap_or_default().as_bytes(), qq);
            k = j + 1;
            continue;
        }
        if k + 1 < w.len() && w[k + 1].b == b'?' {
            push_value(&mut v, fmt_u64(&mut status_buf, status as u64), qq);
            k += 2;
            continue;
        }
        if k + 1 < w.len() && w[k + 1].b == b'$' {
            push_value(&mut v, fmt_u64(&mut pid_buf, pid as u64), qq);
            k += 2;
            continue;
        }
        if k + 1 < w.len() && (w[k + 1].b.is_ascii_alphabetic() || w[k + 1].b == b'_') {
            let mut j = k + 2;
            while j < w.len() && (w[j].b.is_ascii_alphanumeric() || w[j].b == b'_') {
                j += 1;
            }
            let name: Vec<u8> = w[k + 1..j].iter().map(|c| c.b).collect();
            let name = core::str::from_utf8(&name).unwrap_or("");
            push_value(&mut v, vars_get(name).unwrap_or_default().as_bytes(), qq);
            k = j;
            continue;
        }
        // $ seguito da altro (spazio, fine, cifra...): $ letterale.
        v.push(Ch { b: b'$', q: c.q });
        k += 1;
    }
    if !split {
        let mut one: Vec<Vec<Ch>> = Vec::new();
        one.push(v);
        return Ok(one);
    }
    let mut words: Vec<Vec<Ch>> = Vec::new();
    let mut cur: Vec<Ch> = Vec::new();
    for c in v {
        if c.b.is_ascii_whitespace() && c.q == Q_EXP {
            if !cur.is_empty() {
                words.push(core::mem::take(&mut cur));
            }
            continue;
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    Ok(words)
}

fn push_value(v: &mut Vec<Ch>, bytes: &[u8], q: u8) {
    for &b in bytes {
        v.push(Ch { b, q });
    }
}

/// Decimale in buffer stack (niente format!/alloc): ritorna la slice.
fn fmt_u64(buf: &mut [u8; 20], mut n: u64) -> &[u8] {
    if n == 0 {
        buf[19] = b'0';
        return &buf[19..20];
    }
    let mut i = 20;
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    let (head, tail) = buf.split_at_mut(i);
    let _ = head;
    &tail[..20 - i]
}

/// Quote-removal: le quote sono gia' consumate in tokenize (mai nei Ch);
/// resta solo da prendere i byte (input gia' UTF-8 da `read_line`).
fn word_string(w: &[Ch]) -> String {
    let bytes: Vec<u8> = w.iter().map(|c| c.b).collect();
    String::from_utf8(bytes).unwrap_or_default()
}

// ── Glob ────────────────────────────────────────────────────────────

/// Matcher `*` (qualunque sequenza) e `?` (un byte). Byte-wise (nomi ASCII
/// nei test; documentato). `*` non attraversa `/` (i nomi non ne hanno).
fn pat_match(pat: &[u8], name: &[u8]) -> bool {
    if pat.is_empty() {
        return name.is_empty();
    }
    if pat[0] == b'*' {
        let mut pi = 1;
        while pi < pat.len() && pat[pi] == b'*' {
            pi += 1;
        }
        if pi == pat.len() {
            return true;
        }
        let mut ni = 0;
        while ni <= name.len() {
            if pat_match(&pat[pi..], &name[ni..]) {
                return true;
            }
            ni += 1;
        }
        return false;
    }
    if name.is_empty() {
        return false;
    }
    if pat[0] == b'?' || pat[0] == name[0] {
        return pat_match(&pat[1..], &name[1..]);
    }
    false
}

/// Espande un pattern glob (trigger `*`/`?` non quotati e non da espansione
/// quotata): match ordinati byte-wise, no-match = letterale (bash default),
/// dotfile solo se il pattern inizia per `.`. Directory da readdir (stesso
/// formato di `cmd_ls`); a errore = letterale.
fn glob_word(w: &[Ch]) -> Vec<String> {
    let bytes: Vec<u8> = w.iter().map(|c| c.b).collect();
    let trigger = w
        .iter()
        .any(|c| (c.q == Q_PLAIN || c.q == Q_EXP) && (c.b == b'*' || c.b == b'?'));
    let s = String::from_utf8(bytes.clone()).unwrap_or_default();
    if !trigger {
        let mut one: Vec<String> = Vec::new();
        one.push(s);
        return one;
    }
    // Parte dir (come digitata, o cwd) + pattern (ultimo componente).
    let (dir_typed, pat) = match bytes.iter().rposition(|&b| b == b'/') {
        Some(p) => (&bytes[..p], &bytes[p + 1..]),
        None => (&[][..], &bytes[..]),
    };
    let dir_typed = core::str::from_utf8(dir_typed).unwrap_or("");
    let dir_abs = if dir_typed.is_empty() {
        cwd::cwd_get()
    } else {
        cwd::resolve(dir_typed)
    };
    let mut buf = alloc::vec![0u8; 4096];
    if libr::readdir(&dir_abs, &mut buf, 4096).is_err() {
        let mut one: Vec<String> = Vec::new();
        one.push(s);
        return one;
    }
    let dot_pat = pat.first() == Some(&b'.');
    let mut hits: Vec<Vec<u8>> = Vec::new();
    let mut i = 0;
    while i < buf.len() {
        if buf[i] == 0 {
            break;
        }
        let start = i;
        while i < buf.len() && buf[i] != 0 {
            i += 1;
        }
        let name = &buf[start..i];
        i += 1; // skip NUL
        if !dot_pat && name.first() == Some(&b'.') {
            continue;
        }
        if pat_match(pat, name) {
            hits.push(name.to_vec());
        }
    }
    if hits.is_empty() {
        let mut one: Vec<String> = Vec::new();
        one.push(s);
        return one;
    }
    hits.sort();
    hits.into_iter()
        .map(|n| {
            let n = String::from_utf8(n).unwrap_or_default();
            if dir_typed.is_empty() {
                n
            } else {
                let mut full = String::from(dir_typed);
                full.push('/');
                full.push_str(&n);
                full
            }
        })
        .collect()
}

// ── Parse riga ──────────────────────────────────────────────────────

struct RawRedir {
    slot: u8,
    append: bool,
    dup_to_1: bool,
    target: Vec<Ch>,
}

/// Chiude parole+redirect in un `Command` espanso (o None se vuoto).
/// `NAME=val` come unica parola = assegnazione persistente; con comando
/// appresso = prefisso d'ambiente mono-comando (Fase 43) = errore.
fn finish(words: Vec<Vec<Ch>>, redirs: Vec<RawRedir>, status: i64, bg: bool) -> Result<Option<Command>, ParseError> {
    let mut assign: Option<(String, String)> = None;
    let mut argv_words: &[Vec<Ch>] = &words[..];
    if words.len() == 1 {
        if let Some((name, val)) = split_assign(&words[0]) {
            let val = expand_word(&val, status, false)?;
            argv_words = &[];
            assign = Some((name, word_string(&val[0])));
        }
    } else if !words.is_empty() && split_assign(&words[0]).is_some() {
        // `NAME=val cmd...`: prefisso d'ambiente mono-comando (Fase 43).
        return Err(ParseError::EnvPrefix);
    }
    let mut argv: Vec<String> = Vec::new();
    for w in argv_words {
        for piece in expand_word(w, status, true)? {
            for g in glob_word(&piece) {
                argv.push(g);
            }
        }
    }
    let mut rr: Vec<redirect::Redir> = Vec::new();
    for r in redirs.iter() {
        let t = expand_word(&r.target, status, false)?;
        rr.push(redirect::Redir {
            slot: r.slot,
            append: r.append,
            dup_to_1: r.dup_to_1,
            target: word_string(&t[0]),
        });
    }
    if assign.is_some() && !argv.is_empty() {
        return Err(ParseError::EnvPrefix);
    }
    if argv.is_empty() && rr.is_empty() && assign.is_none() {
        return Ok(None);
    }
    Ok(Some(Command { argv, redirs: rr, bg, assign }))
}

/// Parsa una riga in comandi sequenziati gia' espansi. `status` = `$?`.
/// Connettori consecutivi: vince l'ultimo (documentato, mai errore).
pub(crate) fn parse_line(line: &str, status: i64) -> Result<Seq, ParseError> {
    let pieces = tokenize(line)?;
    let mut cmds: Vec<Command> = Vec::new();
    let mut cons: Vec<Conn> = Vec::new();
    let mut pending: Option<Conn> = None;
    let mut words: Vec<Vec<Ch>> = Vec::new();
    let mut redirs: Vec<RawRedir> = Vec::new();
    // Chiude il comando corrente al connettore (bg = da `&`): se non vuoto
    // lo spinge con il connettore pendente; a comando vuoto il connettore
    // si accumula (vince l'ultimo) o cade (in testa).
    let at_conn = |cmds: &mut Vec<Command>,
                       cons: &mut Vec<Conn>,
                       pending: &mut Option<Conn>,
                       words: &mut Vec<Vec<Ch>>,
                       redirs: &mut Vec<RawRedir>,
                       conn: Conn,
                       bg: bool,
                       status: i64|
     -> Result<(), ParseError> {
        let words_take = core::mem::take(words);
        let redirs_take = core::mem::take(redirs);
        let nonempty = !words_take.is_empty() || !redirs_take.is_empty();
        if nonempty {
            match finish(words_take, redirs_take, status, bg)? {
                Some(cmd) => {
                    cmds.push(cmd);
                    // Il connettore tra questo e il precedente e' il pendente
                    // (se c'e': al primo comando non c'e' mai); il connettore
                    // corrente diventa pendente per il gap successivo.
                    if cmds.len() >= 2 {
                        match pending.take() {
                            Some(p) => cons.push(p),
                            None => cons.push(Conn::Seq),
                        }
                    }
                    *pending = Some(conn);
                }
                None => {
                    // Espanso a vuoto (`$UNSET` solo): come comando vuoto.
                    if !cmds.is_empty() {
                        *pending = Some(conn);
                    }
                }
            }
        } else if !cmds.is_empty() {
            *pending = Some(conn);
        }
        Ok(())
    };
    let mut i = 0;
    while i < pieces.len() {
        match &pieces[i] {
            Piece::Word(w) => {
                words.push(w.clone());
                i += 1;
            }
            Piece::Op(op) => match op {
                // `2>&1` non ha target (alias): va registrato da solo, SENZA
                // consumare la parola dopo (bug passo-1: cadeva nel ramo con
                // target e `2>&1 > /f` moriva in MissingTarget).
                Op::Dup21 => {
                    redirs.push(RawRedir { slot: 2, append: false, dup_to_1: true, target: Vec::new() });
                    i += 1;
                }
                Op::Gt | Op::GtGt | Op::Lt | Op::E2Gt | Op::E2GtGt => {
                    let (slot, append, dup) = match op {
                        Op::Gt => (1, false, false),
                        Op::GtGt => (1, true, false),
                        Op::Lt => (0, false, false),
                        Op::E2Gt => (2, false, false),
                        Op::E2GtGt => (2, true, false),
                        // Irraggiungibile (Dup21 ha un ramo dedicato sopra):
                        // serve per l'esaustivita'.
                        Op::Dup21 => (2, false, true),
                        _ => (1, false, false),
                    };
                    let tgt = match pieces.get(i + 1) {
                        Some(Piece::Word(w)) => w.clone(),
                        _ => {
                            let name: &'static str = match op {
                                Op::Gt => ">",
                                Op::GtGt => ">>",
                                Op::Lt => "<",
                                Op::E2Gt => "2>",
                                _ => "2>>",
                            };
                            return Err(ParseError::MissingTarget(name));
                        }
                    };
                    redirs.push(RawRedir { slot, append, dup_to_1: dup, target: tgt });
                    i += 2;
                }
                Op::Semi => {
                    at_conn(&mut cmds, &mut cons, &mut pending, &mut words, &mut redirs, Conn::Seq, false, status)?;
                    i += 1;
                }
                Op::And => {
                    at_conn(&mut cmds, &mut cons, &mut pending, &mut words, &mut redirs, Conn::And, false, status)?;
                    i += 1;
                }
                Op::Or => {
                    at_conn(&mut cmds, &mut cons, &mut pending, &mut words, &mut redirs, Conn::Or, false, status)?;
                    i += 1;
                }
                Op::Bg => {
                    at_conn(&mut cmds, &mut cons, &mut pending, &mut words, &mut redirs, Conn::Seq, true, status)?;
                    i += 1;
                }
            },
        }
    }
    // Coda riga: comando finale senza connettore.
    if !words.is_empty() || !redirs.is_empty() {
        if let Some(cmd) = finish(words, redirs, status, false)? {
            cmds.push(cmd);
            if cmds.len() >= 2 {
                match pending.take() {
                    Some(p) => cons.push(p),
                    None => cons.push(Conn::Seq),
                }
            }
        }
    }
    Ok(Seq { cmds, cons })
}
