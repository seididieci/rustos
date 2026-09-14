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

/// Risolve un path in FsKind per i filesystem locali (ram, fat).
/// Per i path remoti, ritorna None (usa resolve_mount).
fn resolve_local(path: &str) -> Option<FsKind> {
    let t = path.trim_start_matches('/');
    if t == "fat" || t.starts_with("fat/") {
        Some(FsKind::Fat)
    } else if t.starts_with("dev/") || t == "dev" {
        None // gestito da resolve_mount
    } else {
        Some(FsKind::Ram)
    }
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

/// Path relativo al mount FAT (`/fat/HELLO.TXT` -> `HELLO.TXT`, `/fat` -> ``).
fn fat_rel_path(path: &str) -> &str {
    path.trim_start_matches('/')
        .strip_prefix("fat")
        .map(|s| s.trim_start_matches('/'))
        .unwrap_or("")
}

// ── Helper conversione ─────────────────────────────────────────────

/// Converte `Option<u64>` in valore IPC: `Some(v)` → `v`, `None` → `ERR`.
#[inline]
fn to_reply(val: Option<u64>) -> u64 {
    val.unwrap_or(ERR)
}

// ── ramfs ──────────────────────────────────────────────────────────

#[derive(Clone)]
enum FsNode {
    File(Vec<u8>),
    Dir(BTreeMap<String, FsNode>),
}

struct RamFs {
    root: BTreeMap<String, FsNode>,
}

impl RamFs {
    fn new() -> Self {
        Self { root: BTreeMap::new() }
    }

    /// Trova un nodo per path (es. "hello.txt" o "dir/file.txt").
    fn find(&self, path: &str) -> Option<&FsNode> {
        if path.is_empty() || path == "/" {
            return None;
        }
        let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        let mut current_dir = &self.root;
        let mut final_node = None;
        for &part in &parts {
            match current_dir.get(part) {
                Some(FsNode::Dir(d)) => {
                    current_dir = d;
                }
                Some(node) => {
                    final_node = Some(node);
                    break;
                }
                None => return None,
            }
        }
        final_node
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
                    .or_insert_with(|| FsNode::File(Vec::new()));
                return current.get_mut(part);
            }
            let entry = current.entry(String::from(part))
                .or_insert_with(|| FsNode::Dir(BTreeMap::new()));
            match entry {
                FsNode::Dir(dir) => current = dir,
                _ => return None,
            }
        }
        None
    }

    /// Crea un file vuoto se non esiste, ritorna il nodo.
    fn create_file(&mut self, path: &str) -> Option<&mut Vec<u8>> {
        let node = self.find_or_create(path)?;
        match node {
            FsNode::File(data) => Some(data),
            FsNode::Dir(_) => None,
        }
    }

    /// Lista le entry di una directory.
    fn readdir(&self, path: &str) -> Option<Vec<String>> {
        if path.is_empty() || path == "/" {
            return Some(self.root.keys().cloned().collect());
        }
        let node = self.find(path)?;
        match node {
            FsNode::Dir(entries) => Some(entries.keys().cloned().collect()),
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
                    .or_insert_with(|| FsNode::Dir(BTreeMap::new()));
                return Some(());
            }
            let entry = current.entry(String::from(part))
                .or_insert_with(|| FsNode::Dir(BTreeMap::new()));
            match entry {
                FsNode::Dir(dir) => current = dir,
                _ => return None,
            }
        }
        None
    }
}

// ── Open file table ────────────────────────────────────────────────

enum FileEntry {
    Local { path: String, kind: FsKind, offset: usize },
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

    fn open(&mut self, chan: u64, path: &str, kind: FsKind) -> u64 {
        let fd = self.alloc_fd(chan);
        self.files.insert((chan, fd), FileEntry::Local {
            path: String::from(path),
            kind,
            offset: 0,
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

    fn get(&self, chan: u64, fd: u32) -> Option<(&str, FsKind, usize)> {
        match self.files.get(&(chan, fd))? {
            FileEntry::Local { path, kind, offset } => Some((path.as_str(), *kind, *offset)),
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
    fat: Option<&Fat32<IpcDisk>>,
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

    // Filesystem locali (ram, fat).
    match resolve_local(path)? {
        FsKind::Fat => {
            let fat = fat?;
            fat.find(fat_rel_path(path))?;
            Some(ftable.open(chan, fat_rel_path(path), FsKind::Fat))
        }
        FsKind::Ram => {
            fs.create_file(path);
            Some(ftable.open(chan, path, FsKind::Ram))
        }
    }
}

fn handle_read(
    fs: &RamFs,
    ftable: &mut FileTable,
    fat: Option<&Fat32<IpcDisk>>,
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

    let (path, kind, offset) = ftable.get(chan, fd)?;

    let data: Vec<u8> = match kind {
        FsKind::Ram => {
            let d = match fs.find(path)? {
                FsNode::File(d) => d,
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
            let fat = fat?;
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
    let (path, kind, offset) = ftable.get(chan, fd)?;
    if kind == FsKind::Fat {
        return None; // FAT32 read-only
    }

    match fs.find_or_create(path)? {
        FsNode::File(file_data) => {
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
    fat: Option<&Fat32<IpcDisk>>,
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

    let entries: Vec<String> = match resolve_local(path)? {
        FsKind::Fat => {
            let fat = fat?;
            let rel = fat_rel_path(path);
            fat.list_dir(rel).into_iter().map(|d| d.name).collect()
        }
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

fn handle_mkdir(fs: &mut RamFs, path: &str) -> Option<u64> {
    if path.is_empty() || path.len() > MAX_PATH {
        return None;
    }
    // mkdir solo su ramfs (FAT32 e' read-only).
    match resolve_local(path)? {
        FsKind::Ram => {
            fs.mkdir(path)?;
            Some(0)
        }
        _ => None, // FAT32 e' read-only, mount remoti non supportano mkdir
    }
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

    // Monta il FAT32 via userdisk (Fase 16, nodo /dev/sda = handle 0):
    // IpcDisk riconnette da solo a ogni restart di userdisk (lazy), quindi il
    // mount sopravvive alla morte del driver (t32). Se userdisk/disco assenti
    // a boot, ramfs-only (come prima quando mancava il disco).
    let fat = Fat32::mount(IpcDisk::new(0));
    match &fat {
        Some(_) => println!("[userfs] FAT32 montato a /fat (via userdisk)"),
        None => println!("[userfs] FAT32 assente: ramfs only"),
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
            // Se il morto era userdisk, invalida il client IPC (il prossimo
            // read FAT riconnette da solo: lookup + HELLO + remap, t32).
            // Veloce: solo un compare dentro IpcDisk.
            if let Some(f) = fat.as_ref() {
                f.disk().note_peer_death(chan);
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
            R_OPEN | R_MKDIR | R_READDIR | R_REGISTER => w0 as usize,
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
                    Ok(path) => handle_open(&mut fs, &mut ftable, fat.as_ref(), &mounts, chan, path),
                    Err(_) => None,
                }
            }

            R_READ => {
                handle_read(&fs, &mut ftable, fat.as_ref(), &rings, chan, w0 as u32, w1 as usize)
            }

            R_WRITE => {
                handle_write_local(&mut fs, &mut ftable, chan, w0 as u32, w1 as usize, &payload)
            }

            R_CLOSE => {
                handle_close(&mut ftable, chan, w0 as u32)
            }

            R_READDIR => {
                match core::str::from_utf8(&payload) {
                    Ok("") | Ok("/") => handle_readdir(&fs, fat.as_ref(), &mounts, &rings, chan, "/"),
                    Ok(path) => handle_readdir(&fs, fat.as_ref(), &mounts, &rings, chan, path),
                    Err(_) => None,
                }
            }

            R_MKDIR => {
                match core::str::from_utf8(&payload) {
                    Ok(path) => handle_mkdir(&mut fs, path),
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
