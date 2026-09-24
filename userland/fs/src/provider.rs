use super::*;
use alloc::boxed::Box;

/// Sink per l'output di readdir: ogni entry scrive il nome nel buffer del client.
pub trait EntrySink {
    fn emit(&mut self, name: &str);
}

/// Metadati di una directory entry (presentazione POSIX).
#[derive(Clone, Copy)]
pub struct Meta {
    pub size: u64,
    pub kind: u8, // 0 = file, 1 = dir, 2 = device
    pub readonly: bool,
    pub mtime: u64,
}

/// Provider di filesystem locale (ramfs, FAT, ArcaFS, ...).
/// La trait e' la presentazione POSIX: il core del FS resta nativo.
pub trait LocalFs {
    type Handle: Copy + PartialEq;

    fn open(&mut self, rel: &str, flags: u32) -> Result<Self::Handle, u64>;
    fn read(&mut self, h: Self::Handle, off: usize, buf: &mut [u8]) -> Result<usize, u64>;
    fn write(&mut self, h: Self::Handle, off: usize, buf: &[u8], append: bool) -> Result<usize, u64>;
    fn close(&mut self, h: Self::Handle);
    fn readdir(&mut self, rel: &str, out: &mut dyn EntrySink) -> Result<usize, u64>;
    fn stat(&mut self, rel: &str) -> Result<Meta, u64>;
    fn mkdir(&mut self, rel: &str) -> Result<(), u64>;
    fn remove(&mut self, rel: &str) -> Result<(), u64>;
}

/// Filesystem montato su un target. Le varianti tengono l'istanza viva;
/// `None` = spec registrata ma inattiva (sorgente assente all'ultimo tentativo).
pub enum MountedFs {
    Fat(Option<Fat32<IpcDisk>>),
    Local(Box<dyn LocalFsDyn>),
}

/// Versione object-safe di LocalFs: il Handle e' erased a `*const ()` per il
/// dynamic dispatch. Il puntatore punta a un handle allocato in Box dentro
/// il chiamante (o al bordo del canale).
pub trait LocalFsDyn {
    fn open_dyn(&mut self, rel: &str, flags: u32) -> Result<*const (), u64>;
    fn read_dyn(&mut self, h: *const (), off: usize, buf: &mut [u8]) -> Result<usize, u64>;
    fn write_dyn(&mut self, h: *const (), off: usize, buf: &[u8], append: bool) -> Result<usize, u64>;
    fn close_dyn(&mut self, h: *const ());
    fn readdir_dyn(&mut self, rel: &str, out: &mut dyn EntrySink) -> Result<usize, u64>;
    fn stat_dyn(&mut self, rel: &str) -> Result<Meta, u64>;
    fn mkdir_dyn(&mut self, rel: &str) -> Result<(), u64>;
    fn remove_dyn(&mut self, rel: &str) -> Result<(), u64>;
}

/// Wrapper che adatta T: LocalFs a LocalFsDyn usando boxed handles.
struct DynHandle<T: LocalFs> {
    fs: T,
}

impl<T: LocalFs> LocalFsDyn for DynHandle<T> {
    fn open_dyn(&mut self, rel: &str, flags: u32) -> Result<*const (), u64> {
        let h = <T as LocalFs>::open(&mut self.fs, rel, flags)?;
        // Boxa l'handle e restituisce il raw pointer.
        Ok(Box::into_raw(Box::new(h)) as *const ())
    }

    fn read_dyn(&mut self, h: *const (), off: usize, buf: &mut [u8]) -> Result<usize, u64> {
        let h = unsafe { &*(h as *const <T as LocalFs>::Handle) };
        <T as LocalFs>::read(&mut self.fs, *h, off, buf)
    }

    fn write_dyn(&mut self, h: *const (), off: usize, buf: &[u8], append: bool) -> Result<usize, u64> {
        let h = unsafe { &*(h as *const <T as LocalFs>::Handle) };
        <T as LocalFs>::write(&mut self.fs, *h, off, buf, append)
    }

    fn close_dyn(&mut self, h: *const ()) {
        // Libera il box e distrugge l'handle.
        unsafe { drop(Box::from_raw(h as *mut <T as LocalFs>::Handle)) };
    }

    fn readdir_dyn(&mut self, rel: &str, out: &mut dyn EntrySink) -> Result<usize, u64> {
        <T as LocalFs>::readdir(&mut self.fs, rel, out)
    }

    fn stat_dyn(&mut self, rel: &str) -> Result<Meta, u64> {
        <T as LocalFs>::stat(&mut self.fs, rel)
    }

    fn mkdir_dyn(&mut self, rel: &str) -> Result<(), u64> {
        <T as LocalFs>::mkdir(&mut self.fs, rel)
    }

    fn remove_dyn(&mut self, rel: &str) -> Result<(), u64> {
        <T as LocalFs>::remove(&mut self.fs, rel)
    }
}
