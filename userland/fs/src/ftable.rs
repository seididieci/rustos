use super::*;

// ── Open file table ────────────────────────────────────────────────

#[derive(Clone)]
pub enum FileEntry {
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
    /// `append` (Fase 40, O_APPEND): le write ignorano `offset` e accodano a
    /// fine file; le read usano `offset` normalmente.
    Local { path: String, kind: mount_legacy::FsKind, offset: usize, mnt: Option<usize>, fat_info: Option<FileInfo>, fat_gen: u64, append: bool },
    Remote { server_chan: u64, remote_fd: u32 },
}

pub struct FileTable {
    pub files: BTreeMap<(u64, u32), FileEntry>,
    next_fd: BTreeMap<u64, u32>,
}

impl FileTable {
    pub fn new() -> Self {
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

    pub fn open(&mut self, chan: u64, path: &str, kind: mount_legacy::FsKind, mnt: Option<usize>, append: bool) -> u64 {
        let fd = self.alloc_fd(chan);
        self.files.insert((chan, fd), FileEntry::Local {
            path: String::from(path),
            kind,
            offset: 0,
            mnt,
            fat_info: None,
            fat_gen: 0,
            append,
        });
        fd as u64
    }

    /// Come `open` ma con FileInfo FAT gia' risolto (evita un find al primo
    /// uso): `gen` e' la generazione corrente (la cache nasce valida).
    pub fn open_fat(&mut self, chan: u64, path: &str, mnt: usize, info: FileInfo, fgen: u64, append: bool) -> u64 {
        let fd = self.alloc_fd(chan);
        self.files.insert((chan, fd), FileEntry::Local {
            path: String::from(path),
            kind: mount_legacy::FsKind::Fat,
            offset: 0,
            mnt: Some(mnt),
            fat_info: Some(info),
            fat_gen: fgen,
            append,
        });
        fd as u64
    }

    /// Inserisce una entry LOCALE clonata da uno snapshot (Fase 40, claim di
    /// un grant): fd fresco sul canale del claimant, entry indipendente. La
    /// cache FAT NON si eredita (fat_info = None: il primo uso rifa `find`
    /// — grant e claim sono vicini ma mai assumere freschezza oltre l'op).
    /// Ritorna None se lo snapshot non e' Local (mai, per costruzione).
    pub fn open_cloned(&mut self, chan: u64, snap: &FileEntry) -> Option<u64> {
        match snap {
            FileEntry::Local { path, kind, offset, mnt, append, .. } => {
                let fd = self.alloc_fd(chan);
                self.files.insert((chan, fd), FileEntry::Local {
                    path: path.clone(),
                    kind: *kind,
                    offset: *offset,
                    mnt: *mnt,
                    fat_info: None,
                    fat_gen: 0,
                    append: *append,
                });
                Some(fd as u64)
            }
            FileEntry::Remote { .. } => None,
        }
    }

    pub fn open_remote(&mut self, chan: u64, server_chan: u64, remote_fd: u32) -> u64 {
        let fd = self.alloc_fd(chan);
        self.files.insert((chan, fd), FileEntry::Remote { server_chan, remote_fd });
        fd as u64
    }

    pub fn close(&mut self, chan: u64, fd: u32) -> bool {
        self.files.remove(&(chan, fd)).is_some()
    }

    /// Purga TUTTO lo stato del canale `chan` (morte del peer, notifica
    /// unificata Fase 14): fd locali e remoti + contatore next_fd. Raccoglie
    /// in `remotes` le coppie `(server_chan, remote_fd)` da chiudere presso
    /// i driver con DEV_CLOSE (il chiamante lo fa best-effort).
    pub fn purge(&mut self, chan: u64, remotes: &mut Vec<(u64, u32)>) {
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
    pub fn has_mount_users(&self, mi: usize) -> bool {
        self.files.values().any(|e| match e {
            FileEntry::Local { mnt: Some(m), .. } => *m == mi,
            _ => false,
        })
    }

    pub fn get(&self, chan: u64, fd: u32) -> Option<(&str, mount_legacy::FsKind, usize, Option<usize>)> {
        match self.files.get(&(chan, fd))? {
            FileEntry::Local { path, kind, offset, mnt, .. } => {
                Some((path.as_str(), *kind, *offset, *mnt))
            }
            FileEntry::Remote { .. } => None,
        }
    }

    pub fn get_remote(&self, chan: u64, fd: u32) -> Option<(u64, u32)> {
        match self.files.get(&(chan, fd))? {
            FileEntry::Remote { server_chan, remote_fd } => Some((*server_chan, *remote_fd)),
            _ => None,
        }
    }

    pub fn set_offset(&mut self, chan: u64, fd: u32, offset: usize) {
        if let Some(FileEntry::Local { offset: o, .. }) = self.files.get_mut(&(chan, fd)) {
            *o = offset;
        }
    }

    /// True se il fd e' aperto in O_APPEND (Fase 40): le write accodano.
    pub fn is_append(&self, chan: u64, fd: u32) -> bool {
        matches!(
            self.files.get(&(chan, fd)),
            Some(FileEntry::Local { append: true, .. })
        )
    }

    /// Aggiorna la cache FileInfo del fd (dopo una scrittura che puo' aver
    /// cambiato size/first_cluster): `None` se l'entry non e' un file FAT.
    pub fn refresh_fat_info(&mut self, chan: u64, fd: u32, info: Option<FileInfo>, fgen: u64) {
        if let Some(FileEntry::Local { kind: mount_legacy::FsKind::Fat, fat_info, fat_gen, .. }) =
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
pub fn fd_fat_info(
    ftable: &mut FileTable,
    fat: &Fat32<IpcDisk>,
    chan: u64,
    fd: u32,
    fgen: u64,
) -> Option<FileInfo> {
    let rel_owned = {
        let e = ftable.files.get(&(chan, fd))?;
        match e {
            FileEntry::Local { fat_info: Some(info), fat_gen: g, kind: mount_legacy::FsKind::Fat, .. }
                if *g == fgen =>
            {
                return Some(*info)
            }
            FileEntry::Local { path, kind: mount_legacy::FsKind::Fat, .. } => path.clone(),
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
