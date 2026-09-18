use super::*;
use crate::*;
use super::types::{ATTR_DIR, ATTR_VOLUME};

impl<B: BlockSource> Fat32<B> {
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
    /// P1.2 — parse incrementale settore per settore, SENZA accumulare lo
    /// stream in un Vec (ogni Vec temporaneo per-op frammentava la free-list
    /// dell'heap di userfs: +1 blocco non coalescibile per op → scansioni
    /// O(n)/O(n²) su tutte le op successive). Gli `entry_off` sono identici
    /// a prima (offset aritmetici nello stesso stream).
    fn read_dir(&self, cluster: u32) -> Vec<DirEntry> {
        let mut entries = Vec::new();
        let spc = self.spc as usize;
        let mut c = cluster;
        let mut base = 0usize; // offset stream all'inizio del cluster corrente
        let mut sec = [0u8; 512];
        'walk: loop {
            for si in 0..spc {
                let lba =
                    self.data_start as u64 + (c - 2) as u64 * spc as u64 + si as u64;
                if !self.disk.read_sector(lba, &mut sec) {
                    return Vec::new();
                }
                let mut k = 0usize;
                while k + 32 <= 512 {
                    let e = &sec[k..k + 32];
                    let first = e[0];
                    if first == 0x00 {
                        break 'walk; // fine directory
                    }
                    if first == 0xE5 {
                        k += 32; // cancellata
                        continue;
                    }
                    let attr = e[11];
                    if attr & 0x0F == 0x0F {
                        k += 32; // entry LFN: la segue l'8.3
                        continue;
                    }
                    if attr & ATTR_VOLUME != 0 {
                        k += 32; // volume label
                        continue;
                    }

                    let name = Self::entry_name(e);
                    if name == "." || name == ".." {
                        k += 32;
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
                        entry_off: base + si * 512 + k,
                    });
                    k += 32;
                }
            }
            base += spc * 512;
            match self.next_cluster(c) {
                Some(next) => c = next,
                None => break,
            }
        }
        entries
    }

    /// Normalizza un componente di path allo stesso formato di `entry_name`
    /// (nome.est uppercase senza padding) per il match case-insensitive.
    pub(crate) fn normalize_component(comp: &str) -> String {
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
                    is_dir: hit.attr & ATTR_DIR != 0,
                    dir_cluster: cluster,
                    entry_off: hit.entry_off,
                });
            }
            if hit.attr & ATTR_DIR == 0 {
                return None; // componente intermedio non e' una directory
            }
            cluster = hit.first_cluster;
        }
        None
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
