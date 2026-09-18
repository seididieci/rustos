use super::*;
use super::types::{BAD_CLUSTER, EOC};

impl<B: BlockSource> Fat32<B> {
    /// Valore della entry FAT per `cluster` (28 bit utili).
    fn fat_entry(&self, cluster: u32) -> u32 {
        let byte_off = cluster as usize * 4;
        let lba = self.fat_start + (byte_off / 512) as u32;
        let off = byte_off % 512;
        let sec = match self.fat_sector(lba) {
            Some(s) => s,
            None => return BAD_CLUSTER,
        };
        u32::from_le_bytes([sec[off], sec[off + 1], sec[off + 2], sec[off + 3]]) & 0x0FFFFFFF
    }

    /// Prossimo cluster della catena, `None` a fine catena / errore.
    pub(crate) fn next_cluster(&self, cluster: u32) -> Option<u32> {
        let v = self.fat_entry(cluster);
        if v >= EOC || v == BAD_CLUSTER {
            None
        } else {
            Some(v)
        }
    }

    /// Valore EOC da scrivere nelle entry FAT allocate.
    const EOC_VAL: u32 = 0x0FFFFFFF;

    /// Scrive il valore (28 bit, nibble alto preservato) nella entry FAT di
    /// `cluster`, in TUTTE le copie. Ritorna false su errore IO.
    pub(crate) fn set_fat_entry(&self, cluster: u32, val: u32) -> bool {
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
    pub(crate) fn alloc_one(&self) -> Option<u32> {
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
    pub(crate) fn chain_tail(&self, start: u32) -> Option<(usize, u32)> {
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

    /// Aggiorna FSInfo (settore 1): free count di `delta` (negativo in
    /// allocazione) + hint next-free. Silente se il settore non ha le firme
    /// (volume senza FSInfo: niente da aggiornare).
    pub(crate) fn fsinfo_bump(&self, delta: i64, next_free: u32) -> bool {
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
}
