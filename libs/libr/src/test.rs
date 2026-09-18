//! Harness condiviso per la test suite (A4): traversal delle entry readdir
//! (formato "name\0name\0...\0\0", `count` entry). Prima copiato in
//! testfs/testfat/usertests con 3 consumatori diversi (print/collect/search).
//! Solo il traversal e' condiviso: `report`/`expect`/`run` restano locali ai
//! singoli test (formati diversi, single-user).

/// Chiama `f(name)` per ognuna delle `count` entry NUL-terminate in `entries`.
/// Nomi non-UTF8 arrivano come "?". Si ferma dopo `count` nomi registrati o a
/// doppio NUL (fine lista). Stessa semantica dei 3 loop originari (skip zeri,
/// `seen` incrementato solo a nome registrato). Il lifetime del callback e'
/// legato a `entries`: il chiamante puo' anche accumulare i nomi (testfat).
pub fn each_name<'e, F>(entries: &'e [u8], count: usize, mut f: F)
where
    F: FnMut(&'e str),
{
    let mut i = 0usize;
    let mut seen = 0usize;
    while i < entries.len() && seen < count {
        if entries[i] == 0 {
            i += 1;
            continue;
        }
        let start = i;
        while i < entries.len() && entries[i] != 0 {
            i += 1;
        }
        if i > start {
            f(core::str::from_utf8(&entries[start..i]).unwrap_or("?"));
            seen += 1;
        }
        if i < entries.len() && entries[i] == 0 {
            i += 1;
        }
        if i < entries.len() && entries[i] == 0 {
            break;
        }
    }
}
