use super::*;
use super::provider::MountedFs;

// ── Mount locali dinamici (Fase 16b) ─────────────────────────────────
// Tabella VFS userspace (nessun kernel coinvolto, ADR-0005): binding
// target → filesystem montato. La radice resta sempre ramfs. Il contenitore
// e' generico (`FsMount` + `MountedFs`): oggi solo FAT32, domani ext2/ISO9660
// aggiungono una variante senza reshuffle della tabella.

/// Filesystem montato su un target. Le varianti tengono l'istanza viva
/// (parser + client); `None` = spec registrata ma inattiva (sorgente assente
/// all'ultimo tentativo: gli accessi sotto il target falliscono invece di
/// finire shadow in ramfs, e il prossimo accesso ritenta l'attivazione).

/// Mount locale: binding target → sorgente + istanza.
pub struct FsMount {
    /// Target normalizzato senza slash ("fat", "mnt").
    pub target: String,
    /// Source originale (`UUID=xxxxxxxx`, mai lettere instabili) per
    /// diagnostica e re-apply.
    source: String,
    /// Opzioni mount (placeholder Strato 0: conservate, non interpretate —
    /// futuro: uid=/gid=/mode per i permessi FAT finti alla Linux).
    opts: String,
    /// Handle nodo disco codificato (disco<<16|sub, vedi `disk_handle`).
    /// Ha senso solo per sorgenti a blocchi (variante Fat); le future
    /// varianti non-blocco lo ignoreranno.
    handle: u32,
    /// Filesystem montato.
    fs: MountedFs,
}

/// Spec statiche applicate a OGNI boot (fresco o restart): sostituiscono il
/// binding hardcodato con lo stesso codice dei mount dinamici (dogfood).
/// Montate per UUID stabile (Fase 16d): il boot non dipende piu' dalle
/// lettere `sdX`. Dinamici (R_MOUNT, 16b.2) si aggiungono alla tabella ma si
/// perdono al restart (stato runtime, come fd e handshake: i client
/// ristabiliscono).
pub const STATIC_MOUNTS: &[(&str, &str)] = &[("UUID=4F4C4556", "fat")];

/// Normalizza un target ("//mnt//" → "mnt"). Rifiuta root, vuoti, `.`/`..`.
pub fn normalize_target(target: &str) -> Option<String> {
    let t = target.trim().trim_matches('/');
    if t.is_empty() {
        return None;
    }
    if t.split('/').any(|c| c.is_empty() || c == "." || c == "..") {
        return None;
    }
    Some(String::from(t))
}

/// Normalizza una source. Tre forme (Fase 16d): `/dev/<nodo>` (nomi brevi
/// `sda`, by-path `disk/by-uuid/<HEX>` / `disk/by-label/<NOME>`), `UUID=<hex8>`
/// (seriale volume FAT, maiuscolo), `LABEL=<nome>` (match esatto, case
/// sensibile). Solo controllo sintattico: l'handle lo alloca userdisk via
/// DISK_RESOLVE (`resolve_mount_source`). Ritorna None fuori grammatica.
fn normalize_source(source: &str) -> Option<String> {
    let s = source.trim();
    if let Some(name) = s.strip_prefix("/dev/") {
        if name.is_empty() || name.contains("//") || name.len() > 32 {
            return None;
        }
        let name = name.trim_matches('/');
        if name.is_empty() {
            return None;
        }
        return Some(alloc::format!("/dev/{}", name));
    }
    if let Some(hex) = s.strip_prefix("UUID=") {
        if hex.len() != 8 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        return Some(String::from(s));
    }
    if let Some(label) = s.strip_prefix("LABEL=") {
        if label.is_empty() || label.len() > 11 || label.contains('/') {
            return None;
        }
        return Some(String::from(s));
    }
    None
}

/// Riduce una source normalizzata alla chiave di resolve (Fase 16d):
/// `/dev/sda` → `sda`, `/dev/disk/by-uuid/<H>` → `<H>`,
/// `/dev/disk/by-label/<N>` → `<N>`, `UUID=<H>` → `<H>`, `LABEL=<N>` → `<N>`.
/// La semantica (`/dev` = namespace, driver = matching) resta una sola:
/// userfs possiede il layout, userdisk il matching nome/UUID/label.
fn resolve_key(source: &str) -> Option<String> {
    if let Some(name) = source.strip_prefix("/dev/") {
        if let Some(tail) = name.strip_prefix("disk/by-uuid/") {
            return (!tail.is_empty() && !tail.contains('/')).then(|| String::from(tail));
        }
        if let Some(tail) = name.strip_prefix("disk/by-label/") {
            return (!tail.is_empty() && !tail.contains('/')).then(|| String::from(tail));
        }
        if name.is_empty() || name.contains('/') {
            return None;
        }
        return Some(String::from(name));
    }
    if let Some(hex) = source.strip_prefix("UUID=") {
        return Some(String::from(hex));
    }
    if let Some(label) = source.strip_prefix("LABEL=") {
        return Some(String::from(label));
    }
    None
}

/// Risolve una source in handle presso userdisk (Fase 16c/16d: single
/// source of truth nel driver). Ritorna None a chiave sconosciuta o driver
/// irraggiungibile (bound, mai wedge): il chiamante non cambia stato.
pub fn resolve_mount_source(source: &str) -> Option<u32> {
    let key = resolve_key(source)?;
    if key.is_empty() || key.len() > 16 {
        return None;
    }
    IpcDisk::new(0).resolve(&key)
}

/// Applica una spec (statica o dinamica): valida e registra/aggiorna sempre la
/// spec (idempotente sul target). L'handle si chiede a userdisk (Fase 16c).
/// Resolve fallito (nome sconosciuto o driver irraggiungibile): NESSUN cambio
/// di stato (come il parse fallito di prima) — la distinzione nome-ignoto vs
/// driver-down non serve: a driver caduto il client riprova (restart ~50 tick,
/// bound 500 dentro `resolve`); l'inattivita' lazy resta per BPB invalida e
/// drop d'epoca (`note_peer_death`). Ritorna true se il mount e' ATTIVO
/// (BPB valida subito), false altrimenti (spec inattiva registrata solo a
/// resolve riuscito ma BPB illeggibile: ritenta lazy, mai shadow ramfs).
pub fn apply_mount_spec(
    mounts: &mut Vec<FsMount>,
    source: &str,
    target: &str,
    opts: &str,
) -> bool {
    let norm_source = match normalize_source(source) {
        Some(x) => x,
        None => return false,
    };
    let norm_target = match normalize_target(target) {
        Some(x) => x,
        None => return false,
    };
    // Resolve una sola volta qui (vale per spec nuove e sostituite): a
    // fallimento la tabella resta intatta (mai distruggere un buon mount con
    // una source sbagliata, mai registrare nomi ignoti).
    let handle = match resolve_mount_source(&norm_source) {
        Some(h) => h,
        None => return false,
    };
    if let Some(m) = mounts.iter_mut().find(|m| m.target == norm_target) {
        m.source = norm_source;
        m.opts = String::from(opts);
        m.handle = handle;
        m.fs = MountedFs::Fat(Fat32::mount(IpcDisk::new(handle)));
        return matches!(&m.fs, MountedFs::Fat(Some(_)));
    }
    let fs = MountedFs::Fat(Fat32::mount(IpcDisk::new(handle)));
    let active = matches!(&fs, MountedFs::Fat(Some(_)));
    mounts.push(FsMount {
        target: norm_target,
        source: norm_source,
        opts: String::from(opts),
        handle,
        fs,
    });
    active
}

/// Riattiva un mount inattivo (Fase 16c): re-resolve del nome presso userdisk
/// (gli handle possono cambiare dopo un restart del driver) + remount.
/// Fast path: mount gia' attivo → true senza IPC. Ritorna true se attivo.
/// A remount riuscito bumpa `gen` (l'istanza parser e' nuova: le cache
/// FileInfo per-fd vanno rifatte).
pub fn reactivate_mount(mounts: &mut Vec<FsMount>, mi: usize, fgen: &mut u64) -> bool {
    if mounts.get(mi).map_or(false, |m| m.is_active()) {
        return true;
    }
    let name = match mounts.get(mi) {
        Some(m) => m.source.clone(),
        None => return false,
    };
    let handle = match resolve_mount_source(&name) {
        Some(h) => h,
        None => return false,
    };
    match mounts.get_mut(mi) {
        Some(m) => {
            m.handle = handle;
            m.fs = MountedFs::Fat(Fat32::mount(IpcDisk::new(handle)));
            let ok = m.is_active();
            if ok {
                *fgen = fgen.wrapping_add(1);
            }
            ok
        }
        None => false,
    }
}

impl FsMount {
    /// Istanza FAT se montata e attiva (None se altra variante o inattiva).
    /// Le future varianti (ext2/ISO) aggiungono i loro accessor qui; gli
    /// handler matchano la variante una sola volta per op.
    pub fn fat(&self) -> Option<&Fat32<IpcDisk>> {
        match &self.fs {
            MountedFs::Fat(opt) => opt.as_ref(),
            MountedFs::Local(_) => None,
        }
    }

    /// Invalida il client disco alla morte del peer (solo variante Fat con
    /// mount attivo; le future varianti con client propri fanno lo stesso).
    /// Se eravamo connessi (cambio d'epoca) droppa anche l'istanza: gli handle
    /// possono cambiare dopo un restart del driver (Fase 16c) e un handle
    /// stale leggerebbe il disco sbagliato in silenzio — il prossimo accesso
    /// re-risolve per nome e rimonta (fail-loud, mai shadow ramfs).
    /// Ritorna true se l'istanza e' stata droppata (il chiamante bumpa la
    /// generazione delle cache FileInfo).
    pub fn note_peer_death(&mut self, dead_chan: u64) -> bool {
        if let MountedFs::Fat(Some(f)) = &self.fs {
            if f.disk().note_peer_death(dead_chan) {
                self.fs = MountedFs::Fat(None);
                return true;
            }
        }
        false
    }

    /// true se il mount e' attivo (istanza viva).
    pub fn is_active(&self) -> bool {
        match &self.fs {
            MountedFs::Fat(opt) => opt.is_some(),
            MountedFs::Local(_) => true,
        }
    }
}

/// Match puro target (longest prefix, SENZA attivazione): true se il path e'
/// sotto un mount FAT noto (anche inattivo). Usato per rifiutare le op di
/// scrittura/creazione ramfs sotto target FAT (niente shadow).
pub fn target_match(mounts: &[FsMount], path: &str) -> bool {
    let t = path.trim_start_matches('/');
    mounts.iter().any(|m| {
        t == m.target
            || (t.len() > m.target.len()
                && t.as_bytes().get(m.target.len()) == Some(&b'/')
                && t.starts_with(m.target.as_str()))
    })
}

/// Risolve un path nel mount col prefix piu' lungo. Attiva lazy se il mount e'
/// inattivo (re-resolve per nome + remount via `reactivate_mount`; solo
/// variante Fat: le future varianti aggiungono il loro ramo qui).
/// Ritorna (indice mount, rel).
pub fn resolve_fsmount<'a>(mounts: &mut Vec<FsMount>, path: &'a str, fgen: &mut u64) -> Option<(usize, &'a str)> {
    let t = path.trim_start_matches('/');
    let mut best: Option<(usize, &str)> = None;
    for (i, m) in mounts.iter().enumerate() {
        let rel = if t == m.target {
            ""
        } else if t.len() > m.target.len()
            && t.as_bytes().get(m.target.len()) == Some(&b'/')
            && t.starts_with(m.target.as_str())
        {
            &t[m.target.len() + 1..]
        } else {
            continue;
        };
        if best.map_or(true, |(_, r)| rel.len() < r.len()) {
            best = Some((i, rel));
        }
    }
    let (i, rel) = best?;
    if !reactivate_mount(mounts, i, fgen) {
        return None;
    }
    Some((i, rel))
}

// ── Helper conversione ─────────────────────────────────────────────

/// Converte `Result<u64, u64>` in valore IPC (Fase 40, errori tipizzati):
/// `Ok(v)` → `v`, `Err(code)` → la sentinella del rifiuto.
#[inline]
pub fn to_reply_res(val: Result<u64, u64>) -> u64 {
    val.unwrap_or_else(|e| e)
}
