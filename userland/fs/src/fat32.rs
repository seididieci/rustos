//! Parser FAT32 (Fase 9.2, scrivibile da Fase 20).
//!
//! Legge BPB, FAT, catene di cluster, directory e file 8.3 da una sorgente
//! settori `BlockSource` (Fase 16: client IPC verso userdisk).
//! Generalizzato a qualunque dimensione di cluster (BytesPerSec x SPC).
//! Limiti: niente LFN (le entry 0x0F sono saltate), 8.3 names, niente
//! mkdir/rm su FAT (solo overwrite/create/grow, Fase 20).

extern crate alloc;
use alloc::boxed::Box;

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
        // O_CREAT (0x200): crea file vuoto se non esiste.
        if flags & 0x200 != 0 {
            if self.find(path).is_some() || self.create_file(path) {
                return Ok(self.find(path).unwrap_or(FileInfo {
                    first_cluster: 0,
                    size: 0,
                    is_dir: false,
                    dir_cluster: self.root_cluster,
                    entry_off: 0,
                }));
            }
            return Err(crate::ERR_NOTFOUND);
        }
        // Altrimenti: il file deve esistere.
        match self.find(path) {
            Some(info) => Ok(info),
            None => Err(crate::ERR_NOTFOUND),
        }
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

// ── Implementazione LocalFsDyn per Fat32 (Fase 48: wiring handler) ───
// Fat32 implementa gia' LocalFs; LocalFsDyn e' il wrapper object-safe con
// handles erasure a *const (). Qui si boxa/unbox come DynHandle<T> ma per
// Fat32<B> direttamente (nessun wrapper intermedio).

impl<B: BlockSource> crate::provider::LocalFsDyn for Fat32<B> {
    fn open_dyn(&mut self, rel: &str, flags: u32) -> Result<*const (), u64> {
        let h = <Self as crate::provider::LocalFs>::open(self, rel, flags)?;
        Ok(Box::into_raw(Box::new(h)) as *const ())
    }

    fn read_dyn(&mut self, h: *const (), off: usize, buf: &mut [u8]) -> Result<usize, u64> {
        let h = unsafe { &*(h as *const <Self as crate::provider::LocalFs>::Handle) };
        <Self as crate::provider::LocalFs>::read(self, *h, off, buf)
    }

    fn write_dyn(&mut self, h: *const (), off: usize, buf: &[u8], append: bool) -> Result<usize, u64> {
        let h = unsafe { &*(h as *const <Self as crate::provider::LocalFs>::Handle) };
        <Self as crate::provider::LocalFs>::write(self, *h, off, buf, append)
    }

    fn close_dyn(&mut self, h: *const ()) {
        unsafe { drop(Box::from_raw(h as *mut <Self as crate::provider::LocalFs>::Handle)) };
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
