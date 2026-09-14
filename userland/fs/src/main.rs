//! userfs — File system server (Fase 9.1 + 9.2 + 9.3 + 10.2 + 16.2).
//!
//! Riceve IPC dai processi client (open/read/write/close/readdir/mkdir) e
//! gestisce:
//!   - ramfs in memoria sul mount point `/` (scrivibile, Fase 9.1)
//!   - FAT32 read-only dal disco via `userdisk` sul mount point `/fat`
//!     (Fase 9.2 su ATA locale, Fase 16 via IPC `DISK_*`)
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
use alloc::vec;
use alloc::vec::Vec;
use libr;

mod fat32;
mod ipc_disk;

use fat32::Fat32;
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

const R_OPEN: u32 = 0x10;
const R_READ: u32 = 0x11;
const R_WRITE: u32 = 0x12;
const R_CLOSE: u32 = 0x13;
const R_READDIR: u32 = 0x14;
const R_MKDIR: u32 = 0x15;
/// Monta una sorgente su un target (Fase 16b): payload "source\0target\0".
const R_MOUNT: u32 = 0x16;
/// Smonta un target (Fase 16b): payload "target".
const R_UMOUNT: u32 = 0x17;
/// Un driver registra il proprio prefix di mount.
const R_REGISTER: u32 = 0x30;

/// IPC tag: il client ha scritto nel request ring e notifica il server.
const FS_NOTIFY: u64 = 0x32;
/// Handshake client: "i miei ring buffer hanno fisico req=w0, resp=w1".
const FS_BUF_REG: u64 = 0x31;

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

// ── IPC tag: registrazione driver ──────────────────────────────────

const FS_REGISTER: u64 = 0x30;

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
/// (disco<<16|sub, 0 = whole-disk). Usato per gli open raw `/dev/sdX`, dove il
/// prefix matchato e' il nodo stesso (rel vuota): userdisk valida davvero
/// (quante partizioni ha il disco) e rifiuta gli handle impossibili.
/// Ritorna None se non e' un nome disco.
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
    /// Source originale ("/dev/sda") per diagnostica e re-apply.
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
/// Dinamici (R_MOUNT, 16b.2) si aggiungono alla tabella ma si perdono al
/// restart (stato runtime, come fd e handshake: i client ristabiliscono).
const STATIC_MOUNTS: &[(&str, &str)] = &[("/dev/sda", "fat")];

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

/// Normalizza una source ("/dev/sda" → (source, handle)). Solo nodi disco.
fn normalize_source(source: &str) -> Option<(String, u32)> {
    let s = source.trim();
    let name = s.strip_prefix("/dev/")?;
    if name.is_empty() || name.contains('/') {
        return None;
    }
    let handle = disk_handle(name)?;
    Some((String::from(s), handle))
}

/// Applica una spec (statica o dinamica): valida e registra/aggiorna sempre la
/// spec (idempotente sul target), monta subito se il disco c'e'. Ritorna true
/// se il mount e' ATTIVO.
fn apply_mount_spec(
    mounts: &mut Vec<FsMount>,
    source: &str,
    target: &str,
    opts: &str,
) -> bool {
    let (norm_source, handle) = match normalize_source(source) {
        Some(x) => x,
        None => return false,
    };
    let norm_target = match normalize_target(target) {
        Some(x) => x,
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
    fn note_peer_death(&self, dead_chan: u64) {
        if let MountedFs::Fat(Some(f)) = &self.fs {
            f.disk().note_peer_death(dead_chan);
        }
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
/// inattivo (ritenta il mount ora; solo variante Fat: le future varianti
/// aggiungono il loro ramo qui). Ritorna (indice mount, rel).
fn resolve_fsmount<'a>(mounts: &mut Vec<FsMount>, path: &'a str) -> Option<(usize, &'a str)> {
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
    let handle = mounts[i].handle;
    let active = match &mut mounts[i].fs {
        MountedFs::Fat(opt) => {
            if opt.is_none() {
                *opt = Fat32::mount(IpcDisk::new(handle));
            }
            opt.is_some()
        }
    };
    if !active {
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
/// tanto e' read-only).
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
        let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
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
        let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
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
}

// ── Open file table ────────────────────────────────────────────────

enum FileEntry {
    /// File locale: `path` e' relativo al suo filesystem (ramfs: path assoluto
    /// senza slash iniziale; FAT: relativo al mount). `mnt` = indice in
    /// `mounts_fat` per i file FAT, None per ramfs (radice sempre locale).
    Local { path: String, kind: FsKind, offset: usize, mnt: Option<usize> },
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
            FileEntry::Local { path, kind, offset, mnt } => {
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
    path: &str,
) -> Option<u64> {
    if path.is_empty() || path.len() > MAX_PATH {
        return None;
    }

    // Cerca nei mount point registrati (devfs, console, userdisk, futuri driver).
    if let Some((driver_chan, rel)) = resolve_mount(path, mounts) {
        // Nodo disco raw (Fase 16): open("/dev/sda") matcha il prefix del nodo
        // stesso (rel vuota) — l'handle si ricava dal nome Linux, prima di
        // dev_type (che su "" fallirebbe comunque). Handle impossibili o
        // userdisk irraggiungibile → None (client -1, mai wedge).
        if rel.is_empty() {
            let prefix = path.trim_start_matches('/');
            if let Some(name) = prefix.strip_prefix("dev/") {
                if let Some(handle) = disk_handle(name) {
                    let reply = libr::send(driver_chan, DEV_OPEN, handle as u64, 0).ok()?;
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
    if let Some((mi, rel)) = resolve_fsmount(mounts_fat, path) {
        let fat = mounts_fat[mi].fat()?;
        fat.find(rel)?;
        return Some(ftable.open(chan, rel, FsKind::Fat, Some(mi)));
    }
    match resolve_local(mounts_fat, path)? {
        FsKind::Fat => None, // mount inattivo: errore, mai shadow ramfs
        FsKind::Ram => {
            fs.create_file(path);
            Some(ftable.open(chan, path, FsKind::Ram, None))
        }
    }
}

fn handle_read(
    fs: &RamFs,
    ftable: &mut FileTable,
    mounts_fat: &Vec<FsMount>,
    rings: &BTreeMap<u64, (u64, u64)>,
    chan: u64,
    fd: u32,
    count: usize,
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

    let data: Vec<u8> = match kind {
        FsKind::Ram => {
            let d = match fs.find(path)? {
                FsNode::File { data: d, .. } => d,
                _ => return None,
            };
            if offset >= d.len() {
                Vec::new()
            } else {
                let end = (offset + count).min(d.len());
                d[offset..end].to_vec()
            }
        }
        FsKind::Fat => {
            let mi = mnt?;
            let fat = mounts_fat.get(mi)?.fat()?;
            let info = fat.find(path)?;
            let mut buf = vec![0u8; count];
            let n = fat.read_file(&info, offset, count, &mut buf);
            buf.truncate(n);
            buf
        }
    };

    let bytes_read = data.len();
    // Scrivi i dati nella response ring del client.
    if bytes_read > 0 {
        if let Some(&(_, _)) = rings.get(&chan) {
            map_client_resp_ring(rings, chan);
            resp_ring_write(bytes_read as u64, 0, &data);
        }
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
/// `payload`. FAT32 e' read-only.
fn handle_write_local(
    fs: &mut RamFs,
    ftable: &mut FileTable,
    chan: u64,
    fd: u32,
    count: usize,
    payload: &[u8],
) -> Option<u64> {
    let (path, kind, offset, _mnt) = ftable.get(chan, fd)?;
    if kind == FsKind::Fat {
        return None; // FAT32 read-only
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

    if let Some((mi, rel)) = resolve_fsmount(mounts_fat, path) {
        let fat = mounts_fat[mi].fat()?;
        let entries: Vec<String> =
            fat.list_dir(rel).into_iter().map(|d| d.name).collect();
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

    let entries: Vec<String> = match resolve_local(mounts_fat, path)? {
        FsKind::Fat => return None, // mount noto ma inattivo: errore, mai shadow
        FsKind::Ram => fs.readdir(path)?,
    };

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

fn handle_mkdir(fs: &mut RamFs, mounts: &[FsMount], path: &str) -> Option<u64> {
    if path.is_empty() || path.len() > MAX_PATH {
        return None;
    }
    // mkdir solo su ramfs (i mount sono read-only o remoti).
    match resolve_local(mounts, path)? {
        FsKind::Ram => {
            fs.mkdir(path)?;
            Some(0)
        }
        _ => None,
    }
}

/// Monta una sorgente sul target (Fase 16b, payload "source\0target\0").
/// Ritorna Some(0) se il mount e' ATTIVO, None altrimenti (sorgente/target
/// invalidi o disco assente: la spec resta comunque registrata e ritenta
/// lazy, ma al client risponde errore subito).
fn handle_mount(mounts: &mut Vec<FsMount>, payload: &str) -> Option<u64> {
    let mut parts = payload.split('\0');
    let source = parts.next()?;
    let target = parts.next()?;
    if source.is_empty() || target.is_empty() {
        return None;
    }
    if apply_mount_spec(mounts, source, target, "") {
        Some(0)
    } else {
        None
    }
}

/// Smonta un target (Fase 16b). Rifiutato se ci sono fd aperti sotto il mount
/// (EBUSY); la radice ramfs non e' smontabile (non e' in tabella).
fn handle_umount(
    mounts: &mut Vec<FsMount>,
    ftable: &FileTable,
    target: &str,
) -> Option<u64> {
    let norm = normalize_target(target)?;
    let idx = mounts.iter().position(|m| m.target == norm)?;
    if ftable.has_mount_users(idx) {
        return None;
    }
    mounts.remove(idx);
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

    // Client registrati: pid → (req_ring_phys, resp_ring_phys).
    let mut rings: BTreeMap<u64, (u64, u64)> = BTreeMap::new();

    // Pre-populate: file di esempio
    if let Some(data) = fs.create_file("hello.txt") {
        data.extend_from_slice(b"Hello from Velordor ramfs!\n");
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
        if libr::send_async(libr::CHANNEL_PARENT, 0x7D, reg_ok as u64, 0).is_ok() {
            break;
        }
        for _ in 0..10_000 {
            core::hint::spin_loop();
        }
    }

    loop {
        let msg = match libr::recv() {
            Ok(m) => m,
            Err(_) => continue,
        };

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
            if op_tag == R_REGISTER && payload_len > 0 && payload_len <= MAX_PATH {
                let mut prefix_buf = [0u8; 256];
                req_ring_read_payload(&mut prefix_buf, payload_len);
                if let Ok(prefix) = core::str::from_utf8(&prefix_buf[..payload_len]) {
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
            // Se il morto era un driver, i suoi mount tornano registrabili:
            // lo stale, primo in lista, avvelenerebbe resolve_mount anche
            // dopo una re-registrazione dello stesso prefix.
            mounts.retain(|m| m.driver_chan != chan);
            // Se il morto era userdisk, invalida i client disco di tutti i
            // mount (il prossimo read riconnette da solo: lookup + HELLO +
            // remap, t32). Veloce: solo compare dentro IpcDisk.
            for m in fat_mounts.iter() {
                m.note_peer_death(chan);
            }
            continue;
        }

        // Ogni altra operazione deve essere un FS_NOTIFY.
        if tag != FS_NOTIFY {
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
            R_OPEN | R_MKDIR | R_READDIR | R_REGISTER | R_MOUNT | R_UMOUNT => w0 as usize,
            R_WRITE => w1 as usize,
            R_READ | R_CLOSE => 0,
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
        let mut payload = vec![0u8; expect];
        req_ring_read_payload(&mut payload, expect);

        // Dispatch in base all'op_tag del ring. Ogni handler riceve gia' il
        // payload estratto: il frame e' stato interamente consumato sopra.
        let result = match op_tag {
            R_OPEN => {
                match core::str::from_utf8(&payload) {
                    Ok(path) => handle_open(&mut fs, &mut ftable, &mut fat_mounts, &mounts, chan, path),
                    Err(_) => None,
                }
            }

            R_READ => {
                handle_read(&fs, &mut ftable, &fat_mounts, &rings, chan, w0 as u32, w1 as usize)
            }

            R_WRITE => {
                handle_write_local(&mut fs, &mut ftable, chan, w0 as u32, w1 as usize, &payload)
            }

            R_CLOSE => {
                handle_close(&mut ftable, chan, w0 as u32)
            }

            R_READDIR => {
                match core::str::from_utf8(&payload) {
                    Ok("") | Ok("/") => handle_readdir(&fs, &mut fat_mounts, &mounts, &rings, chan, "/"),
                    Ok(path) => handle_readdir(&fs, &mut fat_mounts, &mounts, &rings, chan, path),
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
                    Ok(spec) => handle_mount(&mut fat_mounts, spec),
                    Err(_) => None,
                }
            }

            R_UMOUNT => {
                match core::str::from_utf8(&payload) {
                    Ok(target) => handle_umount(&mut fat_mounts, &ftable, target),
                    Err(_) => None,
                }
            }

            _ => {
                // Tag sconosciuto: frame gia' consumato sopra, ritorna errore.
                None
            }
        };

        // Scrivi il response frame (se non e' gia' stato scritto dall'handler).
        // Gli handler locali (read, readdir) scrivono direttamente nella response
        // ring; qui scriviamo solo il result frame per conferma.
        // NOTA: handle_read e handle_readdir locali scrivono payload+result,
        // quindi qui NON dobbiamo scrivere di nuovo. Per gli altri handler,
        // scriviamo solo il result.
        match op_tag {
            R_READ | R_READDIR => {
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
