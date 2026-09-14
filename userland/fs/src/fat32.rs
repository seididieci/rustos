//! Parser FAT32 read-only (Fase 9.2).
//!
//! Legge BPB, FAT, catene di cluster, directory e file 8.3 da una sorgente
//! settori `BlockSource` (Fase 16: disco ATA locale prima, client IPC poi).
//! Generalizzato a qualunque dimensione di cluster (BytesPerSec x SPC).
//! Limiti MVP: read-only, niente LFN (le entry 0x0F sono saltate), 8.3 names.

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// Sorgente di settori da 512 byte (LBA assoluti nel nodo montato).
/// Implementata dal driver ATA locale (Fase 9.2) o dal client IPC verso
/// userdisk (Fase 16, `ipc_disk.rs`): il parser non distingue.
pub trait BlockSource {
    fn read_sector(&self, lba: u64, buf: &mut [u8; 512]) -> bool;
}

const EOC: u32 = 0x0FFFFFF8;   // valori >= questo = fine catena
const BAD_CLUSTER: u32 = 0x0FFFFFF7;

pub struct DirEntry {
    /// Nome 8.3 senza padding ne' estensione separata ("HELLO.TXT").
    pub name: String,
    pub attr: u8,
    pub first_cluster: u32,
    pub size: u32,
}

pub struct FileInfo {
    pub first_cluster: u32,
    pub size: u32,
}

pub struct Fat32<B: BlockSource> {
    disk: B,
    bytes_per_sec: u16,
    spc: u8,
    fat_start: u32,   // LBA della prima FAT
    data_start: u32,  // LBA dei dati (cluster 2)
    root_cluster: u32,
}

const ATTR_DIR: u8 = 0x10;
const ATTR_VOLUME: u8 = 0x08;

impl<B: BlockSource> Fat32<B> {
    /// Monta il filesystem leggendo il BPB dal settore 0. Ritorna `None` se il
    /// disco non e' presente o i campi BPB non sono validi (fallback ramfs).
    pub fn mount(disk: B) -> Option<Fat32<B>> {        let mut boot = [0u8; 512];
        if !disk.read_sector(0, &mut boot) {
            return None;
        }
        if boot[510] != 0x55 || boot[511] != 0xAA {
            return None;
        }

        let bps = u16::from_le_bytes([boot[11], boot[12]]);
        let spc = boot[13];
        let rsvd = u16::from_le_bytes([boot[14], boot[15]]);
        let num_fats = boot[16];
        let fat_size = u32::from_le_bytes([boot[36], boot[37], boot[38], boot[39]]);
        let root = u32::from_le_bytes([boot[44], boot[45], boot[46], boot[47]]);

        // Validazioni minime del BPB.
        if bps != 512 || spc == 0 || (spc & (spc - 1)) != 0 {
            return None;
        }
        if num_fats == 0 || fat_size == 0 || root < 2 {
            return None;
        }

        let fat_start = rsvd as u32;
        let data_start = rsvd as u32 + num_fats as u32 * fat_size;

        Some(Fat32 {
            disk,
            bytes_per_sec: bps,
            spc,
            fat_start,
            data_start,
            root_cluster: root,
        })
    }

    fn cluster_bytes(&self) -> usize {
        self.bytes_per_sec as usize * self.spc as usize
    }

    /// Accesso alla sorgente settori (es. per invalidarla alla morte del
    /// server disco senza rimontare: la riconnessione e' lazy al prossimo read).
    pub fn disk(&self) -> &B {
        &self.disk
    }

    /// Legge `n` settori contigui a partire da `lba`.
    fn read_sectors(&self, lba: u32, n: usize, out: &mut [u8]) -> bool {
        for i in 0..n {
            let mut sector = [0u8; 512];
            if !self.disk.read_sector(lba as u64 + i as u64, &mut sector) {
                return false;
            }
            out[i * 512..i * 512 + 512].copy_from_slice(&sector);
        }
        true
    }

    /// Legge un intero cluster nel buffer (grandezza cluster).
    fn read_cluster(&self, cluster: u32, out: &mut [u8]) -> bool {
        let lba = self.data_start + (cluster - 2) * self.spc as u32;
        self.read_sectors(lba, self.spc as usize, out)
    }

    /// Valore della entry FAT per `cluster` (28 bit utili).
    fn fat_entry(&self, cluster: u32) -> u32 {
        let byte_off = cluster as usize * 4;
        let lba = self.fat_start + (byte_off / 512) as u32;
        let off = byte_off % 512;
        let mut sec = [0u8; 512];
        if !self.read_sectors(lba, 1, &mut sec) {
            return BAD_CLUSTER;
        }
        u32::from_le_bytes([sec[off], sec[off + 1], sec[off + 2], sec[off + 3]]) & 0x0FFFFFFF
    }

    /// Prossimo cluster della catena, `None` a fine catena / errore.
    fn next_cluster(&self, cluster: u32) -> Option<u32> {
        let v = self.fat_entry(cluster);
        if v >= EOC || v == BAD_CLUSTER {
            None
        } else {
            Some(v)
        }
    }

    /// Legge l'intera catena di cluster (es. un file o una directory).
    fn read_chain(&self, start: u32, out: &mut Vec<u8>) -> bool {
        let csize = self.cluster_bytes();
        let mut cluster = start;
        let mut chunk = vec![0u8; csize];
        loop {
            if !self.read_cluster(cluster, &mut chunk) {
                return false;
            }
            out.extend_from_slice(&chunk);
            match self.next_cluster(cluster) {
                Some(next) => cluster = next,
                None => break,
            }
        }
        true
    }

    /// Nome 8.3 dalla directory entry (spazi rimossi, estensione ricostruita).
    fn entry_name(raw: &[u8]) -> String {
        let name: Vec<u8> = raw[0..8].iter().copied().take_while(|&b| b != b' ').collect();
        let ext: Vec<u8> = raw[8..11].iter().copied().take_while(|&b| b != b' ').collect();
        let mut s = String::from_utf8_lossy(&name).into_owned();
        if !ext.is_empty() {
            s.push('.');
            s.push_str(&String::from_utf8_lossy(&ext));
        }
        s
    }

    /// Legge le entry di una directory (catena di cluster) saltando
    /// 0x00 (fine), 0xE5 (cancellata), 0x0F (LFN) e i volumi.
    fn read_dir(&self, cluster: u32) -> Vec<DirEntry> {
        let mut data = Vec::new();
        if !self.read_chain(cluster, &mut data) {
            return Vec::new();
        }

        let mut entries = Vec::new();
        let mut i = 0;
        while i + 32 <= data.len() {
            let e = &data[i..i + 32];
            let first = e[0];
            if first == 0x00 {
                break; // fine directory
            }
            if first == 0xE5 {
                i += 32; // cancellata
                continue;
            }
            let attr = e[11];
            if attr & 0x0F == 0x0F {
                i += 32; // entry LFN: la segue l'8.3
                continue;
            }
            if attr & ATTR_VOLUME != 0 {
                i += 32; // volume label
                continue;
            }

            let name = Self::entry_name(e);
            if name == "." || name == ".." {
                i += 32;
                continue;
            }

            let cl_lo = u16::from_le_bytes([e[26], e[27]]);
            let cl_hi = u16::from_le_bytes([e[20], e[21]]);
            let first_cluster = ((cl_hi as u32) << 16) | cl_lo as u32;
            let size = u32::from_le_bytes([e[28], e[29], e[30], e[31]]);

            entries.push(DirEntry {
                name,
                attr,
                first_cluster,
                size,
            });
            i += 32;
        }
        entries
    }

    /// Normalizza un componente di path allo stesso formato di `entry_name`
    /// (nome.est uppercase senza padding) per il match case-insensitive.
    fn normalize_component(comp: &str) -> String {
        let comp = comp.trim_matches('/').to_uppercase();
        match comp.split_once('.') {
            Some((n, e)) if !n.is_empty() => format!("{}.{}", n.trim(), e.trim()),
            _ => comp,
        }
    }

    /// Cerca un file/directory nel path (es. "SUB/NOTES.TXT"). L'ultimo
    /// componente puo' essere un file; i precedenti devono essere directory.
    pub fn find(&self, path: &str) -> Option<FileInfo> {
        let path = path.trim_matches('/');
        if path.is_empty() {
            return None;
        }
        let mut cluster = self.root_cluster;
        let parts: Vec<&str> = path.split('/').collect();

        for (idx, part) in parts.iter().enumerate() {
            if part.is_empty() {
                continue;
            }
            let target = Self::normalize_component(part);
            let entries = self.read_dir(cluster);
            let hit = entries.into_iter().find(|d| d.name.to_uppercase() == target)?;

            if idx == parts.len() - 1 {
                return Some(FileInfo {
                    first_cluster: hit.first_cluster,
                    size: hit.size,
                });
            }
            if hit.attr & ATTR_DIR == 0 {
                return None; // componente intermedio non e' una directory
            }
            cluster = hit.first_cluster;
        }
        None
    }

    /// Legge fino a `count` byte del file a partire da `offset`, copiandoli in
    /// `out`. Ritorna i byte letti (puo' essere < count a fine file).
    pub fn read_file(&self, info: &FileInfo, offset: usize, count: usize, out: &mut [u8]) -> usize {
        let csize = self.cluster_bytes();
        let size = info.size as usize;
        if offset >= size {
            return 0;
        }
        let to_read = count.min(size - offset);
        let mut written = 0usize;
        let mut cluster = info.first_cluster;
        let mut file_pos = 0usize;
        let mut chunk = vec![0u8; csize];

        while written < to_read {
            if !self.read_cluster(cluster, &mut chunk) {
                break;
            }
            let cl_start = file_pos;
            let cl_end = (file_pos + csize).min(size); // ultimo cluster troncato
            let ov_start = cl_start.max(offset);
            let ov_end = cl_end.min(offset + to_read);
            if ov_end > ov_start {
                let src = &chunk[ov_start - cl_start..ov_end - cl_start];
                out[written..written + src.len()].copy_from_slice(src);
                written += src.len();
            }
            file_pos = cl_end;
            if file_pos >= size {
                break;
            }
            match self.next_cluster(cluster) {
                Some(next) => cluster = next,
                None => break,
            }
        }
        written
    }

    /// Lista le entry di una directory (per path).
    pub fn list_dir(&self, path: &str) -> Vec<DirEntry> {
        if path.trim_matches('/').is_empty() {
            return self.read_dir(self.root_cluster);
        }
        match self.find(path) {
            Some(FileInfo { first_cluster, .. }) => self.read_dir(first_cluster),
            None => Vec::new(),
        }
    }
}
