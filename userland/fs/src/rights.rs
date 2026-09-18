use super::*;

// ── Diritti per-canale (Fase 17, self-restriction only) ─────────────
// Un `Channel` e' tutto-o-niente: chi ha l'id manda qualunque cosa. Primo
// passo verso IPC a capability, senza kernel (userfs conosce gia' ogni peer
// dal canale): tabella `chan → {ops bitmask, subtree prefix}`, SOLO in
// riduzione (DROP fa AND, mai widen, nessuna auth: nessuno puo' darsi
// diritti, solo toglierseli — nessun GRANT, i canali non sono trasferibili).
// Default (entry assente): {ALL, root} = tutto verde, zero alloc, suite
// invariata. Purge su EXIT_NOTIFY come rings/ftable. Effimeri: restart
// userfs = re-handshake full (limite dichiarato).
//
// Check su DUE livelli nel dispatch FS_NOTIFY:
// - ops bit: CENTRALE, prima di qualunque contatto handler/driver;
// - subtree: solo alle op con path (OPEN/MKDIR/READDIR/MOUNT-target/
//   UMOUNT-target); gli fd restano capability pure (read/write/close non
//   ricontrollano il path aperto).
// CLOSE sempre consentito (rilascia stato, mai escalation: nessun bit).
// DROP/GET sempre consentiti (gestire i propri diritti non si nega).
// Registrazione driver (FS_REGISTER, altro IPC tag) non gatata: handshake
// server-to-server, fuori dal modello self-restriction (limite dichiarato).

/// Diritti di un canale client: mask ops + subtree normalizzato senza slash
/// ("" = root).
#[derive(Clone)]
pub struct ChanRights {
    ops: u32,
    subtree: String,
}

/// Mask ops effettiva (default ALL a entry assente).
pub fn rights_ops(rights: &BTreeMap<u64, ChanRights>, chan: u64) -> u32 {
    rights.get(&chan).map_or(libr::RIGHTS_ALL, |r| r.ops)
}

/// Subtree effettivo (default root "" a entry assente).
pub fn rights_subtree<'a>(rights: &'a BTreeMap<u64, ChanRights>, chan: u64) -> &'a str {
    rights.get(&chan).map_or("", |r| r.subtree.as_str())
}

/// Normalizza subtree/path ("//fat//" → "fat", "/" o "" → "").
fn normalize_sub(path: &str) -> String {
    String::from(path.trim().trim_matches('/'))
}

/// Vista normalizzata (solo trim, ZERO alloc): per i CHECK per-op nel choke
/// point (ogni op con path la attraversa). Per gli STORE nella tabella diritti
/// (long-lived oltre la richiesta) resta `normalize_sub` owned.
pub fn normalize_sub_view(path: &str) -> &str {
    path.trim().trim_matches('/')
}

/// true se il path normalizzato `p` e' dentro il subtree `sub` ("" = root).
pub fn within_subtree(sub: &str, p: &str) -> bool {
    sub.is_empty()
        || p == sub
        || (p.len() > sub.len()
            && p.as_bytes().get(sub.len()) == Some(&b'/')
            && p.starts_with(sub))
}

/// Bit ops richiesto dall'op_tag. None = sempre consentito (CLOSE, DROP, GET).
pub fn op_bit(op_tag: u32) -> Option<u32> {
    match op_tag {
        R_OPEN => Some(libr::RIGHTS_OPEN),
        R_READ => Some(libr::RIGHTS_READ),
        R_WRITE => Some(libr::RIGHTS_WRITE),
        R_READDIR => Some(libr::RIGHTS_READDIR),
        R_MKDIR => Some(libr::RIGHTS_MKDIR),
        R_MOUNT => Some(libr::RIGHTS_MOUNT),
        R_UMOUNT => Some(libr::RIGHTS_UMOUNT),
        R_DELETE => Some(libr::RIGHTS_DELETE),
        // R_STAT e' metadato di listing: stesso bit di READDIR (Fase 19.2).
        R_STAT => Some(libr::RIGHTS_READDIR),
        _ => None,
    }
}

/// R_RIGHTS_DROP: w0 = mask da tenere, payload = subtree (vuoto = solo-ops).
/// Solo shrink (ops &= mask&ALL); subtree sostituito solo se dentro il
/// corrente, altrimenti widen = None senza NESSUN cambio (prima valida, poi
/// applica). Crea l'entry da default se assente. Ritorna Some(0) o None.
pub fn handle_rights_drop(
    rights: &mut BTreeMap<u64, ChanRights>,
    chan: u64,
    keep: u32,
    payload: &[u8],
) -> Option<u64> {
    let sub = match core::str::from_utf8(payload) {
        Ok(s) => s,
        Err(_) => return None,
    };
    let (cur_ops, cur_sub) = match rights.get(&chan) {
        Some(r) => (r.ops, r.subtree.clone()),
        None => (libr::RIGHTS_ALL, String::new()),
    };
    // Subtree richiesto (raw non-vuoto: "/" esplicita conta come richiesta di
    // root, NON come no-op — da "/fat" sarebbe widen e va rifiutata).
    let norm = if sub.is_empty() {
        None
    } else {
        let n = normalize_sub(sub);
        if !within_subtree(&cur_sub, &n) {
            return None;
        }
        Some(n)
    };
    let entry = rights.entry(chan).or_insert(ChanRights {
        ops: libr::RIGHTS_ALL,
        subtree: String::new(),
    });
    entry.ops = cur_ops & (keep & libr::RIGHTS_ALL);
    if let Some(n) = norm {
        entry.subtree = n;
    }
    Some(0)
}

/// R_RIGHTS_GET: scrive il response frame `[ops:8][sublen:8][subtree]`
/// (self-written come read/readdir: il dispatch generico NON riscrive) e
/// ritorna Some(ops). Sempre consentito.
pub fn handle_rights_get(
    rights: &BTreeMap<u64, ChanRights>,
    rings: &BTreeMap<u64, (u64, u64)>,
    chan: u64,
) -> Option<u64> {
    let (ops, sub) = match rights.get(&chan) {
        Some(r) => (r.ops, r.subtree.as_str()),
        None => (libr::RIGHTS_ALL, ""),
    };
    if rings.contains_key(&chan) {
        rings::map_client_resp_ring(rings, chan);
        rings::resp_ring_write(ops as u64, sub.len() as u64, sub.as_bytes());
    }
    Some(ops as u64)
}
