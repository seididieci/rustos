//! Parser FAT32 (Fase 9.2, scrivibile da Fase 20).
//!
//! Legge BPB, FAT, catene di cluster, directory e file 8.3 da una sorgente
//! settori `BlockSource` (Fase 16: client IPC verso userdisk).
//! Generalizzato a qualunque dimensione di cluster (BytesPerSec x SPC).
//! Limiti: niente LFN (le entry 0x0F sono saltate), 8.3 names, niente
//! mkdir/rm su FAT (solo overwrite/create/grow, Fase 20).

extern crate alloc;

use alloc::format;

mod types;
mod mount;
mod fat;
mod dir;
mod file;

pub use types::*;

// ── Implementazione LocalFs per Fat32 (U0, provider trait) ─────────

impl<B: BlockSource> crate::provider::LocalFs for Fat32<B> {
    type Handle = FileInfo;

    fn open(&mut self, rel: &str, flags: u32) -> Result<Self::Handle, u64> {
        let path = rel.trim_matches('/');
        if path.is_empty() {
            return Err(crate::ERR_NOTFOUND);
        }
        // O_CREAT: crea il file se non esiste (single source `libr::O_*`,
        // Fase 49: mai costanti magiche nel provider).
        if flags & libr::O_CREAT != 0 && self.find(path).is_none() && !self.create_file(path) {
            return Err(crate::ERR_NOTFOUND);
        }
        let info = self.find(path).ok_or(crate::ERR_NOTFOUND)?;
        if info.is_dir {
            return Err(crate::ERR_ISDIR);
        }
        // O_TRUNC assorbito qui (Fase 49, come `RamFs::open`): niente piu'
        // find+truncate fuori trait nell'handler. Fallimento = rifiuto, mai
        // truncate parziale dichiarato riuscito (contratto `truncate`).
        if flags & libr::O_TRUNC != 0 {
            if !self.truncate(&info) {
                return Err(crate::ERR);
            }
            // La truncate cambia size/first_cluster: rileggi fresco.
            return self.find(path).ok_or(crate::ERR_NOTFOUND);
        }
        Ok(info)
    }

    fn read(&mut self, h: Self::Handle, off: usize, buf: &mut [u8]) -> Result<usize, u64> {
        if h.is_dir {
            return Err(crate::ERR_ISDIR);
        }
        let n = self.read_file(&h, off, buf.len(), buf);
        Ok(n)
    }

    fn write(&mut self, h: Self::Handle, off: usize, buf: &[u8], append: bool) -> Result<usize, u64> {
        if h.is_dir {
            return Err(crate::ERR_ISDIR);
        }
        // Semantica come ramfs: O_APPEND ignora `off` e accoda a fine file;
        // altrimenti si scrive a `off`. `write_grow` e' un superset di
        // `write_file` (overwrite entro la size, crescita — e allocazione del
        // primo cluster per i file appena creati — oltre) come il vecchio
        // handler FAT faceva sempre.
        let off = if append { h.size as usize } else { off };
        Ok(self.write_grow(&h, off, buf))
    }

    fn close(&mut self, _h: Self::Handle) {
        // FAT non ha stato per-fd.
    }

    fn readdir(&mut self, rel: &str, out: &mut dyn crate::provider::EntrySink) -> Result<usize, u64> {
        let entries = self.list_dir(rel);
        for e in &entries {
            out.emit(&e.name);
        }
        Ok(entries.len())
    }

    fn stat(&mut self, rel: &str) -> Result<crate::provider::Meta, u64> {
        match self.find(rel) {
            Some(info) => Ok(crate::provider::Meta {
                size: info.size as u64,
                kind: if info.is_dir { 1 } else { 0 },
                readonly: false, // FAT scrivibile dalla Fase 20 (write/grow).
                mtime: 0,
            }),
            None => Err(crate::ERR_NOTFOUND),
        }
    }

    fn mkdir(&mut self, _rel: &str) -> Result<(), u64> {
        // mkdir su FAT e' fuori scope (niente unlink/mkdir).
        Err(crate::ERR_READONLY)
    }

    fn remove(&mut self, _rel: &str) -> Result<(), u64> {
        // remove su FAT e' fuori scope.
        Err(crate::ERR_READONLY)
    }
}

// ── Implementazione LocalFsDyn per Fat32 (Fase 48 wiring, Fase 49 handle
// unico): dispatch su `AnyHandle` discriminato — il ramo sbagliato e'
// errore, mai reinterpretazione (niente piu' `*const ()` + Box).
impl<B: BlockSource> crate::provider::LocalFsDyn for Fat32<B> {
    fn open_dyn(&mut self, rel: &str, flags: u32) -> Result<crate::provider::AnyHandle, u64> {
        <Self as crate::provider::LocalFs>::open(self, rel, flags).map(crate::provider::AnyHandle::Fat)
    }

    fn read_dyn(&mut self, h: crate::provider::AnyHandle, off: usize, buf: &mut [u8]) -> Result<usize, u64> {
        match h {
            crate::provider::AnyHandle::Fat(info) => {
                <Self as crate::provider::LocalFs>::read(self, info, off, buf)
            }
            _ => Err(crate::ERR_INVALID),
        }
    }

    fn write_dyn(
        &mut self,
        h: crate::provider::AnyHandle,
        off: usize,
        buf: &[u8],
        append: bool,
    ) -> Result<usize, u64> {
        match h {
            crate::provider::AnyHandle::Fat(info) => {
                <Self as crate::provider::LocalFs>::write(self, info, off, buf, append)
            }
            _ => Err(crate::ERR_INVALID),
        }
    }

    fn readdir_dyn(&mut self, rel: &str, out: &mut dyn crate::provider::EntrySink) -> Result<usize, u64> {
        <Self as crate::provider::LocalFs>::readdir(self, rel, out)
    }

    fn stat_dyn(&mut self, rel: &str) -> Result<crate::provider::Meta, u64> {
        <Self as crate::provider::LocalFs>::stat(self, rel)
    }

    fn mkdir_dyn(&mut self, rel: &str) -> Result<(), u64> {
        <Self as crate::provider::LocalFs>::mkdir(self, rel)
    }

    fn remove_dyn(&mut self, rel: &str) -> Result<(), u64> {
        <Self as crate::provider::LocalFs>::remove(self, rel)
    }
}
