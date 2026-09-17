//! userfs — File system server (Fase 9.1 + 9.2 + 9.3 + 10.2 + 16.2).
//!
//! Riceve IPC dai processi client (open/read/write/close/readdir/mkdir) e
//! gestisce:
//!   - ramfs in memoria sul mount point `/` (scrivibile, Fase 9.1)
//!   - FAT32 dal disco via `userdisk` sul mount point `/fat` (Fase 9.2 su
//!     ATA locale; Fase 16 via IPC `DISK_*`; Fase 16c resolve nome→handle
//!     lato driver; **scrivibile dalla Fase 20**: overwrite/crescita/`O_CREAT`,
//!     niente unlink)
//!   - devfs/console remoti via IPC per device `/dev/*` (Fase 9.3)
//!
//! Trasferimento dati (Fase 10.2): ogni client ha DUE pagine ring SPSC
//! (request + response) allocate dalla syscall 26 (`SYS_RING_ALLOC`). Il client
//! scrive un request frame nel request ring, notifica con `FS_NOTIFY`, e userfs
//! legge il frame, processa, e scrive il response frame nel response ring del
//! client. Per i device remoti userfs inietta la response ring del client nel
//! processo driver (`libr::map_in`) cosi' il driver scrive i dati direttamente
//! nella response ring del client — zero copie.

#![no_std]
#![no_main]

extern crate alloc;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use libr;

mod fat32;
mod ipc_disk;

use fat32::{Fat32, FileInfo};
use ipc_disk::IpcDisk;
use libr::println;

/// Request ring virtuale (coincide con USER_FS_BUFFER del kernel).
const REQ_RING_VA: u64 = 0x0000_4000_0020_0000;
/// Response ring virtuale (USER_FS_BUFFER + 0x1000).
const RESP_RING_VA: u64 = 0x0000_4000_0021_0000;
/// Capacita' dati per ring: l'area dati occupa [0x0000, 0xFF8) = 4088 byte;
/// head/tail vivono a 0xFF8/0xFFC (fuori dall'area dati).
const RING_DATA_CAP: usize = 4088;
/// Offset head nel ring page.
const RING_HEAD: usize = 0xFF8;
/// Offset tail nel ring page.
const RING_TAIL: usize = 0xFFC;

/// Valore di errore IPC: tutti i bit a 1 (equivalente unsigned di -1).
const ERR: u64 = !0u64;
/// Il client non ha (piu') l'handshake ring presso questo server (es. server
/// riavviato dopo la registrazione: t28). Il client deve rifare FS_BUF_REG e
/// ripetere l'operazione UNA volta. Riservato: nessun result legittimo (fd,
/// conteggi) puo' assumere questo valore in pratica.
const ERR_NOHANDSHAKE: u64 = !0u64 - 1;

// ── Tag delle operazioni (nei frame del ring) ─────────────────────
// Single source in `syscall-numbers` (Fase 17): include R_RIGHTS_DROP/GET.
use libr::{
    R_CLOSE, R_DELETE, R_MKDIR, R_MOUNT, R_OPEN, R_READ, R_READDIR, R_REGISTER, R_UMOUNT,
    R_WRITE, R_RIGHTS_DROP, R_RIGHTS_GET, R_STAT,
};
// Tag IPC FS/boot (DocsB): single source in `syscall-numbers`, via `libr`.
use libr::{FS_BUF_REG, FS_NOTIFY, FS_REGISTER, SVC_READY};

const MAX_PATH: usize = 256;

// ── IPC tags verso i driver remoti (devfs/console) ─────────────────

const DEV_OPEN: u64 = 0x20;
const DEV_READ: u64 = 0x21;
const DEV_WRITE: u64 = 0x22;
const DEV_CLOSE: u64 = 0x23;
const DEV_READDIR: u64 = 0x24;

// ── Device types (w0 di DEV_OPEN) ──────────────────────────────────

const DEV_NULL: u64 = 0;
const DEV_ZERO: u64 = 1;
/// Device type del console server (tastiera/terminale, prefix "/dev/input").
const DEV_KEYBOARD: u64 = 2;
/// Device type del console server come output (Fase 15, prefix "/dev/console").
const DEV_CONSOLE: u64 = 3;
/// Device type del driver tastiera (Fase 15, prefix "/dev/kbd").
const DEV_KBD: u64 = 4;

// ── Mount table dinamica ───────────────────────────────────────────

/// Risoluzione di un path: filesystem locale o server remoto.
#[derive(Clone, Copy, PartialEq)]
enum FsKind {
    Ram,
    Fat,
}

/// Un mount point registrato da un driver via FS_REGISTER.
struct Mount {
    prefix: alloc::string::String,
    /// Canale del driver verso userfs (ADR-0008): userfs inoltra le DEV_* su
    /// QUESTO canale (il driver lo ha aperto con service_lookup(Fs)).
    driver_chan: u64,
}

/// Cerca il mount point più lungo che matcha il path (longest prefix match).
/// Ritorna (driver_chan, path relativo al mount).
fn resolve_mount<'a>(path: &'a str, mounts: &[Mount]) -> Option<(u64, &'a str)> {
    let t = path.trim_start_matches('/');
    let mut best: Option<(u64, &'a str)> = None;
    for m in mounts {
        let prefix = m.prefix.trim_start_matches('/');
        if t == prefix {
            let rel = "";
            if best.as_ref().map_or(true, |(_, r)| r.len() > rel.len()) {
                best = Some((m.driver_chan, rel));
            }
        } else if t.len() > prefix.len()
            && t.as_bytes().get(prefix.len()) == Some(&b'/')
            && t.starts_with(prefix)
        {
            let rel = &t[prefix.len() + 1..];
            if best.as_ref().map_or(true, |(_, r)| r.len() > rel.len()) {
                best = Some((m.driver_chan, rel));
            }
        }
    }
    best
}

/// Figli immediati di `path` tra i prefix registrati (Fase 16d, discovery).
/// I prefix (`/dev/null`, `/dev/disk/by-uuid/<H>`, …) implicano le directory
/// che li contengono: `readdir("/dev")` → ["console", "disk", "input", …].
/// Root INCLUSA (Fase 18.1-ter: union con dedupe nel chiamante, mai shadow
/// del ramfs): `readdir("/")` → ["dev", …]. Ritorna None se nessun prefix sta
/// sotto `path`. Nessun IPC: la Mount table basta (single source gia' qui).
/// Nomi presi in prestito dai prefix (mai heap: vivono nella Mount table oltre
/// la richiesta); il contenitore e' scratch (vita = iterazione corrente).
fn synth_children<'a>(mounts: &'a [Mount], path: &str) -> Option<StrList<'a>> {
    let t = path.trim_matches('/');
    let mut out = StrList::with_capacity(mounts.len())?;
    for m in mounts {
        let p = m.prefix.trim_start_matches('/');
        if t.is_empty() {
            // Root: primo componente di ogni prefix ("dev" da "/dev/null").
            let child = p.split('/').next().unwrap_or("");
            if !child.is_empty() && !out.contains(child) {
                out.push(child);
            }
            continue;
        }
        if p.len() <= t.len() {
            continue;
        }
        if p.starts_with(t) && p.as_bytes().get(t.len()) == Some(&b'/') {
            let rest = &p[t.len() + 1..];
            let child = rest.split('/').next().unwrap_or("");
            if !child.is_empty() && !out.contains(child) {
                out.push(child);
            }
        }
    }
    if out.is_empty() {
        None
    } else {
        out.sort();
        Some(out)
    }
}

/// Lista scratch di `&str` (backing libr, vita = iterazione corrente del
/// loop). Capacita' esatta a monte (ogni mount contribuisce al massimo un
/// figlio: il dedupe rende i push ≤ cap): `push` oltre cap e' no-op difensivo
/// (mai heap di fallback — i bound sono strutturali, come i ring).
/// Il contenitore e' un raw pointer (non un borrow `'static`): `as_slice`
/// restituisce un borrow legato a `&self`, che il compilatore traccia
/// nell'iterazione — meglio di un `&'static` che mentirebbe oltre il reset.
struct StrList<'a> {
    ptr: *mut &'a str,
    cap: usize,
    len: usize,
}

impl<'a> StrList<'a> {
    fn with_capacity(cap: usize) -> Option<Self> {
        // `'s = 'a`: il borrow del contenitore vive quanto i contenuti.
        let buf = libr::scratch::alloc_slice::<'a, &'a str>(cap)?;
        Some(Self { ptr: buf.as_mut_ptr(), cap, len: 0 })
    }

    fn push(&mut self, s: &'a str) {
        if self.len < self.cap {
            unsafe {
                *self.ptr.add(self.len) = s;
            }
            self.len += 1;
        }
    }

    fn contains(&self, s: &str) -> bool {
        self.as_slice().iter().any(|e| *e == s)
    }

    fn sort(&mut self) {
        unsafe {
            core::slice::from_raw_parts_mut(self.ptr, self.len).sort();
        }
    }

    fn as_slice(&self) -> &[&'a str] {
        unsafe { core::slice::from_raw_parts(self.ptr as *const _, self.len) }
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// Figli immediati di `path` tra i target dei mount locali (Fase 18.1-ter,
/// speculare a `synth_children`): i target (`fat`, `mnt`, …) sono mount point
/// e compaiono nei listing (`ls /` → ["fat", …]). Include gli inattivi: il
/// mount point esiste, l'accesso fallisce lazy come oggi. None se nessun
/// target sta sotto `path`. Come `synth_children`: nomi in prestito, scratch.
fn fsmount_children<'a>(mounts: &'a [FsMount], path: &str) -> Option<StrList<'a>> {
    let t = path.trim_matches('/');
    let mut out = StrList::with_capacity(mounts.len())?;
    for m in mounts {
        let p = m.target.as_str();
        let rest = if t.is_empty() {
            Some(p)
        } else if p.len() > t.len()
            && p.starts_with(t)
            && p.as_bytes().get(t.len()) == Some(&b'/')
        {
            Some(&p[t.len() + 1..])
        } else {
            None
        };
        if let Some(rest) = rest {
            let child = rest.split('/').next().unwrap_or("");
            if !child.is_empty() && !out.contains(child) {
                out.push(child);
            }
        }
    }
    if out.is_empty() {
        None
    } else {
        out.sort();
        Some(out)
    }
}

/// Union di entry locali con i figli dei mount (Fase 18.1-ter): i mount point
/// (`fat`, `dev`, …) compaiono nei listing senza mai coprire le entry locali
/// (dedupe a parita' di nome + sort). Rispecchia `handle_open` (driver → FAT
/// → ramfs): a parita' di nome l'entry e' una sola, mai ambigua.
fn union_mount_children(
    mut entries: Vec<String>,
    mounts: &[Mount],
    mounts_fat: &[FsMount],
    path: &str,
) -> Vec<String> {
    // Extra in prestito dalle tabelle (scratch): solo i nomi dei mount point
    // restano owned (pochi, solo nei listing che contengono mount — es. `ls /`).
    if let Some(extra) = synth_children(mounts, path) {
        for e in extra.as_slice() {
            if !entries.iter().any(|x| x.as_str() == *e) {
                entries.push(String::from(*e));
            }
        }
    }
    if let Some(extra) = fsmount_children(mounts_fat, path) {
        for e in extra.as_slice() {
            if !entries.iter().any(|x| x.as_str() == *e) {
                entries.push(String::from(*e));
            }
        }
    }
    entries.sort();
    entries
}

/// Risolve un path in FsKind (ramfs di default).
/// Per i path remoti (/dev/*), ritorna None (usa resolve_mount).
/// Per i path sotto un mount noto, ritorna Fat (usa resolve_fsmount per indice+rel).
fn resolve_local(mounts_fat: &[FsMount], path: &str) -> Option<FsKind> {
    let t = path.trim_start_matches('/');
    if t.starts_with("dev/") || t == "dev" {
        return None; // gestito da resolve_mount
    }
    if target_match(mounts_fat, path) {
        return Some(FsKind::Fat);
    }
    Some(FsKind::Ram)
}

/// Converte device name in tipo devfs (w0 di DEV_OPEN).
fn dev_type(name: &str) -> Option<u64> {
    match name {
        "null" => Some(DEV_NULL),
        "zero" => Some(DEV_ZERO),
        "keyboard" => Some(DEV_KEYBOARD),
        "console" => Some(DEV_CONSOLE),
        "kbd" => Some(DEV_KBD),
        _ => None,
    }
}

/// Parsa un nome nodo disco Linux ("sda".."sdp", "sda1"..) in handle codificato
/// (disco<<16|sub, 0 = whole-disk). SOLO per gli open raw `/dev/sdX` (rel
/// vuota): il mount (Fase 16c) risolve l'handle presso userdisk via
/// DISK_RESOLVE invece di indovinarlo qui. Ritorna None se non e' un nome disco.
fn disk_handle(name: &str) -> Option<u32> {
    let rest = name.strip_prefix("sd")?;
    let mut chars = rest.chars();
    let letter = chars.next()?;
    if !('a'..='p').contains(&letter) {
        return None;
    }
    let disk = (letter as u32) - ('a' as u32);
    let tail: String = chars.collect();
    let sub = if tail.is_empty() {
        0
    } else {
        let n: u32 = tail.parse().ok()?;
        if n == 0 || n > 64 {
            return None;
        }
        n
    };
    Some((disk << 16) | sub)
}

// ── Mount locali dinamici (Fase 16b) ─────────────────────────────────
// Tabella VFS userspace (nessun kernel coinvolto, ADR-0005): binding
// target → filesystem montato. La radice resta sempre ramfs. Il contenitore
// e' generico (`FsMount` + `MountedFs`): oggi solo FAT32, domani ext2/ISO9660
// aggiungono una variante senza reshuffle della tabella.

/// Filesystem montato su un target. Le varianti tengono l'istanza viva
/// (parser + client); `None` = spec registrata ma inattiva (sorgente assente
/// all'ultimo tentativo: gli accessi sotto il target falliscono invece di
/// finire shadow in ramfs, e il prossimo accesso ritenta l'attivazione).
enum MountedFs {
    Fat(Option<Fat32<IpcDisk>>),
}

/// Mount locale: binding target → sorgente + istanza.
struct FsMount {
    /// Target normalizzato senza slash ("fat", "mnt").
    target: String,
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
const STATIC_MOUNTS: &[(&str, &str)] = &[("UUID=5253544F", "fat")];

/// Normalizza un target ("//mnt//" → "mnt"). Rifiuta root, vuoti, `.`/`..`.
fn normalize_target(target: &str) -> Option<String> {
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
fn resolve_mount_source(source: &str) -> Option<u32> {
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
fn apply_mount_spec(
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
fn reactivate_mount(mounts: &mut Vec<FsMount>, mi: usize, fgen: &mut u64) -> bool {
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
    fn fat(&self) -> Option<&Fat32<IpcDisk>> {
        match &self.fs {
            MountedFs::Fat(opt) => opt.as_ref(),
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
    fn note_peer_death(&mut self, dead_chan: u64) -> bool {
        if let MountedFs::Fat(Some(f)) = &self.fs {
            if f.disk().note_peer_death(dead_chan) {
                self.fs = MountedFs::Fat(None);
                return true;
            }
        }
        false
    }

    /// true se il mount e' attivo (istanza viva).
    fn is_active(&self) -> bool {
        match &self.fs {
            MountedFs::Fat(opt) => opt.is_some(),
        }
    }
}

/// Match puro target (longest prefix, SENZA attivazione): true se il path e'
/// sotto un mount FAT noto (anche inattivo). Usato per rifiutare le op di
/// scrittura/creazione ramfs sotto target FAT (niente shadow).
fn target_match(mounts: &[FsMount], path: &str) -> bool {
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
fn resolve_fsmount<'a>(mounts: &mut Vec<FsMount>, path: &'a str, fgen: &mut u64) -> Option<(usize, &'a str)> {
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

/// Converte `Option<u64>` in valore IPC: `Some(v)` → `v`, `None` → `ERR`.
#[inline]
fn to_reply(val: Option<u64>) -> u64 {
    val.unwrap_or(ERR)
}

// ── ramfs ──────────────────────────────────────────────────────────

#[derive(Clone)]
#[allow(dead_code)] // `mode`: placeholder Strato 0 (16b), enforcement futuro
enum FsNode {
    File { data: Vec<u8>, mode: u32 },
    Dir { entries: BTreeMap<String, FsNode>, mode: u32 },
}

/// Mode Unix di default (placeholder Strato 0, Fase 16b): conservati, MAI
/// enforcement (nessun uid nel sistema; i check R/W/X arrivano col login
/// boundary, futuro). FAT e' mappata fissa a mount (file 0o444, dir 0o555:
/// placeholder, l'enforcement non esiste; FAT e' scrivibile dalla Fase 20).
pub const MODE_FILE_DEF: u32 = 0o666;
pub const MODE_DIR_DEF: u32 = 0o777;
pub const MODE_FAT_FILE: u32 = 0o444;
pub const MODE_FAT_DIR: u32 = 0o555;

struct RamFs {
    root: BTreeMap<String, FsNode>,
}

impl RamFs {
    fn new() -> Self {
        Self { root: BTreeMap::new() }
    }

    /// Trova un nodo per path (es. "hello.txt" o "dir/file.txt").
    /// Ritorna il nodo finale (file o dir); i componenti intermedi devono
    /// essere directory (altrimenti None, come ENOTDIR).
    fn find(&self, path: &str) -> Option<&FsNode> {
        if path.is_empty() || path == "/" {
            return None;
        }
        // Componenti in scratch (mai heap: 1 alloc per lookup prima). Two-pass:
        // conta poi riempi — il path e' minuscolo, la doppia scansione e'
        // trascurabile contro una free-list round-trip.
        let t = path.trim_start_matches('/');
        let n = t.split('/').count();
        let parts_buf = libr::scratch::alloc_slice::<&str>(n)?;
        for (i, comp) in t.split('/').enumerate() {
            parts_buf[i] = comp;
        }
        let parts = &parts_buf[..n];
        let mut current_dir = &self.root;
        for (i, &part) in parts.iter().enumerate() {
            let node = current_dir.get(part)?;
            if i == parts.len() - 1 {
                return Some(node);
            }
            match node {
                FsNode::Dir { entries: d, .. } => current_dir = d,
                _ => return None,
            }
        }
        None
    }

    /// Trova o crea un nodo per path (crea le directory intermedie).
    fn find_or_create(&mut self, path: &str) -> Option<&mut FsNode> {
        // Componenti in scratch come `find` (i `String::from` sotto restano
        // heap: vivono nell'albero ramfs oltre la richiesta, mai scratch).
        let t = path.trim_start_matches('/');
        let n = t.split('/').count();
        let parts_buf = libr::scratch::alloc_slice::<&str>(n)?;
        for (i, comp) in t.split('/').enumerate() {
            parts_buf[i] = comp;
        }
        let parts = &parts_buf[..n];
        if parts.is_empty() || parts[0].is_empty() {
            return None;
        }

        let mut current = &mut self.root;
        for (i, &part) in parts.iter().enumerate() {
            if i == parts.len() - 1 {
                current.entry(String::from(part))
                    .or_insert_with(|| FsNode::File { data: Vec::new(), mode: MODE_FILE_DEF });
                return current.get_mut(part);
            }
            let entry = current.entry(String::from(part))
                .or_insert_with(|| FsNode::Dir { entries: BTreeMap::new(), mode: MODE_DIR_DEF });
            match entry {
                FsNode::Dir { entries: dir, .. } => current = dir,
                _ => return None,
            }
        }
        None
    }

    /// Crea un file vuoto se non esiste, ritorna il nodo.
    fn create_file(&mut self, path: &str) -> Option<&mut Vec<u8>> {
        let node = self.find_or_create(path)?;
        match node {
            FsNode::File { data, .. } => Some(data),
            FsNode::Dir { .. } => None,
        }
    }

    /// Lista le entry di una directory.
    fn readdir(&self, path: &str) -> Option<Vec<String>> {
        if path.is_empty() || path == "/" {
            return Some(self.root.keys().cloned().collect());
        }
        let node = self.find(path)?;
        match node {
            FsNode::Dir { entries, .. } => Some(entries.keys().cloned().collect()),
            _ => None,
        }
    }

    /// Crea una directory al path specificato (crea le directory intermedie).
    fn mkdir(&mut self, path: &str) -> Option<()> {
        let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        if parts.is_empty() || parts[0].is_empty() {
            return None;
        }
        let mut current = &mut self.root;
        for (i, &part) in parts.iter().enumerate() {
            if i == parts.len() - 1 {
                current.entry(String::from(part))
                    .or_insert_with(|| FsNode::Dir { entries: BTreeMap::new(), mode: MODE_DIR_DEF });
                return Some(());
            }
            let entry = current.entry(String::from(part))
                .or_insert_with(|| FsNode::Dir { entries: BTreeMap::new(), mode: MODE_DIR_DEF });
            match entry {
                FsNode::Dir { entries: dir, .. } => current = dir,
                _ => return None,
            }
        }
        None
    }

    /// Cancella un file o una directory VUOTA (Fase 18.2, `R_DELETE`).
    /// Directory non vuote, root e path inesistenti → None. Non crea nulla.
    fn remove(&mut self, path: &str) -> Option<()> {
        let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        if parts.is_empty() || parts[0].is_empty() {
            return None;
        }
        let mut current = &mut self.root;
        for (i, &part) in parts.iter().enumerate() {
            if i == parts.len() - 1 {
                // File, o dir vuota: rimuovibile. Dir non vuota, root o
                // assente: rifiuto (niente `remove` sotto borrow attivo).
                let ok = match current.get(part) {
                    Some(FsNode::File { .. }) => true,
                    Some(FsNode::Dir { entries, .. }) => entries.is_empty(),
                    _ => false,
                };
                if !ok {
                    return None;
                }
                current.remove(part);
                return Some(());
            }
            match current.get_mut(part) {
                Some(FsNode::Dir { entries: dir, .. }) => current = dir,
                _ => return None,
            }
        }
        None
    }
}

// ── Open file table ────────────────────────────────────────────────

enum FileEntry {
    /// File locale: `path` e' relativo al suo filesystem (ramfs: path assoluto
    /// senza slash iniziale; FAT: relativo al mount). `mnt` = indice in
    /// `mounts_fat` per i file FAT, None per ramfs (radice sempre locale).
    /// `fat_info`/`fat_gen`: FileInfo in cache per i file FAT (solo FAT: il
    /// find in ramfs e' in-memoria e costa zero). La cache evita il dir-walk
    /// (root + dir: ~4-10 round-trip DISK) a OGNI read/write su fd aperti: il
    /// load di un binario da 30 KB faceva ~15 find × walk. Validita': la
    /// generazione globale `fat_gen` viene bumpata a OGNI mutazione FAT
    /// (write/create/mount/umount/remount/drop d'epoca); a mismatch si rifa
    /// `find` e si riaggiorna. Mai stale oltre l'op corrente (single-thread).
    Local { path: String, kind: FsKind, offset: usize, mnt: Option<usize>, fat_info: Option<FileInfo>, fat_gen: u64 },
    Remote { server_chan: u64, remote_fd: u32 },
}

struct FileTable {
    files: BTreeMap<(u64, u32), FileEntry>,
    next_fd: BTreeMap<u64, u32>,
}

impl FileTable {
    fn new() -> Self {
        Self {
            files: BTreeMap::new(),
            next_fd: BTreeMap::new(),
        }
    }

    fn alloc_fd(&mut self, chan: u64) -> u32 {
        let fd = self.next_fd.entry(chan).or_insert(1);
        let current = *fd;
        *fd += 1;
        current
    }

    fn open(&mut self, chan: u64, path: &str, kind: FsKind, mnt: Option<usize>) -> u64 {
        let fd = self.alloc_fd(chan);
        self.files.insert((chan, fd), FileEntry::Local {
            path: String::from(path),
            kind,
            offset: 0,
            mnt,
            fat_info: None,
            fat_gen: 0,
        });
        fd as u64
    }

    /// Come `open` ma con FileInfo FAT gia' risolto (evita un find al primo
    /// uso): `gen` e' la generazione corrente (la cache nasce valida).
    fn open_fat(&mut self, chan: u64, path: &str, mnt: usize, info: FileInfo, fgen: u64) -> u64 {
        let fd = self.alloc_fd(chan);
        self.files.insert((chan, fd), FileEntry::Local {
            path: String::from(path),
            kind: FsKind::Fat,
            offset: 0,
            mnt: Some(mnt),
            fat_info: Some(info),
            fat_gen: fgen,
        });
        fd as u64
    }

    fn open_remote(&mut self, chan: u64, server_chan: u64, remote_fd: u32) -> u64 {
        let fd = self.alloc_fd(chan);
        self.files.insert((chan, fd), FileEntry::Remote { server_chan, remote_fd });
        fd as u64
    }

    fn close(&mut self, chan: u64, fd: u32) -> bool {
        self.files.remove(&(chan, fd)).is_some()
    }

    /// Purga TUTTO lo stato del canale `chan` (morte del peer, notifica
    /// unificata Fase 14): fd locali e remoti + contatore next_fd. Raccoglie
    /// in `remotes` le coppie `(server_chan, remote_fd)` da chiudere presso
    /// i driver con DEV_CLOSE (il chiamante lo fa best-effort).
    fn purge(&mut self, chan: u64, remotes: &mut Vec<(u64, u32)>) {
        self.files.retain(|&(c, _), e| {
            if c != chan {
                return true;
            }
            if let FileEntry::Remote { server_chan, remote_fd } = e {
                remotes.push((*server_chan, *remote_fd));
            }
            false
        });
        self.next_fd.remove(&chan);
    }

    /// true se qualche fd locale e' aperto su questo mount (EBUSY per umount).
    fn has_mount_users(&self, mi: usize) -> bool {
        self.files.values().any(|e| match e {
            FileEntry::Local { mnt: Some(m), .. } => *m == mi,
            _ => false,
        })
    }

    fn get(&self, chan: u64, fd: u32) -> Option<(&str, FsKind, usize, Option<usize>)> {
        match self.files.get(&(chan, fd))? {
            FileEntry::Local { path, kind, offset, mnt, .. } => {
                Some((path.as_str(), *kind, *offset, *mnt))
            }
            FileEntry::Remote { .. } => None,
        }
    }

    fn get_remote(&self, chan: u64, fd: u32) -> Option<(u64, u32)> {
        match self.files.get(&(chan, fd))? {
            FileEntry::Remote { server_chan, remote_fd } => Some((*server_chan, *remote_fd)),
            _ => None,
        }
    }

    fn set_offset(&mut self, chan: u64, fd: u32, offset: usize) {
        if let Some(FileEntry::Local { offset: o, .. }) = self.files.get_mut(&(chan, fd)) {
            *o = offset;
        }
    }

    /// Aggiorna la cache FileInfo del fd (dopo una scrittura che puo' aver
    /// cambiato size/first_cluster): `None` se l'entry non e' un file FAT.
    fn refresh_fat_info(&mut self, chan: u64, fd: u32, info: Option<FileInfo>, fgen: u64) {
        if let Some(FileEntry::Local { kind: FsKind::Fat, fat_info, fat_gen, .. }) =
            self.files.get_mut(&(chan, fd))
        {
            *fat_info = info;
            *fat_gen = fgen;
        }
    }
}

/// FileInfo del fd (solo file FAT): cache per-fd con generazione (vedi
/// `FileEntry`). A mismatch di generazione o cache assente rifa `find` sul
/// mount (gia' riattivato dal chiamante) e aggiorna la cache. Ritorna None se
/// il fd non e' un file FAT o il file non esiste piu'.
fn fd_fat_info(
    ftable: &mut FileTable,
    fat: &Fat32<IpcDisk>,
    chan: u64,
    fd: u32,
    fgen: u64,
) -> Option<FileInfo> {
    let rel_owned = {
        let e = ftable.files.get(&(chan, fd))?;
        match e {
            FileEntry::Local { fat_info: Some(info), fat_gen: g, kind: FsKind::Fat, .. }
                if *g == fgen =>
            {
                return Some(*info)
            }
            FileEntry::Local { path, kind: FsKind::Fat, .. } => path.clone(),
            _ => return None,
        }
    };
    let info = fat.find(&rel_owned)?;
    if let Some(FileEntry::Local { fat_info, fat_gen: g, .. }) = ftable.files.get_mut(&(chan, fd)) {
        *fat_info = Some(info);
        *g = fgen;
    }
    Some(info)
}

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
struct ChanRights {
    ops: u32,
    subtree: String,
}

/// Mask ops effettiva (default ALL a entry assente).
fn rights_ops(rights: &BTreeMap<u64, ChanRights>, chan: u64) -> u32 {
    rights.get(&chan).map_or(libr::RIGHTS_ALL, |r| r.ops)
}

/// Subtree effettivo (default root "" a entry assente).
fn rights_subtree<'a>(rights: &'a BTreeMap<u64, ChanRights>, chan: u64) -> &'a str {
    rights.get(&chan).map_or("", |r| r.subtree.as_str())
}

/// Normalizza subtree/path ("//fat//" → "fat", "/" o "" → "").
fn normalize_sub(path: &str) -> String {
    String::from(path.trim().trim_matches('/'))
}

/// Vista normalizzata (solo trim, ZERO alloc): per i CHECK per-op nel choke
/// point (ogni op con path la attraversa). Per gli STORE nella tabella diritti
/// (long-lived oltre la richiesta) resta `normalize_sub` owned.
fn normalize_sub_view(path: &str) -> &str {
    path.trim().trim_matches('/')
}

/// true se il path normalizzato `p` e' dentro il subtree `sub` ("" = root).
fn within_subtree(sub: &str, p: &str) -> bool {
    sub.is_empty()
        || p == sub
        || (p.len() > sub.len()
            && p.as_bytes().get(sub.len()) == Some(&b'/')
            && p.starts_with(sub))
}

/// Bit ops richiesto dall'op_tag. None = sempre consentito (CLOSE, DROP, GET).
fn op_bit(op_tag: u32) -> Option<u32> {
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
fn handle_rights_drop(
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
fn handle_rights_get(
    rights: &BTreeMap<u64, ChanRights>,
    rings: &BTreeMap<u64, (u64, u64)>,
    chan: u64,
) -> Option<u64> {
    let (ops, sub) = match rights.get(&chan) {
        Some(r) => (r.ops, r.subtree.as_str()),
        None => (libr::RIGHTS_ALL, ""),
    };
    if rings.contains_key(&chan) {
        map_client_resp_ring(rings, chan);
        resp_ring_write(ops as u64, sub.len() as u64, sub.as_bytes());
    }
    Some(ops as u64)
}

// ── Ring I/O (Fase 10.2) ─────────────────────────────────────────

/// Legge head e tail dal ring a `ring_va`.
unsafe fn ring_positions(ring_va: u64) -> (u32, u32) {
    let head = unsafe { core::ptr::read_volatile((ring_va + RING_HEAD as u64) as *const u32) };
    let tail = unsafe { core::ptr::read_volatile((ring_va + RING_TAIL as u64) as *const u32) };
    (head, tail)
}

/// Byte disponibili nel ring.
fn ring_available(head: u32, tail: u32) -> usize {
    ((head + RING_DATA_CAP as u32 - tail) % RING_DATA_CAP as u32) as usize
}

/// Legge `count` byte dal ring a `ring_va` dalla posizione `pos`.
unsafe fn ring_read_at(ring_va: u64, pos: u32, dst: &mut [u8], count: usize) {
    let src = ring_va as *const u8;
    for i in 0..count.min(dst.len()) {
        let p = ((pos as usize) + i) % RING_DATA_CAP;
        dst[i] = unsafe { core::ptr::read_volatile(src.add(p)) };
    }
}

/// Scrive `data` nel ring a `ring_va` dalla posizione `pos`.
unsafe fn ring_write_at(ring_va: u64, pos: u32, data: &[u8]) {
    let dst = ring_va as *mut u8;
    for (i, byte) in data.iter().enumerate() {
        let p = ((pos as usize) + i) % RING_DATA_CAP;
        unsafe { core::ptr::write_volatile(dst.add(p), *byte); }
    }
}

/// Legge un request frame dal request ring. Formato: [tag:4][w0:8][w1:8][payload].
/// Ritorna (tag, w0, w1, payload_len) o None se il ring e' vuoto.
fn req_ring_read() -> Option<(u32, u64, u64, usize)> {
    unsafe {
        let (head, tail) = ring_positions(REQ_RING_VA);
        if ring_available(head, tail) < 20 {
            return None;
        }
        let mut tag_bytes = [0u8; 4];
        ring_read_at(REQ_RING_VA, tail, &mut tag_bytes, 4);
        let tag = u32::from_le_bytes(tag_bytes);
        let mut w0_bytes = [0u8; 8];
        ring_read_at(REQ_RING_VA, tail + 4, &mut w0_bytes, 8);
        let w0 = u64::from_le_bytes(w0_bytes);
        let mut w1_bytes = [0u8; 8];
        ring_read_at(REQ_RING_VA, tail + 12, &mut w1_bytes, 8);
        let w1 = u64::from_le_bytes(w1_bytes);
        let total = ring_available(head, tail);
        let payload_len = if total > 20 { total - 20 } else { 0 };
        Some((tag, w0, w1, payload_len))
    }
}

/// Legge i payload bytes dal request ring (dopo tag+w0+w1).
fn req_ring_read_payload(dst: &mut [u8], payload_len: usize) {
    unsafe {
        let (_, tail) = ring_positions(REQ_RING_VA);
        ring_read_at(REQ_RING_VA, (tail + 20) % RING_DATA_CAP as u32, dst, payload_len);
        let total = 20 + payload_len;
        let new_tail = ((tail as usize) + total) % RING_DATA_CAP;
        core::ptr::write_volatile((REQ_RING_VA + RING_TAIL as u64) as *mut u32, new_tail as u32);
    }
}

/// Avanza la tail del request ring (per frame letti senza payload).
fn req_ring_consume(frame_len: usize) {
    unsafe {
        let (_, tail) = ring_positions(REQ_RING_VA);
        let new_tail = ((tail as usize) + frame_len) % RING_DATA_CAP;
        core::ptr::write_volatile((REQ_RING_VA + RING_TAIL as u64) as *mut u32, new_tail as u32);
    }
}

/// Resync del request ring del client (tail=head, scarta tutto): il frame in
/// testa e' impossibile (tag sconosciuto o incompleto) e qualunque consumo lo
/// disallineerebbe per sempre (osservato: tag=0x0, RINGFULL lato client e
/// stallo senza recovery). Il mittente riceve ERR; i client ritentano
/// (tty: flush riprova, pump alla prossima notify) o vedono -1 (sync).
/// Sicuro: il mittente scrive un frame intero prima di notificare, quindi
/// scartare qui non taglia mai un frame valido a meta'.
fn req_resync() {
    unsafe {
        let (head, _) = ring_positions(REQ_RING_VA);
        core::ptr::write_volatile((REQ_RING_VA + RING_TAIL as u64) as *mut u32, head);
    }
    println!("[userfs] resync request ring (frame impossibile, tail=head)");
}

/// Scrive un response frame nel response ring. Formato: [result:8][w1:8][payload].
fn resp_ring_write(result: u64, w1: u64, payload: &[u8]) {
    let frame_len = 16 + payload.len();
    unsafe {
        let (head, _tail) = ring_positions(RESP_RING_VA);
        let mut hdr = [0u8; 16];
        hdr[0..8].copy_from_slice(&result.to_le_bytes());
        hdr[8..16].copy_from_slice(&w1.to_le_bytes());
        ring_write_at(RESP_RING_VA, head, &hdr);
        if !payload.is_empty() {
            ring_write_at(RESP_RING_VA, (head + 16) % RING_DATA_CAP as u32, payload);
        }
        let new_head = ((head as usize) + frame_len) % RING_DATA_CAP;
        core::ptr::write_volatile((RESP_RING_VA + RING_HEAD as u64) as *mut u32, new_head as u32);
    }
}

/// Mappa il request ring del client a REQ_RING_VA nello spazio di userfs.
fn map_client_req_ring(rings: &BTreeMap<u64, (u64, u64)>, chan: u64) -> bool {
    if let Some(&(req_phys, _)) = rings.get(&chan) {
        if libr::map_physical(req_phys, REQ_RING_VA, 1).is_ok() {
            return true;
        }
    }
    false
}

/// Mappa il response ring del client a RESP_RING_VA nello spazio di userfs.
fn map_client_resp_ring(rings: &BTreeMap<u64, (u64, u64)>, chan: u64) -> bool {
    if let Some(&(_, resp_phys)) = rings.get(&chan) {
        if libr::map_physical(resp_phys, RESP_RING_VA, 1).is_ok() {
            return true;
        }
    }
    false
}

// ── Handler (Option<u64> internamente) ─────────────────────────────

fn handle_open(
    fs: &mut RamFs,
    ftable: &mut FileTable,
    mounts_fat: &mut Vec<FsMount>,
    mounts: &[Mount],
    chan: u64,
    flags: u64,
    path: &str,
    fgen: &mut u64,
) -> Option<u64> {
    if path.is_empty() || path.len() > MAX_PATH {
        return None;
    }

    // Cerca nei mount point registrati (devfs, console, userdisk, futuri driver).
    if let Some((driver_chan, rel)) = resolve_mount(path, mounts) {
        // Nodo disco raw (Fase 16/16d): open("/dev/sda") o degli alias
        // stabili ("/dev/disk/by-uuid/<H>", "/dev/disk/by-label/<N>") matcha
        // il prefix del nodo stesso (rel vuota) — e cosi' i device registrati
        // per-nome ("/dev/null": rel vuota sul prefix esatto, Fase 16d).
        // L'handle/tipo si chiede al driver (nomi `sdX` via parse locale +
        // validazione, by-path via DISK_RESOLVE, device via dev_type
        // sull'ultimo componente). Impossibile o driver irraggiungibile →
        // None (-1, mai wedge).
        if rel.is_empty() {
            let prefix = path.trim_start_matches('/');
            if let Some(name) = prefix.strip_prefix("dev/") {
                // Alias stabili (by-uuid/by-label): SOLO qui si interroga
                // userdisk (DISK_RESOLVE). Un open di /dev/null NON deve fare
                // un round-trip al disco a ogni chiamata (il flood di t30 lo
                // amplificava a ~1000 HELLO).
                if name.starts_with("disk/by-") {
                    let h = resolve_mount_source(&alloc::format!("/dev/{}", name))?;
                    let reply = libr::send(driver_chan, DEV_OPEN, h as u64, 0).ok()?;
                    if reply.w0 == ERR {
                        return None;
                    }
                    return Some(ftable.open_remote(chan, driver_chan, reply.w0 as u32));
                }
                // Nodo disco raw (/dev/sda, /dev/sdb...): parse locale
                // validato dal driver con DEV_OPEN.
                if let Some(h) = disk_handle(name) {
                    let reply = libr::send(driver_chan, DEV_OPEN, h as u64, 0).ok()?;
                    if reply.w0 == ERR {
                        return None;
                    }
                    return Some(ftable.open_remote(chan, driver_chan, reply.w0 as u32));
                }
                // Device registrato per-nome (null/zero/...): tipo dall'ultimo
                // componente del prefix esatto.
                if let Some(dev) = name.rsplit('/').next().and_then(dev_type) {
                    let reply = libr::send(driver_chan, DEV_OPEN, dev, 0).ok()?;
                    if reply.w0 == ERR {
                        return None;
                    }
                    return Some(ftable.open_remote(chan, driver_chan, reply.w0 as u32));
                }
            }
            return None;
        }
        let device_type = dev_type(rel)?;
        let reply = libr::send(driver_chan, DEV_OPEN, device_type, 0).ok()?;
        let remote_fd = reply.w0 as u32;
        return Some(ftable.open_remote(chan, driver_chan, remote_fd));
    }

    // Filesystem locali: prima i mount FAT (con attivazione lazy), poi ramfs.
    // resolve_local copre ramfs + il caso "mount noto ma inattivo" (→ None,
    // mai shadow in ramfs: stesso contratto di prima).
    if let Some((mi, rel)) = resolve_fsmount(mounts_fat, path, fgen) {
        // POSIX come ramfs (20.4): con O_CREAT crea l'entry 8.3 se manca
        // (no LFN, mkdir-su-FAT fuori scope); senza, il file deve esistere.
        // La creazione muta la directory: bumpa la generazione (invalida le
        // cache FileInfo: la nuova entry cambia il layout dir).
        if flags as u32 & libr::O_CREAT != 0 {
            if let Some(fat) = mounts_fat.get(mi).and_then(|m| m.fat()) {
                if fat.find(rel).is_none() {
                    let _ = fat.create_file(rel);
                    *fgen = fgen.wrapping_add(1);
                }
            }
        }
        let fat = mounts_fat[mi].fat()?;
        let info = fat.find(rel)?;
        return Some(ftable.open_fat(chan, rel, mi, info, *fgen));
    }
    match resolve_local(mounts_fat, path)? {
        FsKind::Fat => None, // mount inattivo: errore, mai shadow ramfs
        FsKind::Ram => {
            // POSIX (Fase 18.2, ADR-0015): senza O_CREAT il file deve
            // esistere — mai creare. Con O_CREAT, crea se manca (su dir
            // esistente resta apribile come prima: create_file → None
            // ignorato, poi find).
            if flags as u32 & libr::O_CREAT != 0 {
                let _ = fs.create_file(path);
            }
            fs.find(path)?;
            Some(ftable.open(chan, path, FsKind::Ram, None))
        }
    }
}

fn handle_read(
    fs: &RamFs,
    ftable: &mut FileTable,
    mounts_fat: &mut Vec<FsMount>,
    rings: &BTreeMap<u64, (u64, u64)>,
    chan: u64,
    fd: u32,
    count: usize,
    fgen: &mut u64,
) -> Option<u64> {
    if count > 4096 {
        return None;
    }

    // File remoto: inoltro al driver. userfs inietta entrambi i ring del
    // client nel driver (`map_in`): il driver legge dalla request ring e
    // scrive nella response ring → zero copie (Fase 10.2).
    if let Some((driver_chan, remote_fd)) = ftable.get_remote(chan, fd) {
        let (req_phys, resp_phys) = match rings.get(&chan) {
            Some(&r) => r,
            None => return None,
        };
        libr::map_in(driver_chan, req_phys, libr::CLI_REQ_VA, 1).ok()?;
        libr::map_in(driver_chan, resp_phys, libr::CLI_RESP_VA, 1).ok()?;
        let reply = libr::send(driver_chan, DEV_READ, remote_fd as u64, count as u64).ok()?;
        return Some(reply.w0);
    }
    if ftable.get(chan, fd).is_none() {
        return None;
    }

    let (path, kind, offset, mnt) = ftable.get(chan, fd)?;

    // P1.2 — buffer di risposta sullo stack (count ≤ 4096 per il check in
    // testa): niente `to_vec()`/`Vec` temporanei per-op (la free-list
    // dell'heap di userfs cresceva di ~1 blocco a op FAT → scansioni O(n)
    // su tutte le op successive; vedi read_dir in fat32.rs).
    let mut buf_stack = [0u8; 4096];
    let data: &[u8] = match kind {
        FsKind::Ram => {
            let d = match fs.find(path)? {
                FsNode::File { data: d, .. } => d,
                _ => return None,
            };
            if offset >= d.len() {
                &[]
            } else {
                let end = (offset + count).min(d.len());
                let n = end - offset;
                buf_stack[..n].copy_from_slice(&d[offset..end]);
                &buf_stack[..n]
            }
        }
        FsKind::Fat => {
            let mi = mnt?;
            // Il mount puo' essere caduto inattivo alla morte di userdisk
            // (drop d'epoca in `note_peer_death`): riattiva per nome qui, come
            // `resolve_fsmount` fa per open/readdir (fail-loud, mai shadow).
            if !reactivate_mount(mounts_fat, mi, fgen) {
                return None;
            }
            let g = *fgen;
            let fat = mounts_fat.get(mi)?.fat()?;
            let info = fd_fat_info(ftable, fat, chan, fd, g)?;
            let n = fat.read_file(&info, offset, count, &mut buf_stack[..count]);
            &buf_stack[..n]
        }
    };

    let bytes_read = data.len();
    // Frame SEMPRE, anche vuoto a EOF (Fase 18.2-bis): senza, il client non
    // trova risposta e riporta -1 invece di 0. Stesso contratto dei driver
    // (console scrive sempre il frame). Short (< count) o vuoto (0) = EOF.
    if let Some(&(_, _)) = rings.get(&chan) {
        map_client_resp_ring(rings, chan);
        resp_ring_write(bytes_read as u64, 0, &data);
    }
    ftable.set_offset(chan, fd, offset + bytes_read);
    Some(bytes_read as u64)
}

/// File remoto: inoltro al driver. Il payload del WRITE RESTA nel request ring
/// del client (zero copy): userfs inietta entrambi i ring del client nel driver
/// (`map_in`), il driver legge i dati direttamente dal request ring e avanza la
/// tail (SPSC). La chiamata NON deve consumare il frame nel request ring.
/// Ritorna i byte accettati dal driver (reply.w0).
fn handle_write_remote(
    ftable: &FileTable,
    rings: &BTreeMap<u64, (u64, u64)>,
    chan: u64,
    fd: u32,
    count: usize,
) -> Option<u64> {
    let (driver_chan, remote_fd) = match ftable.get_remote(chan, fd) {
        Some(r) => r,
        None => return None,
    };
    let (req_phys, resp_phys) = match rings.get(&chan) {
        Some(&r) => r,
        None => return None,
    };
    libr::map_in(driver_chan, req_phys, libr::CLI_REQ_VA, 1).ok()?;
    libr::map_in(driver_chan, resp_phys, libr::CLI_RESP_VA, 1).ok()?;
    let reply = libr::send(driver_chan, DEV_WRITE, remote_fd as u64, count as u64).ok()?;
    Some(reply.w0)
}

/// Write locale (ramfs): il frame e' gia' stato consumato e il payload e' in
/// Scrive `count` byte di `payload` sul fd (ramfs o FAT-overwrite). FAT32 e'
/// scrivibile dalla Fase 20 (write-through, niente cache): solo overwrite
/// entro la size esistente (20.2) — la crescita/creazione arrivano dopo.
fn handle_write_local(
    fs: &mut RamFs,
    ftable: &mut FileTable,
    mounts_fat: &mut Vec<FsMount>,
    chan: u64,
    fd: u32,
    count: usize,
    payload: &[u8],
    fgen: &mut u64,
) -> Option<u64> {
    let (path, kind, offset, mnt) = ftable.get(chan, fd)?;
    if kind == FsKind::Fat {
        // Scrittura FAT (Fase 20): overwrite + crescita con allocazione
        // (write-through, niente cache FileInfo: la scrittura puo' cambiare
        // size/first_cluster, quindi dopo si bumpa la generazione e si
        // riaggiorna la cache con un find fresco — un find per write, rumore
        // contro le centinaia di round-trip DISK della scrittura stessa).
        let mi = mnt?;
        if !reactivate_mount(mounts_fat, mi, fgen) {
            return None;
        }
        let g = *fgen;
        let fat = mounts_fat.get(mi)?.fat()?;
        let info = fd_fat_info(ftable, fat, chan, fd, g)?;
        if info.is_dir {
            return None;
        }
        let n = fat.write_grow(&info, offset, &payload[..count.min(payload.len())]);
        *fgen = fgen.wrapping_add(1);
        let g2 = *fgen;
        // Rileggi l'entry dopo la mutazione (size/first_cluster possono aver
        // cambiato valore): la cache resta valida alla nuova generazione.
        let fat = mounts_fat.get(mi)?.fat()?;
        let rel: String = match ftable.files.get(&(chan, fd)) {
            Some(FileEntry::Local { path, kind: FsKind::Fat, .. }) => path.clone(),
            _ => return Some(n as u64),
        };
        let fresh = fat.find(&rel);
        ftable.refresh_fat_info(chan, fd, fresh, g2);
        ftable.set_offset(chan, fd, offset + n);
        return Some(n as u64);
    }

    match fs.find_or_create(path)? {
        FsNode::File { data: file_data, .. } => {
            if offset + count > file_data.len() {
                file_data.resize(offset + count, 0);
            }
            file_data[offset..offset + count].copy_from_slice(&payload[..count]);
            ftable.set_offset(chan, fd, offset + count);
            Some(count as u64)
        }
        _ => None,
    }
}

fn handle_close(ftable: &mut FileTable, chan: u64, fd: u32) -> Option<u64> {
    // File remoto: chiudi anche sul server.
    if let Some((driver_chan, remote_fd)) = ftable.get_remote(chan, fd) {
        let _ = libr::send(driver_chan, DEV_CLOSE, remote_fd as u64, 0);
    }
    if ftable.close(chan, fd) { Some(0) } else { None }
}

fn handle_readdir(
    fs: &RamFs,
    mounts_fat: &mut Vec<FsMount>,
    mounts: &[Mount],
    rings: &BTreeMap<u64, (u64, u64)>,
    chan: u64,
    path: &str,
    fgen: &mut u64,
) -> Option<u64> {
    // Directory remota (device): inoltro al driver, che scrive le entry nella
    // response ring del client (mappata li' da map_in).
    if let Some((driver_chan, _rel)) = resolve_mount(path, mounts) {
        let (req_phys, resp_phys) = rings.get(&chan)?;
        libr::map_in(driver_chan, *req_phys, libr::CLI_REQ_VA, 1).ok()?;
        libr::map_in(driver_chan, *resp_phys, libr::CLI_RESP_VA, 1).ok()?;
        let reply = libr::send(driver_chan, DEV_READDIR, 0, 0).ok()?;
        return Some(reply.w0);
    }

    if let Some((mi, rel)) = resolve_fsmount(mounts_fat, path, fgen) {
        let fat = mounts_fat[mi].fat()?;
        let entries: Vec<String> =
            fat.list_dir(rel).into_iter().map(|d| d.name).collect();
        // Mount annidati sotto dir FAT (edge raro, gratis col design union).
        let entries = union_mount_children(entries, mounts, mounts_fat, path);
        let mut buf = Vec::new();
        for entry in &entries {
            buf.extend_from_slice(entry.as_bytes());
            buf.push(0);
        }
        buf.push(0);
        if let Some(&(_, _)) = rings.get(&chan) {
            map_client_resp_ring(rings, chan);
            resp_ring_write(entries.len() as u64, 0, &buf);
        }
        return Some(entries.len() as u64);
    }

    // Listing sintetizzato dai prefix registrati (Fase 16d, discovery):
    // se `path` e' directory padre di prefix noti (es. "/dev",
    // "/dev/disk/by-uuid") elenca i figli immediati. Solo dove ramfs/fat non
    // hanno la dir (mai shadow, mai cambi ai listing esistenti). Nota:
    // `resolve_local` esclude i path /dev/* (None) prima ancora di guardare
    // ramfs — la sintesi copre anche quelli.
    //
    // Fase 18.1-ter: i mount point si mergiano SEMPRE (union con dedupe, mai
    // shadow): `ls /` mostra ramfs + `fat` + `dev`. Solo nomi, mai contenuti:
    // il check subtree Fase 17 sul path richiesto resta prima del dispatch.
    // Directory esistente ma vuota resta OK (exists): solo "sconosciuto E
    // senza mount" e' errore.
    let (exists, base): (bool, Vec<String>) = match resolve_local(mounts_fat, path) {
        Some(FsKind::Ram) => match fs.readdir(path) {
            Some(e) => (true, e),
            None => (false, Vec::new()),
        },
        Some(FsKind::Fat) => return None, // mount noto ma inattivo: errore
        None => (false, Vec::new()),
    };
    let entries = union_mount_children(base, mounts, mounts_fat, path);
    if !exists && entries.is_empty() {
        return None;
    }

    let mut buf = Vec::new();
    for entry in &entries {
        buf.extend_from_slice(entry.as_bytes());
        buf.push(0);
    }
    buf.push(0);
    // Scrivi le entry nella response ring del client.
    if let Some(&(_, _)) = rings.get(&chan) {
        map_client_resp_ring(rings, chan);
        resp_ring_write(entries.len() as u64, 0, &buf);
    }
    Some(entries.len() as u64)
}

/// Scrive il response frame di R_STAT (`[size:8][kind:8]`, payload vuoto) e
/// ritorna 0 per il reply IPC (self-written: il dispatch non riscrive).
fn stat_reply(rings: &BTreeMap<u64, (u64, u64)>, chan: u64, size: u64, kind: u64) -> u64 {
    if rings.get(&chan).is_some() {
        map_client_resp_ring(rings, chan);
        resp_ring_write(size, kind, &[]);
    }
    0
}

/// R_STAT: metadati del path (Fase 19.2, zero kernel). Self-written come
/// read/readdir (frame `[size:8][kind:8]`, vedi `stat_reply`); None =
/// inesistente. Precedenza come open (mai shadow): device esatti → FAT (con
/// attivazione lazy) → ramfs → padri sintetizzati 16d → None. Mount FAT noto
/// ma inattivo = errore (stesso contratto di open/readdir). kind in
/// `syscall-numbers` (STAT_FILE/DIR/DEVICE + STAT_READONLY): ramfs da' len
/// reale (dir = 0, mai readonly), FAT size dalla dir entry (scrivibile dalla
/// Fase 20: mai readonly), device size 0 readonly 0 (sconosciuto senza
/// interrogare il driver: i prefix registrati sono foglie, qui mai contattati).
fn handle_stat(
    fs: &RamFs,
    mounts_fat: &mut Vec<FsMount>,
    mounts: &[Mount],
    rings: &BTreeMap<u64, (u64, u64)>,
    chan: u64,
    path: &str,
    fgen: &mut u64,
) -> Option<u64> {
    if path.is_empty() || path.len() > MAX_PATH {
        return None;
    }
    // Root ramfs: esiste sempre.
    if path == "/" {
        return Some(stat_reply(rings, chan, 0, libr::STAT_DIR));
    }
    // Device registrati: foglie (rel non vuota = path sotto un device: None,
    // come open che rifiuta i dev_type sconosciuti).
    if let Some((_driver_chan, rel)) = resolve_mount(path, mounts) {
        if rel.is_empty() {
            return Some(stat_reply(rings, chan, 0, libr::STAT_DEVICE));
        }
        return None;
    }
    // FAT con attivazione lazy; mount noto ma inattivo = errore, mai shadow.
    // (find fresco a ogni stat: niente fd, niente cache — i metadati non
    // devono mai essere stale.)
    if let Some((mi, rel)) = resolve_fsmount(mounts_fat, path, fgen) {
        let fat = mounts_fat[mi].fat()?;
        if rel.is_empty() {
            return Some(stat_reply(rings, chan, 0, libr::STAT_DIR));
        }
        let info = fat.find(rel)?;
        let kind = if info.is_dir { libr::STAT_DIR } else { libr::STAT_FILE };
        return Some(stat_reply(rings, chan, info.size as u64, kind));
    }
    match resolve_local(mounts_fat, path) {
        // Mount noto ma inattivo: errore, mai shadow ramfs.
        Some(FsKind::Fat) => None,
        Some(FsKind::Ram) => match fs.find(path) {
            Some(FsNode::File { data, .. }) => {
                Some(stat_reply(rings, chan, data.len() as u64, libr::STAT_FILE))
            }
            Some(FsNode::Dir { .. }) => {
                Some(stat_reply(rings, chan, 0, libr::STAT_DIR))
            }
            // Non in ramfs: puo' essere un padre sintetizzato (sotto).
            None => synth_children(mounts, path)
                .map(|_| stat_reply(rings, chan, 0, libr::STAT_DIR)),
        },
        // /dev/* senza prefix noto: solo sintesi (sotto).
        None => synth_children(mounts, path)
            .map(|_| stat_reply(rings, chan, 0, libr::STAT_DIR)),
    }
}

fn handle_mkdir(fs: &mut RamFs, mounts: &[FsMount], path: &str) -> Option<u64> {
    if path.is_empty() || path.len() > MAX_PATH {
        return None;
    }
    // mkdir solo su ramfs (i mount FAT/remoti non hanno mkdir).
    match resolve_local(mounts, path)? {
        FsKind::Ram => {
            fs.mkdir(path)?;
            Some(0)
        }
        _ => None,
    }
}

/// Cancella un file o una directory VUOTA (Fase 18.2, `R_DELETE`).
/// Solo ramfs: su FAT manca l'unlink (e' scrivibile dalla Fase 20, ma non
/// cancellabile) e i device remoti non sono file cancellabili
/// (e un mount point non si rimuove: si smonta). Ritorna Some(0) o None.
fn handle_delete(
    fs: &mut RamFs,
    mounts_fat: &[FsMount],
    mounts: &[Mount],
    path: &str,
) -> Option<u64> {
    if path.is_empty() || path.len() > MAX_PATH {
        return None;
    }
    // Mai dentro driver remoti…
    if resolve_mount(path, mounts).is_some() {
        return None;
    }
    // …e mai su mount FAT (unlink non implementato): solo ramfs.
    match resolve_local(mounts_fat, path)? {
        FsKind::Ram => {
            fs.remove(path)?;
            Some(0)
        }
        _ => None,
    }
}

/// Monta una sorgente sul target (Fase 16b, payload "source\0target\0").
/// Ritorna Some(0) se il mount e' ATTIVO, None altrimenti: a resolve fallito
/// (sorgente/target invalidi, nome ignoto, driver irraggiungibile) nessun
/// cambio di stato; a BPB illeggibile la spec resta registrata INATTIVA e
/// ritenta lazy (mai shadow ramfs).
fn handle_mount(mounts: &mut Vec<FsMount>, payload: &str, fgen: &mut u64) -> Option<u64> {
    let mut parts = payload.split('\0');
    let source = parts.next()?;
    let target = parts.next()?;
    if source.is_empty() || target.is_empty() {
        return None;
    }
    if apply_mount_spec(mounts, source, target, "") {
        // La tabella e' cambiata (spec nuova/sostituita): invalida le cache.
        *fgen = fgen.wrapping_add(1);
        Some(0)
    } else {
        None
    }
}

/// Smonta un target (Fase 16b). Rifiutato se ci sono fd aperti sotto il mount
/// (EBUSY); la radice ramfs non e' smontabile (non e' in tabella).
/// A rimozione riuscita bumpa `gen` (gli indici mount degli fd restanti non
/// cambiano per EBUSY, ma le istanze vanno comunque ricontrollate).
fn handle_umount(
    mounts: &mut Vec<FsMount>,
    ftable: &FileTable,
    target: &str,
    fgen: &mut u64,
) -> Option<u64> {
    let norm = normalize_target(target)?;
    let idx = mounts.iter().position(|m| m.target == norm)?;
    if ftable.has_mount_users(idx) {
        return None;
    }
    mounts.remove(idx);
    *fgen = fgen.wrapping_add(1);
    Some(0)
}

// ── Main ───────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    println!("[userfs] starting");

    // Registra il servizio Fs SUBITO (ADR-0008): il mount FAT32 e' lento, e i
    // client (devfs, testfs) risolvono Fs per nome appena partono. Registrarsi
    // prima del mount evita che chi spawa dopo aspetti inutilmente.
    // (L'ACK READY a init parte invece DOPO il populate, prima del loop:
    // READY significa "davvero pronto".)
    let reg_ok = libr::service_register(libr::Service::Fs).is_ok();
    if reg_ok {
        println!("[userfs] registered as service Fs");
    } else {
        println!("[userfs] FAILED to register service Fs");
    }

    // Mount FAT32 dalle spec statiche (Fase 16b: stesso codice dei mount
    // dinamici; IpcDisk riconnette da solo a ogni restart di userdisk, quindi
    // i mount sopravvivono alla morte del driver — t32). Spec inattive
    // (disco assente) restano in tabella e ritentano lazy al primo accesso.
    let mut fat_mounts: Vec<FsMount> = Vec::new();
    for (src, tgt) in STATIC_MOUNTS {
        if apply_mount_spec(&mut fat_mounts, src, tgt, "") {
            println!("[userfs] FAT32 montato a /{} (via userdisk)", tgt);
        } else {
            println!("[userfs] mount {} -> {} inattivo (disco assente?)", src, tgt);
        }
    }
    if fat_mounts.iter().all(|m| !m.is_active()) {
        println!("[userfs] nessun FAT attivo: ramfs only");
    }

    let mut fs = RamFs::new();
    let mut ftable = FileTable::new();
    let mut mounts: Vec<Mount> = Vec::new();
    // Generazione delle cache FileInfo per-fd (vedi `FileEntry`): bumpata a
    // ogni mutazione FAT (write/create/mount/umount/remount/drop d'epoca).
    // Parte da 1 (0 = mai usato, come le entry appena create per ramfs).
    let mut fat_gen: u64 = 1;

    // Client registrati: pid → (req_ring_phys, resp_ring_phys).
    let mut rings: BTreeMap<u64, (u64, u64)> = BTreeMap::new();

    // Diritti per-canale (Fase 17): entry assente = {ALL, root}.
    let mut rights: BTreeMap<u64, ChanRights> = BTreeMap::new();

    // Pre-populate: file di esempio
    if let Some(data) = fs.create_file("hello.txt") {
        data.extend_from_slice(b"Hello from rustOS ramfs!\n");
    }
    if let Some(data) = fs.create_file("test.txt") {
        data.extend_from_slice(b"Line 1\nLine 2\nLine 3\n");
    }
    println!("[userfs] ramfs popolata, entro in loop");

    // Notifica a init (canale di nascita) che il servizio Fs e' pronto: init
    // aspetta questo ACK prima di spawnare chi usa il filesystem (boot
    // deterministico, ADR-0008). DOPO il populate: READY significa "davvero
    // pronto" (vale anche per i restart: init procede a filesystem completo).
    // READY fire-and-forget (send_async, init-restart): init consuma senza
    // reply → una send sync resterebbe bloccata. Retry bounded, mai hang.
    for _ in 0..100 {
        if libr::send_async(libr::CHANNEL_PARENT, SVC_READY, reg_ok as u64, 0).is_ok() {
            break;
        }
        for _ in 0..10_000 {
            core::hint::spin_loop();
        }
    }

    // P1.2-diagnosi: contatore rimosso (era temporaneo); heap_stats resta in
    // libr per future diagnosi.
    loop {
        let msg = match libr::recv() {
            Ok(m) => m,
            Err(_) => continue,
        };
        // Scratch arena per-op (libr): TUTTI i borrow sotto muoiono entro
        // questa iterazione (handler sincroni, reply prima del prossimo
        // recv). Mai tenere `&` scratch oltre il fondo del loop.
        libr::scratch::reset();

        // Il client e' identificato dal canale da cui arriva la richiesta
        // (ADR-0008): ogni client ha il proprio canale verso Fs. La reply e'
        // implicita al messaggio corrente: se il client era bloccato in `send`
        // (sync) il kernel la consegna nel reply_slot; se era async (Fase 13)
        // il kernel accoda la risposta con req_id negativo. userfs non cambia.
        let chan = msg.channel;
        let tag = msg.tag;

        // Handshake ring buffer: il client registra i propri indirizzi fisici.
        if tag == FS_BUF_REG {
            rings.insert(chan, (msg.w0, msg.w1));
            println!("[userfs] client chan {} registered rings req={:#x} resp={:#x}", chan, msg.w0, msg.w1);
            let _ = libr::reply(0, 0, 0);
            continue;
        }

        // Registrazione driver: un processo notifica il proprio prefix di mount.
        // Il prefix viaggia nel request ring del chiamante.
        if tag == FS_REGISTER {
            let (req_phys, _resp_phys) = match rings.get(&chan) {
                Some(&r) => r,
                // Senza handshake (server riavviato): il driver rifa FS_BUF_REG
                // e ripete (libr, come sopra).
                None => { let _ = libr::reply(0, ERR_NOHANDSHAKE, 0); continue; }
            };
            // Mappa il request ring del driver per leggere il prefix
            if libr::map_physical(req_phys, REQ_RING_VA, 1).is_err() {
                let _ = libr::reply(0, ERR, 0);
                continue;
            }
            let (op_tag, _w0, _w1, payload_len) = match req_ring_read() {
                Some(f) => f,
                None => { let _ = libr::reply(0, ERR, 0); continue; }
            };
            if op_tag == R_REGISTER && payload_len > 0 && payload_len <= 514 {
                let mut prefix_buf = [0u8; 514];
                req_ring_read_payload(&mut prefix_buf, payload_len);
                // Payload = uno o piu' prefix NUL-separati (Fase 16d): devfs
                // registra "/dev/null\0/dev/zero" con UNA sola IPC, cosi' non
                // esiste una finestra in cui un mount e' forwardable mentre il
                // driver e' ancora bloccato in un secondo register sincrono.
                for raw in prefix_buf[..payload_len].split(|&b| b == 0) {
                    if raw.is_empty() {
                        continue;
                    }
                    let prefix = match core::str::from_utf8(raw) {
                        Ok(p) => p,
                        Err(_) => continue,
                    };
                    // Idempotente sul prefix (init-restart): se il prefix era
                    // gia' registrato (driver morto non ancora purgato o double
                    // register), sostituisci invece di duplicare — lo stale
                    // avvelenerebbe resolve_mount (first-match).
                    mounts.retain(|m| m.prefix.as_str() != prefix);
                    mounts.push(Mount {
                        prefix: String::from(prefix),
                        driver_chan: chan,
                    });
                    println!("[userfs] registered mount '{}' → driver_chan={}", prefix, chan);
                }
            } else {
                req_ring_consume(20 + payload_len);
            }
            // Mappa il response ring del client per scrivere la risposta
            map_client_resp_ring(&rings, chan);
            resp_ring_write(0, 0, &[]);
            let _ = libr::reply(0, 0, 0);
            continue;
        }

        // Morte di un peer (client o driver): purga tutto lo stato per-canale
        // (notifica unificata, Fase 14). Senza reply: il peer e' morto.
        if tag == libr::EXIT_NOTIFY {
            let mut remotes = Vec::new();
            ftable.purge(chan, &mut remotes);
            for (srv, rfd) in remotes {
                // Best-effort: il driver potrebbe essere morto a sua volta.
                let _ = libr::send(srv, DEV_CLOSE, rfd as u64, 0);
            }
            rings.remove(&chan);
            // Diritti effimeri (Fase 17): col peer muore anche la sua riga —
            // al re-handshake riparte da default {ALL, root} (limite dichiarato).
            rights.remove(&chan);
            // Se il morto era un driver, i suoi mount tornano registrabili:
            // lo stale, primo in lista, avvelenerebbe resolve_mount anche
            // dopo una re-registrazione dello stesso prefix.
            mounts.retain(|m| m.driver_chan != chan);
            // Se il morto era userdisk, invalida i client disco di tutti i
            // mount (Fase 16c: drop d'epoca — il prossimo accesso re-risolve
            // per nome e rimonta, t32). Veloce: solo compare dentro IpcDisk.
            // Le istanze cambiano: bumpa la generazione delle cache FileInfo.
            let mut epoch_dropped = false;
            for m in fat_mounts.iter_mut() {
                if m.note_peer_death(chan) {
                    epoch_dropped = true;
                }
            }
            if epoch_dropped {
                fat_gen = fat_gen.wrapping_add(1);
            }
            continue;
        }

        // Ogni altra operazione deve essere un FS_NOTIFY.
        if tag != FS_NOTIFY {
            // Tag ignoto: ERR come prima (nessun cambio semantico).
            let _ = libr::reply(0, ERR, 0);
            continue;
        }
        // Client senza handshake ring (es. server riavviato dopo la sua
        // registrazione, t28): segnale dedicato cosi' il client rifa
        // FS_BUF_REG e ripete l'op UNA volta (libr, Fase 14).
        if !rings.contains_key(&chan) {
            let _ = libr::reply(0, ERR_NOHANDSHAKE, 0);
            continue;
        }
        // Mappa i ring del client nello spazio di userfs.
        if !map_client_req_ring(&rings, chan) || !map_client_resp_ring(&rings, chan) {
            let _ = libr::reply(0, ERR, 0);
            continue;
        }

        // Leggi l'header del request frame (senza consumare: la lunghezza vera
        // e' dichiarata in w0/w1, vedi sotto).
        let (op_tag, w0, w1, avail) = match req_ring_read() {
            Some((t, a, b, avail)) => (t, a, b, avail),
            None => {
                // Ring vuoto a notifica arrivata: spuria/stale, niente da
                // consumare e niente da riallineare (tail==head gia').
                let _ = libr::reply(0, ERR, 0);
                continue;
            }
        };

        // Lunghezza payload dichiarata dal frame. Il formato frame non ha
        // lunghezza esplicita: consumare "tutto il disponibile" inghiotte gli
        // eventuali frame successivi gia' presenti (coalescenza), disallineando
        // il ring per sempre (osservato: tag=0x0 con pay enorme, RINGFULL lato
        // client e stallo senza recovery). Si consuma ESATTAMENTE il dichiarato;
        // il resto resta per la propria notifica.
        let expect: usize = match op_tag {
            R_OPEN | R_MKDIR | R_READDIR | R_REGISTER | R_MOUNT | R_UMOUNT | R_DELETE | R_STAT => {
                w0 as usize
            }
            R_WRITE | R_RIGHTS_DROP => w1 as usize,
            R_READ | R_CLOSE | R_RIGHTS_GET => 0,
            _ => {
                // Tag impossibile: scarta tutto e riallinea (vedi req_resync).
                req_resync();
                let _ = libr::reply(0, ERR, 0);
                continue;
            }
        };
        if expect > 4096 || avail < expect {
            // Frame impossibile o incompleto: riallinea e fallisci
            // visibilmente (mai wedge). Il payload perso appartiene a un'epoca
            // disallineata; il client ritenta (tty) o vede -1 (sync).
            req_resync();
            let _ = libr::reply(0, ERR, 0);
            continue;
        }

        // Diritti per-canale, check ops (Fase 17): CENTRALE, prima di
        // qualunque contatto handler/driver. A diniego il frame va comunque
        // consumato (20 + expect esatti) o il prossimo request del client
        // legge spazzatura — vale anche per il WRITE remoto negato (mai
        // map_in/send al driver in quel caso).
        if let Some(bit) = op_bit(op_tag) {
            if rights_ops(&rights, chan) & bit == 0 {
                req_ring_consume(20 + expect);
                resp_ring_write(ERR, 0, &[]);
                let _ = libr::reply(0, ERR, 0);
                continue;
            }
        }

        // R_WRITE verso un device remoto: NON consumare il request frame. Il
        // payload resta nel request ring del client e il driver (console/devfs),
        // che ha i ring del client iniettati via map_in, lo legge direttamente e
        // avanza la tail di (20 + w1) esatti. Qui scriviamo solo il result frame.
        if op_tag == R_WRITE && ftable.get_remote(chan, w0 as u32).is_some() {
            let result = handle_write_remote(&ftable, &rings, chan, w0 as u32, w1 as usize);
            resp_ring_write(to_reply(result), 0, &[]);
            let _ = libr::reply(0, to_reply(result), 0);
            continue;
        }

        // Percorsi locali (o remote non-WRITE): consuma ESATTAMENTE header +
        // payload dichiarato. req_ring_read_payload avanza la tail di
        // (20 + expect); eventuali byte successivi (coalescenza) restano per
        // la loro notifica invece di essere inghiottiti.
        // Il payload vive in scratch (mai heap: e' il temp per-op piu' grosso,
        // fino a 4096 B per chunk di write; consumato entro l'iterazione).
        // `expect` ≤ 4096 per il bound sopra: sta nel backing iniziale.
        let payload: &mut [u8] = match libr::scratch::alloc_bytes(expect) {
            Some(p) => p,
            None => {
                // OOM vera sullo scratch: come frame impossibile (mai wedge).
                req_resync();
                let _ = libr::reply(0, ERR, 0);
                continue;
            }
        };
        req_ring_read_payload(payload, expect);

        // Diritti per-canale, check subtree (Fase 17): solo le op con path.
        // Gli fd restano capability pure (read/write/close non ricontrollano
        // il path aperto). UTF-8 invalido o spec malformata: passa oltre, lo
        // rifiuta l'handler (i diritti non decidono la validita').
        let subtree_ok = match op_tag {
            R_OPEN | R_MKDIR | R_READDIR | R_DELETE | R_STAT => match core::str::from_utf8(payload) {
                Ok(p) => within_subtree(rights_subtree(&rights, chan), normalize_sub_view(p)),
                Err(_) => true,
            },
            R_MOUNT => match core::str::from_utf8(payload) {
                Ok(spec) => match spec.split_once('\0') {
                    Some((_, target)) => within_subtree(
                        rights_subtree(&rights, chan),
                        normalize_sub_view(target.trim_end_matches('\0')),
                    ),
                    None => true,
                },
                Err(_) => true,
            },
            R_UMOUNT => match core::str::from_utf8(payload) {
                Ok(t) => within_subtree(rights_subtree(&rights, chan), normalize_sub_view(t)),
                Err(_) => true,
            },
            _ => true,
        };
        if !subtree_ok {
            resp_ring_write(ERR, 0, &[]);
            let _ = libr::reply(0, ERR, 0);
            continue;
        }

        // Dispatch in base all'op_tag del ring. Ogni handler riceve gia' il
        // payload estratto: il frame e' stato interamente consumato sopra.
        let result = match op_tag {
            R_OPEN => {
                match core::str::from_utf8(&payload) {
                    // w1 del frame R_OPEN = flags (O_CREAT, w0 = len path):
                    // il server li ignorava (creava sempre) — ora POSIX.
                    Ok(path) => handle_open(&mut fs, &mut ftable, &mut fat_mounts, &mounts, chan, w1, path, &mut fat_gen),
                    Err(_) => None,
                }
            }

            R_READ => {
                handle_read(&fs, &mut ftable, &mut fat_mounts, &rings, chan, w0 as u32, w1 as usize, &mut fat_gen)
            }

            R_WRITE => {
                handle_write_local(&mut fs, &mut ftable, &mut fat_mounts, chan, w0 as u32, w1 as usize, &payload, &mut fat_gen)
            }

            R_CLOSE => {
                handle_close(&mut ftable, chan, w0 as u32)
            }

            R_READDIR => {
                match core::str::from_utf8(&payload) {
                    Ok("") | Ok("/") => handle_readdir(&fs, &mut fat_mounts, &mounts, &rings, chan, "/", &mut fat_gen),
                    Ok(path) => handle_readdir(&fs, &mut fat_mounts, &mounts, &rings, chan, path, &mut fat_gen),
                    Err(_) => None,
                }
            }

            R_MKDIR => {
                match core::str::from_utf8(&payload) {
                    Ok(path) => handle_mkdir(&mut fs, &fat_mounts, path),
                    Err(_) => None,
                }
            }

            R_MOUNT => {
                match core::str::from_utf8(&payload) {
                    Ok(spec) => handle_mount(&mut fat_mounts, spec, &mut fat_gen),
                    Err(_) => None,
                }
            }

            R_UMOUNT => {
                match core::str::from_utf8(&payload) {
                    Ok(target) => handle_umount(&mut fat_mounts, &ftable, target, &mut fat_gen),
                    Err(_) => None,
                }
            }

            R_DELETE => {
                match core::str::from_utf8(&payload) {
                    Ok(path) => handle_delete(&mut fs, &mut fat_mounts, &mounts, path),
                    Err(_) => None,
                }
            }

            R_STAT => {
                match core::str::from_utf8(&payload) {
                    Ok("") | Ok("/") => handle_stat(&fs, &mut fat_mounts, &mounts, &rings, chan, "/", &mut fat_gen),
                    Ok(path) => handle_stat(&fs, &mut fat_mounts, &mounts, &rings, chan, path, &mut fat_gen),
                    Err(_) => None,
                }
            }

            R_RIGHTS_DROP => handle_rights_drop(&mut rights, chan, w0 as u32, &payload),

            R_RIGHTS_GET => handle_rights_get(&rights, &rings, chan),

            _ => {
                // Tag sconosciuto: frame gia' consumato sopra, ritorna errore.
                None
            }
        };

        // Scrivi il response frame (se non e' gia' stato scritto dall'handler).
        // Gli handler locali (read, readdir) scrivono direttamente nella response
        // ring; qui scriviamo solo il result frame per conferma.
        // NOTA: handle_read, handle_readdir, handle_rights_get e handle_stat
        // scrivono payload+result, quindi qui NON dobbiamo scrivere di nuovo.
        // Per gli altri handler, scriviamo solo il result.
        match op_tag {
            R_READ | R_READDIR | R_RIGHTS_GET | R_STAT => {
                // Gli handler locali hanno gia' scritto nella response ring.
                // Per i remote, il driver ha gia' scritto nella response ring.
                // Non fare nulla — il result e' gia' nel frame.
            }
            _ => {
                resp_ring_write(to_reply(result), 0, &[]);
            }
        }

        let _ = libr::reply(0, to_reply(result), 0);
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("[userfs] panic: {}", info.message());
    libr::exit(1)
}
