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
