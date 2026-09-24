use super::*;
use alloc::boxed::Box;
use crate::provider::{LocalFs, EntrySink, Meta};

// ── ramfs ──────────────────────────────────────────────────────────

/// Handle per RamFs: path limitato a 64 byte (Copy + PartialEq).
#[derive(Clone, Copy, PartialEq)]
pub struct RamHandle {
    path: [u8; 64],
    len: usize,
}

impl RamHandle {
    fn new(path: &str) -> Self {
        let bytes = path.as_bytes();
        let len = bytes.len().min(63);
        let mut p = [0u8; 64];
        p[..len].copy_from_slice(&bytes[..len]);
        Self { path: p, len }
    }

    fn as_str(&self) -> &str {
        // SAFETY: i bytes sono stati scritti da as_bytes() di un &str valido.
        unsafe { core::str::from_utf8_unchecked(&self.path[..self.len]) }
    }
}

#[derive(Clone)]
#[allow(dead_code)] // `mode`: placeholder Strato 0 (16b), enforcement futuro
pub enum FsNode {
    File { data: Vec<u8>, mode: u32 },
    Dir { entries: BTreeMap<String, FsNode>, mode: u32 },
}

/// Mode Unix di default (placeholder Strato 0, Fase 16b): conservati, MAI
/// enforcement (nessun uid nel sistema; i check R/W/X arrivano col login
/// boundary, futuro). FAT e' mappata fissa a mount (file 0o444, dir 0o555:
/// placeholder, l'enforcement non esiste; FAT e' scrivibile dalla Fase 20).
pub const MODE_FILE_DEF: u32 = 0o666;
pub const MODE_DIR_DEF: u32 = 0o777;
#[allow(dead_code)]
pub const MODE_FAT_FILE: u32 = 0o444;
#[allow(dead_code)]
pub const MODE_FAT_DIR: u32 = 0o555;

pub struct RamFs {
    root: BTreeMap<String, FsNode>,
}

impl RamFs {
    pub fn new() -> Self {
        Self { root: BTreeMap::new() }
    }

    /// Trova un nodo per path (es. "hello.txt" o "dir/file.txt").
    /// Ritorna il nodo finale (file o dir); i componenti intermedi devono
    /// essere directory (altrimenti None, come ENOTDIR).
    pub fn find(&self, path: &str) -> Option<&FsNode> {
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
    pub fn find_or_create(&mut self, path: &str) -> Option<&mut FsNode> {
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
    pub fn create_file(&mut self, path: &str) -> Option<&mut Vec<u8>> {
        let node = self.find_or_create(path)?;
        match node {
            FsNode::File { data, .. } => Some(data),
            FsNode::Dir { .. } => None,
        }
    }

    /// Lista le entry di una directory.
    pub fn readdir(&self, path: &str) -> Option<Vec<String>> {
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
    pub fn mkdir(&mut self, path: &str) -> Option<()> {
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
    pub fn remove(&mut self, path: &str) -> Option<()> {
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

// ── Implementazione LocalFs per RamFs (U0, provider trait) ─────────

impl LocalFs for RamFs {
    type Handle = RamHandle;

    fn open(&mut self, rel: &str, flags: u32) -> Result<Self::Handle, u64> {
        let path = rel.trim_start_matches('/');
        if path.is_empty() {
            return Err(crate::ERR_NOTFOUND);
        }
        let creat = flags & libr::O_CREAT != 0;
        let trunc = flags & libr::O_TRUNC != 0;

        // O_CREAT: crea il file se non esiste.
        if creat {
            if self.find(path).is_none() {
                // Crea il file (e le directory intermedie se necessario).
                let node = self.find_or_create(path);
                if node.is_none() {
                    return Err(crate::ERR_NOTFOUND);
                }
                match node.unwrap() {
                    FsNode::File { data, .. } => {
                        if trunc {
                            data.clear();
                        }
                    }
                    _ => return Err(crate::ERR_ISDIR),
                }
            } else if trunc {
                // Il file esiste gia': svuotalo.
                let node = self.find(path).unwrap();
                match node {
                    FsNode::File { data, .. } => {
                        // Devo usare find_or_create per avere &mut Vec<u8>.
                        let mut_node = self.find_or_create(path);
                        if let Some(FsNode::File { data, .. }) = mut_node {
                            data.clear();
                        }
                    }
                    _ => return Err(crate::ERR_ISDIR),
                }
            }
        } else if trunc {
            // O_TRUNC senza O_CREAT: il file deve esistere.
            let node = self.find(path).ok_or(crate::ERR_NOTFOUND)?;
            match node {
                FsNode::File { data, .. } => {
                    let mut_node = self.find_or_create(path);
                    if let Some(FsNode::File { data, .. }) = mut_node {
                        data.clear();
                    }
                }
                _ => return Err(crate::ERR_ISDIR),
            }
        }

        // Verifica che il nodo sia un file (non una directory).
        match self.find(path) {
            Some(FsNode::File { .. }) => Ok(RamHandle::new(path)),
            Some(FsNode::Dir { .. }) => Err(crate::ERR_ISDIR),
            None => Err(crate::ERR_NOTFOUND),
        }
    }

    fn read(&mut self, h: Self::Handle, off: usize, buf: &mut [u8]) -> Result<usize, u64> {
        let path = h.as_str();
        match self.find(path) {
            Some(FsNode::File { data, .. }) => {
                let len = data.len().saturating_sub(off);
                let n = len.min(buf.len());
                buf[..n].copy_from_slice(&data[off..off + n]);
                Ok(n)
            }
            Some(FsNode::Dir { .. }) => Err(crate::ERR_ISDIR),
            None => Err(crate::ERR_NOTFOUND),
        }
    }

    fn write(&mut self, h: Self::Handle, off: usize, buf: &[u8], append: bool) -> Result<usize, u64> {
        let path = h.as_str();
        // Clone il path prima di mutare self (borrow checker).
        let path_owned: alloc::string::String = path.into();
        match self.find_or_create(&path_owned) {
            Some(FsNode::File { data, .. }) => {
                if append {
                    let n = buf.len();
                    data.extend_from_slice(buf);
                    Ok(n)
                } else {
                    // Write con offset: estende il vettore se necessario.
                    let end = off + buf.len();
                    if end > data.len() {
                        data.resize(end, 0);
                    }
                    data[off..off + buf.len()].copy_from_slice(buf);
                    Ok(buf.len())
                }
            }
            Some(FsNode::Dir { .. }) => Err(crate::ERR_ISDIR),
            None => Err(crate::ERR_NOTFOUND),
        }
    }

    fn close(&mut self, _h: Self::Handle) {
        // RamFs non ha stato per-fd.
    }

    fn readdir(&mut self, rel: &str, out: &mut dyn EntrySink) -> Result<usize, u64> {
        // Chiama RamFs::readdir esplicitamente per evitare collisione con LocalFs::readdir.
        match RamFs::readdir(self, rel) {
            Some(entries) => {
                for name in &entries {
                    out.emit(name);
                }
                Ok(entries.len())
            }
            None => Err(crate::ERR_NOTFOUND),
        }
    }

    fn stat(&mut self, rel: &str) -> Result<Meta, u64> {
        match self.find(rel) {
            Some(FsNode::File { data, .. }) => Ok(Meta {
                size: data.len() as u64,
                kind: 0, // file
                readonly: false,
                mtime: 0,
            }),
            Some(FsNode::Dir { .. }) => Ok(Meta {
                size: 0,
                kind: 1, // dir
                readonly: false,
                mtime: 0,
            }),
            None => Err(crate::ERR_NOTFOUND),
        }
    }

    fn mkdir(&mut self, rel: &str) -> Result<(), u64> {
        let path = rel.trim_start_matches('/');
        if path.is_empty() {
            return Err(crate::ERR_INVALID);
        }
        // Crea la directory (e le intermedie).
        match self.find(path) {
            Some(FsNode::Dir { .. }) => Ok(()), // gia' esistente.
            Some(FsNode::File { .. }) => Err(crate::ERR_NOTDIR),
            None => {
                // Crea ricorsivamente.
                let parts: Vec<&str> = path.split('/').collect();
                if parts.is_empty() || parts[0].is_empty() {
                    return Err(crate::ERR_INVALID);
                }
                let mut current = &mut self.root;
                for (i, &part) in parts.iter().enumerate() {
                    if i == parts.len() - 1 {
                        // Ultima parte: crea la directory.
                        current.entry(String::from(part))
                            .or_insert_with(|| FsNode::Dir {
                                entries: BTreeMap::new(),
                                mode: MODE_DIR_DEF,
                            });
                        return Ok(());
                    }
                    let entry = current.entry(String::from(part))
                        .or_insert_with(|| FsNode::Dir {
                            entries: BTreeMap::new(),
                            mode: MODE_DIR_DEF,
                        });
                    match entry {
                        FsNode::Dir { entries: dir, .. } => current = dir,
                        _ => return Err(crate::ERR_NOTDIR),
                    }
                }
                Ok(())
            }
        }
    }

    fn remove(&mut self, rel: &str) -> Result<(), u64> {
        if self.remove(rel).is_some() {
            Ok(())
        } else {
            Err(crate::ERR_NOTFOUND)
        }
    }
}
