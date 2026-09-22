use super::*;

// ── Mini-lexer redirect (40.4a) + apertura (40.4b/c/d) ─────────────────
// Parsing puro + `open_all` per builtin/run: apre TUTTI i target IN ORDINE
// (bash-like: a pari slot vince l'ULTIMO, i precedenti risultano comunque
// creati/troncati; `2>&1` aliasa lo slot 2 sull'fd corrente dello slot 1,
// zero open). `<` apre read-only (file deve esistere).

pub(crate) struct Redir {
    pub(crate) slot: u8, // 0 = stdin, 1 = stdout, 2 = stderr
    pub(crate) append: bool,
    pub(crate) dup_to_1: bool, // true solo per `2>&1` (alias, nessun target)
    pub(crate) target: String,
}

pub(crate) enum ParseError {
    MissingTarget(&'static str),
}

/// Tokenizza: whitespace separa, `>` `<` `&` sono sempre confini di token,
/// con unita' multi-char `>>` `2>` `2>>` `2>&1`. `&` da solo (background)
/// resta token a se' anche se attaccato (`foo&` → `foo`, `&`).
fn tokenize(s: &str) -> Vec<String> {
    let b = s.as_bytes();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        // `2>&1` prima di ogni altra regola (contiene `&`).
        if b[i] == b'2' && i + 3 < b.len() + 1 && s[i..].starts_with("2>&1") {
            toks.push(String::from("2>&1"));
            i += 4;
            continue;
        }
        // `2>` / `2>>` attaccati.
        if b[i] == b'2' && i + 1 < b.len() && b[i + 1] == b'>' {
            if i + 2 < b.len() && b[i + 2] == b'>' {
                toks.push(String::from("2>>"));
                i += 3;
            } else {
                toks.push(String::from("2>"));
                i += 2;
            }
            continue;
        }
        if b[i] == b'>' {
            if i + 1 < b.len() && b[i + 1] == b'>' {
                toks.push(String::from(">>"));
                i += 2;
            } else {
                toks.push(String::from(">"));
                i += 1;
            }
            continue;
        }
        if b[i] == b'<' {
            toks.push(String::from("<"));
            i += 1;
            continue;
        }
        if b[i] == b'&' {
            toks.push(String::from("&"));
            i += 1;
            continue;
        }
        // Parola normale: fino a whitespace o `>` `<` `&` (con lookahead
        // `2>&1` gia' gestito sopra all'inizio token; dentro parola `&`
        // chiude comunque: `foo&` → `foo`, `&`).
        let start = i;
        while i < b.len()
            && !b[i].is_ascii_whitespace()
            && b[i] != b'>'
            && b[i] != b'<'
            && b[i] != b'&'
        {
            // `2>` attaccato a parola (`hi2>e` non supportato come operatore
            // unico: troppo magico; solo `2>` a inizio token vale).
            i += 1;
        }
        // Parola vuota impossibile qui (almeno un byte consumato).
        if let Ok(w) = core::str::from_utf8(&b[start..i]) {
            toks.push(String::from(w));
        }
    }
    toks
}

fn is_op(t: &str) -> bool {
    matches!(t, ">" | ">>" | "<" | "2>" | "2>>" | "2>&1")
}

/// Parsa una riga in `(argv, redirs)`. `argv` include `&` finale (background,
/// gestito da `cmd_run` come prima). I redirect multipli sullo stesso slot
/// sono conservati TUTTI in ordine (ultimo vince in esecuzione).
pub(crate) fn parse(line: &str) -> Result<(Vec<String>, Vec<Redir>), ParseError> {
    let toks = tokenize(line);
    let mut argv = Vec::new();
    let mut redirs = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let t = toks[i].as_str();
        if t == "2>&1" {
            redirs.push(Redir {
                slot: 2,
                append: false,
                dup_to_1: true,
                target: String::new(),
            });
            i += 1;
            continue;
        }
        let (slot, append) = match t {
            ">" => (1, false),
            ">>" => (1, true),
            "<" => (0, false),
            "2>" => (2, false),
            "2>>" => (2, true),
            _ => {
                argv.push(String::from(t));
                i += 1;
                continue;
            }
        };
        // Operatore: serve un target parola-non-operatore dopo.
        if i + 1 >= toks.len() || is_op(toks[i + 1].as_str()) {
            let op: &'static str = match t {
                ">" => ">",
                ">>" => ">>",
                "<" => "<",
                "2>" => "2>",
                _ => "2>>",
            };
            return Err(ParseError::MissingTarget(op));
        }
        redirs.push(Redir {
            slot,
            append,
            dup_to_1: false,
            target: String::from(toks[i + 1].as_str()),
        });
        i += 2;
    }
    Ok((argv, redirs))
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
