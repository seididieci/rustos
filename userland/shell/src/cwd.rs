use super::*;

/// Directory corrente, client-side (Fase 18.1): il FS non ha concetto di cwd,
/// la risoluzione e' tutta qui (`resolve()`). Sempre path assoluto normalizzato.
static mut CWD: Option<String> = None;

pub(crate) fn cwd_get() -> String {
    // Niente shared ref diretto allo static (hard error `static_mut_refs`):
    // si passa dal raw pointer (single-threaded, niente aliasing reale).
    unsafe {
        (*core::ptr::addr_of_mut!(CWD))
            .clone()
            .unwrap_or_else(|| String::from("/"))
    }
}

pub(crate) fn cwd_set(s: String) {
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
pub(crate) fn resolve(path: &str) -> String {
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

pub(crate) fn cmd_cd(args: &[&str]) -> i64 {
    if args.len() < 2 {
        cwd_set(String::from("/"));
        return 0;
    }
    let path = resolve(args[1]);
    // Sonda senza effetti collaterali: readdir fallisce su file/inesistenti
    // (open creerebbe il file: mai usarlo come sonda).
    let mut probe = vec![0u8; 256];
    if libr::readdir(&path, &mut probe, 256).is_err() {
        term::term_err("cd: no such directory: ");
        term::term_err(args[1]);
        term::term_err("\n");
        return 1;
    }
    cwd_set(path);
    0
}

pub(crate) fn cmd_pwd() -> i64 {
    term::term_print(&cwd_get());
    term::term_print("\n");
    0
}
