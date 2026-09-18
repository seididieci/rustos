use super::*;

// ── ramfs ──────────────────────────────────────────────────────────

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
