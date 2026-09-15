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
    /// Scrive un settore (Fase 20, FAT scrivibile): write-through, nessun
    /// caching — ogni write torna solo a settore stabile su disco.
    fn write_sector(&self, lba: u64, data: &[u8; 512]) -> bool;
}

const EOC: u32 = 0x0FFFFFF8;   // valori >= questo = fine catena
const BAD_CLUSTER: u32 = 0x0FFFFFF7;

pub struct DirEntry {
    /// Nome 8.3 senza padding ne' estensione separata ("HELLO.TXT").
    pub name: String,
    pub attr: u8,
    pub first_cluster: u32,
    pub size: u32,
    /// Offset in byte dell'entry da 32 B nel data stream della directory
    /// (serve a 20.3 per aggiornare size/first_cluster sul posto).
    pub entry_off: usize,
}

pub struct FileInfo {
    pub first_cluster: u32,
    pub size: u32,
    /// true se directory (da attr, serve a R_STAT; Fase 19.2).
    pub is_dir: bool,
    /// Cluster iniziale della directory CONTENITRICE + offset in byte
    /// dell'entry (Fase 20.3: update size/first_cluster sul posto).
    pub dir_cluster: u32,
    pub entry_off: usize,
}

pub struct Fat32<B: BlockSource> {
    disk: B,
    bytes_per_sec: u16,
    spc: u8,
    fat_start: u32,   // LBA della prima FAT
    /// Numero di copie FAT (Fase 20.3: le scritture aggiornano TUTTE).
    num_fats: u8,
    /// Settori per FAT (capacita' ~ fat_size*128 cluster, bound di scan).
    fat_size: u32,
    data_start: u32,  // LBA dei dati (cluster 2)
    root_cluster: u32,
    /// Seriale volume FAT32 (`vol_id`, BPB+67 LE32 nel layout standard firma
    /// `0x29`@66; `fat_bpb_identity` accetta anche il legacy firma@67).
    /// `None` = assente: niente identità stabile da questo nodo (Fase 16d).
    vol_serial: Option<u32>,
    /// Label volume BPB+71 (11 byte raw, padding spazi): identità `LABEL=`.
    vol_label: [u8; 11],
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

        // Identità stabile del volume (Fase 16d, helper condiviso in libr:
        // stessi check di mount, usati anche dallo sniff per-nodo di userdisk).
        let (vol_serial, vol_label) = match libr::fat_bpb_identity(&boot) {
            Some((s, l)) => (s, l),
            None => (None, [b' '; 11]),
        };

        Some(Fat32 {
            disk,
            bytes_per_sec: bps,
            spc,
            fat_start,
            num_fats,
            fat_size,
            data_start,
            root_cluster: root,
            vol_serial,
            vol_label,
        })
    }

    /// Seriale volume (`UUID=`, maiuscolo hex 8 char) o `None` se assente.
    pub fn vol_serial(&self) -> Option<u32> {
        self.vol_serial
    }

    /// Label volume normalizzata (trim spazi) per match `LABEL=`.
    pub fn vol_label_trimmed(&self) -> &[u8] {
        let mut n = self.vol_label.len();
        while n > 0 && self.vol_label[n - 1] == b' ' {
            n -= 1;
        }
        &self.vol_label[..n]
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
                entry_off: i,
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

    /// Scrive fino a `data.len()` byte del file a partire da `offset`
    /// (Fase 20.2: SOLO overwrite entro `size`, mai crescita — oltre EOF si
    /// ferma e ritorna i byte scritti). Read-modify-write a settori: ogni
    /// settore toccato viene letto, rattoppato e riscritto (write-through,
    /// niente cache). Ritorna i byte scritti (0 = niente da fare/errore).
    pub fn write_file(&self, info: &FileInfo, offset: usize, data: &[u8]) -> usize {
        let csize = self.cluster_bytes();
        let size = info.size as usize;
        if offset >= size || data.is_empty() || info.first_cluster < 2 {
            return 0;
        }
        let to_write = data.len().min(size - offset);
        // Walk fino al cluster che contiene `offset`.
        let mut cluster = info.first_cluster;
        let mut file_pos = 0usize;
        while file_pos + csize <= offset {
            match self.next_cluster(cluster) {
                Some(next) => cluster = next,
                None => return 0, // catena piu' corta di size: corrotta
            }
            file_pos += csize;
        }
        let mut done = 0usize;
        let mut sec = [0u8; 512];
        while done < to_write {
            let rel = offset + done - file_pos; // byte nel cluster
            let sec_idx = (rel / 512) as u32;
            let sec_off = rel % 512;
            let lba = self.data_start as u64
                + ((cluster - 2) as u64) * (self.spc as u64)
                + sec_idx as u64;
            let chunk = (512 - sec_off).min(to_write - done);
            if !self.disk.read_sector(lba, &mut sec) {
                return done;
            }
            sec[sec_off..sec_off + chunk].copy_from_slice(&data[done..done + chunk]);
            if !self.disk.write_sector(lba, &sec) {
                return done;
            }
            done += chunk;
            // Sforato nel cluster successivo: avanza la catena.
            if offset + done - file_pos >= csize {
                file_pos += csize;
                if offset + done < size {
                    match self.next_cluster(cluster) {
                        Some(next) => cluster = next,
                        None => return done, // catena corta: corrotta, stop
                    }
                }
            }
        }
        done
    }

    // ── Scrittura (Fase 20, FAT scrivibile, write-through) ──────────
    // Ordine crash-safe: prima si linkano i cluster (catena sempre valida),
    // poi si azzera, poi i dati, la size nella dir-entry PER ULTIMA. Un crash
    // in mezzo lascia cluster allocati oltre-size (leak, fsck-fixabile) ma mai
    // spazzatura leggibile.

    /// Valore EOC da scrivere nelle entry FAT allocate.
    const EOC_VAL: u32 = 0x0FFFFFFF;

    /// Scrive il valore (28 bit, nibble alto preservato) nella entry FAT di
    /// `cluster`, in TUTTE le copie. Ritorna false su errore IO.
    fn set_fat_entry(&self, cluster: u32, val: u32) -> bool {
        let byte_off = cluster as usize * 4;
        let sec_off = byte_off % 512;
        let sec_idx = (byte_off / 512) as u64;
        let mut sec = [0u8; 512];
        for f in 0..self.num_fats as u64 {
            let lba = self.fat_start as u64 + f * self.fat_size as u64 + sec_idx;
            if !self.disk.read_sector(lba, &mut sec) {
                return false;
            }
            let cur = u32::from_le_bytes([sec[sec_off], sec[sec_off + 1], sec[sec_off + 2], sec[sec_off + 3]]);
            let patched = (cur & 0xF0000000) | (val & 0x0FFFFFFF);
            sec[sec_off..sec_off + 4].copy_from_slice(&patched.to_le_bytes());
            if !self.disk.write_sector(lba, &sec) {
                return false;
            }
        }
        true
    }

    /// Alloca un cluster libero (scan da 2, bound = capacita' FAT) e lo marca
    /// EOC in tutte le copie. NON lo linka: lo fa il chiamante.
    fn alloc_one(&self) -> Option<u32> {
        let max = self.fat_size as usize * 128;
        let mut c = 2u32;
        while (c as usize) < max {
            if self.fat_entry(c) == 0 {
                if self.set_fat_entry(c, Self::EOC_VAL) {
                    return Some(c);
                }
                return None;
            }
            c += 1;
        }
        None // disco pieno (o FAT illeggibile: fat_entry=BAD≠0, skip)
    }

    /// Lunghezza catena (0 se `start` < 2) e ultimo cluster. None se la catena
    /// e' corrotta (loop o BAD oltre il primo).
    fn chain_tail(&self, start: u32) -> Option<(usize, u32)> {
        if start < 2 {
            return Some((0, 0));
        }
        let mut len = 1usize;
        let mut c = start;
        loop {
            match self.next_cluster(c) {
                Some(next) => {
                    c = next;
                    len += 1;
                    if len > self.fat_size as usize * 128 {
                        return None; // loop
                    }
                }
                None => return Some((len, c)),
            }
        }
    }

    /// Azzera un intero cluster (sicurezza: niente stale leggibile dopo grow).
    fn zero_cluster(&self, c: u32) -> bool {
        let zero = [0u8; 512];
        for i in 0..self.spc as u64 {
            let lba = self.data_start as u64 + ((c - 2) as u64) * (self.spc as u64) + i;
            if !self.disk.write_sector(lba, &zero) {
                return false;
            }
        }
        true
    }

    /// Azzera il range [a, b) del file (catena da `first`, size logica `end`):
    /// read-modify-write a settori. Usato per la coda [old_size, new_end).
    fn zero_range(&self, first: u32, a: usize, b: usize) -> bool {
        if b <= a || first < 2 {
            return true;
        }
        let csize = self.cluster_bytes();
        let mut cluster = first;
        let mut file_pos = 0usize;
        while file_pos + csize <= a {
            match self.next_cluster(cluster) {
                Some(next) => cluster = next,
                None => return false,
            }
            file_pos += csize;
        }
        let mut sec = [0u8; 512];
        let mut pos = a;
        while pos < b {
            let rel = pos - file_pos;
            let sec_idx = (rel / 512) as u64;
            let sec_off = rel % 512;
            let lba = self.data_start as u64
                + ((cluster - 2) as u64) * (self.spc as u64)
                + sec_idx;
            let chunk = (512 - sec_off).min(b - pos);
            if !self.disk.read_sector(lba, &mut sec) {
                return false;
            }
            sec[sec_off..sec_off + chunk].fill(0);
            if !self.disk.write_sector(lba, &sec) {
                return false;
            }
            pos += chunk;
            if pos - file_pos >= csize && pos < b {
                file_pos += csize;
                match self.next_cluster(cluster) {
                    Some(next) => cluster = next,
                    None => return false,
                }
            }
        }
        true
    }

    /// (LBA assoluto, offset) dell'entry da 32 B in directory: walk della
    /// catena dir per `entry_off`. L'entry puo' cavalcare due settori (off >
    /// 480): il chiamante gestisce entrambi.
    fn dir_entry_pos(&self, dir_cluster: u32, entry_off: usize) -> Option<(u64, usize)> {
        if dir_cluster < 2 {
            return None;
        }
        let csize = self.cluster_bytes();
        let mut cluster = dir_cluster;
        let mut skip = entry_off / csize;
        while skip > 0 {
            match self.next_cluster(cluster) {
                Some(next) => cluster = next,
                None => return None,
            }
            skip -= 1;
        }
        let in_cl = entry_off % csize;
        let lba = self.data_start as u64
            + ((cluster - 2) as u64) * (self.spc as u64)
            + (in_cl / 512) as u64;
        Some((lba, in_cl % 512))
    }

    /// Aggiorna size (+ first_cluster se cambiato) nella dir-entry. Gestisce
    /// lo straddle su due settori. Size scritta PER ULTIMA (crash-safe).
    fn patch_entry(&self, dir_cluster: u32, entry_off: usize, first: u32, size: u32) -> bool {
        let (lba, off) = match self.dir_entry_pos(dir_cluster, entry_off) {
            Some(p) => p,
            None => return false,
        };
        let mut sec = [0u8; 512];
        if !self.disk.read_sector(lba, &mut sec) {
            return false;
        }
        // first_cluster: byte 26-27 (lo) + 20-21 (hi); size: byte 28-31.
        let patches: [(usize, u8); 8] = [
            (26, (first & 0xFF) as u8),
            (27, ((first >> 8) & 0xFF) as u8),
            (20, ((first >> 16) & 0xFF) as u8),
            (21, ((first >> 24) & 0xFF) as u8),
            (28, (size & 0xFF) as u8),
            (29, ((size >> 8) & 0xFF) as u8),
            (30, ((size >> 16) & 0xFF) as u8),
            (31, ((size >> 24) & 0xFF) as u8),
        ];
        for (i, v) in patches {
            let pos = off + i;
            if pos < 512 {
                sec[pos] = v;
            } else {
                // Straddle: secondo settore (pos - 512).
                let mut sec2 = [0u8; 512];
                if !self.disk.read_sector(lba + 1, &mut sec2) {
                    return false;
                }
                sec2[pos - 512] = v;
                if !self.disk.write_sector(lba + 1, &sec2) {
                    return false;
                }
            }
        }
        self.disk.write_sector(lba, &sec)
    }

    /// Aggiorna FSInfo (settore 1): free count di `delta` (negativo in
    /// allocazione) + hint next-free. Silente se il settore non ha le firme
    /// (volume senza FSInfo: niente da aggiornare).
    fn fsinfo_bump(&self, delta: i64, next_free: u32) -> bool {
        let mut sec = [0u8; 512];
        if !self.disk.read_sector(1, &mut sec) {
            return true; // illeggibile: meglio niente che danni
        }
        if u32::from_le_bytes([sec[0], sec[1], sec[2], sec[3]]) != 0x41615252
            || u32::from_le_bytes([sec[484], sec[485], sec[486], sec[487]]) != 0x61417272
            || sec[510] != 0x55
            || sec[511] != 0xAA
        {
            return true; // niente FSInfo: skip
        }
        let free = u32::from_le_bytes([sec[488], sec[489], sec[490], sec[491]]) as i64 + delta;
        let free = free.max(0) as u32;
        sec[488..492].copy_from_slice(&free.to_le_bytes());
        sec[492..496].copy_from_slice(&next_free.to_le_bytes());
        self.disk.write_sector(1, &sec)
    }

    /// Scrive con crescita (Fase 20.3): se `offset+len` supera `size`, alloca
    /// i cluster mancanti (linkati subito), azzera la coda [size, new_end) e
    /// aggiorna la dir-entry. Ritorna i byte scritti; la size cresce solo di
    /// quanto e' atterrato davvero (mai oltre). Fallimento allocazione →
    /// degrado a overwrite entro size (come 20.2).
    pub fn write_grow(&self, info: &FileInfo, offset: usize, data: &[u8]) -> usize {
        let size = info.size as usize;
        if data.is_empty() || info.is_dir {
            return 0;
        }
        if offset + data.len() <= size {
            return self.write_file(info, offset, data);
        }
        let csize = self.cluster_bytes();
        let (mut have, mut last) = match self.chain_tail(info.first_cluster) {
            Some(t) => t,
            None => return self.write_file(info, offset, data), // corrotta: degrado
        };
        let mut first = info.first_cluster;
        let mut new_end = offset + data.len();
        let need = new_end.div_ceil(csize);
        let mut allocated = 0u32;
        while have < need {
            match self.alloc_one() {
                Some(c) => {
                    if have == 0 {
                        first = c;
                    } else if !self.set_fat_entry(last, c) {
                        // Link fallito: cluster orfano (fsck-fixabile), degrado.
                        break;
                    }
                    last = c;
                    have += 1;
                    allocated += 1;
                }
                None => break, // disco pieno: degrado a quanto c'e'
            }
        }
        // Azzera la coda [size, new_end) sulla catena estesa (sicurezza).
        let cap_end = have * csize;
        if cap_end < new_end {
            // Allocazione corta: si scrive solo fin dove c'e' spazio.
            new_end = cap_end;
        }
        if new_end <= offset {
            return 0;
        }
        if !self.zero_range(first, size.min(new_end), new_end) {
            // Zero fallito: degrado a overwrite entro la vecchia size.
            let grown = self.write_file(info, offset, data);
            let _ = self.fsinfo_bump(-(allocated as i64), last.saturating_add(1));
            return grown;
        }
        // Scrivi i dati (bound = new_end via info ombra).
        let shadow = FileInfo {
            first_cluster: first,
            size: new_end as u32,
            is_dir: false,
            dir_cluster: info.dir_cluster,
            entry_off: info.entry_off,
        };
        let done = self.write_file(&shadow, offset, &data[..(new_end - offset).min(data.len())]);
        let final_size = (offset + done).max(size.min(new_end));
        // Dir-entry PER ULTIMA + FSInfo (best-effort: i dati sono gia' a posto).
        let _ = self.patch_entry(info.dir_cluster, info.entry_off, first, final_size as u32);
        let _ = self.fsinfo_bump(-(allocated as i64), last.saturating_add(1));
        done
    }

    /// Crea un file vuoto (Fase 20.4): entry 8.3 maiuscola (no LFN) nella
    /// directory padre, primo cluster 0 + size 0. Ritorna false se il nome
    /// non e' 8.3 valido, il padre manca/non e' dir, esiste gia', o la
    /// directory e' piena e non si allarga (errore IO). `mkdir` su FAT resta
    /// fuori scope (attr sempre 0x20 = file).
    pub fn create_file(&self, path: &str) -> bool {
        let path = path.trim_matches('/');
        if path.is_empty() {
            return false;
        }
        let (parent, leaf) = match path.rsplit_once('/') {
            Some((p, l)) => (p, l),
            None => ("", path),
        };
        // Nome 8.3 maiuscolo (stessa normalizzazione del match di find).
        let norm = Self::normalize_component(leaf);
        let (name, ext) = match norm.split_once('.') {
            Some((n, e)) => (n, e),
            None => (norm.as_str(), ""),
        };
        if name.is_empty() || name.len() > 8 || ext.len() > 3 {
            return false;
        }
        // Caratteri FAT consentiti (maiusc + cifre + simboli std, niente LFN).
        for &b in name.as_bytes().iter().chain(ext.as_bytes().iter()) {
            let ok = b.is_ascii_uppercase()
                || b.is_ascii_digit()
                || b"$%'-_@~`!(){}^#&".contains(&b);
            if !ok {
                return false;
            }
        }
        // Cluster iniziale della padre (root se vuota).
        let mut dir_cluster = self.root_cluster;
        if !parent.is_empty() {
            match self.find(parent) {
                Some(info) if info.is_dir && info.first_cluster >= 2 => {
                    dir_cluster = info.first_cluster;
                }
                _ => return false,
            }
        }
        // Esiste gia'? (find sulla padre + match nome: evita duplicati.)
        if self.find(path).is_some() {
            return false;
        }
        // Slot libero: primo byte 0x00 (fine, riusabile) o 0xE5 (cancellata)
        // a confine 32 B, scorrendo TUTTA la catena. Se piena, un cluster
        // nuovo azzerato in coda (slot = suo offset 0).
        let csize = self.cluster_bytes();
        let mut cluster = dir_cluster;
        let mut base_off = 0usize; // byte stream della dir a inizio cluster
        let slot_off: usize = loop {
            let lba0 = self.data_start as u64 + ((cluster - 2) as u64) * (self.spc as u64);
            let mut sec = [0u8; 512];
            let mut found: Option<usize> = None;
            'scan: for s in 0..self.spc as u64 {
                if !self.disk.read_sector(lba0 + s, &mut sec) {
                    return false;
                }
                let mut i = 0usize;
                while i + 32 <= 512 {
                    let first = sec[i];
                    if first == 0x00 || first == 0xE5 {
                        found = Some(base_off + (s as usize) * 512 + i);
                        break 'scan;
                    }
                    i += 32;
                }
            }
            if let Some(off) = found {
                break off;
            }
            match self.next_cluster(cluster) {
                Some(next) => {
                    cluster = next;
                    base_off += csize;
                }
                None => {
                    // Catena piena: nuovo cluster azzerato in coda.
                    let c = match self.alloc_one() {
                        Some(c) => c,
                        None => return false, // disco pieno
                    };
                    if !self.zero_cluster(c) || !self.set_fat_entry(cluster, c) {
                        return false;
                    }
                    let _ = self.fsinfo_bump(-1, c + 1);
                    break base_off + csize; // offset 0 del nuovo cluster
                }
            }
        };
        // Entry 32 B: nome 11 maiusc + spazi, attr archivio, cluster 0, size 0.
        let mut raw = [b' '; 11];
        raw[..name.len()].copy_from_slice(&name.as_bytes()[..name.len().min(8)]);
        raw[8..8 + ext.len()].copy_from_slice(&ext.as_bytes()[..ext.len().min(3)]);
        let (lba, off) = match self.dir_entry_pos(dir_cluster, slot_off) {
            Some(p) => p,
            None => return false,
        };
        let mut sec = [0u8; 512];
        if !self.disk.read_sector(lba, &mut sec) {
            return false;
        }
        // Slot a cavallo di due settori: quasi impossibile (slot trovati a
        // confine 32 B dentro un settore da 512 = 16 slot esatti), ma gestito.
        let mut spill: Option<[u8; 512]> = None;
        if off + 32 > 512 {
            let mut s2 = [0u8; 512];
            if !self.disk.read_sector(lba + 1, &mut s2) {
                return false;
            }
            spill = Some(s2);
        }
        let mut put = |i: usize, v: u8| {
            if off + i < 512 {
                sec[off + i] = v;
            } else if let Some(ref mut s2) = spill {
                s2[off + i - 512] = v;
            }
        };
        for i in 0..11 {
            put(i, raw[i]);
        }
        put(11, 0x20); // archivio
        for i in 12..32 {
            put(i, 0);
        }
        if !self.disk.write_sector(lba, &sec) {
            return false;
        }
        if let Some(s2) = spill {
            if !self.disk.write_sector(lba + 1, &s2) {
                return false;
            }
        }
        true
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
