//! Tabella partizioni MBR (Fase 16).
//!
//! Solo voci primarie (4 a `0x1BE`, 16 byte l'una): tipo, LBA iniziale e
//! dimensione. Niente catene extended/logical (futuro documentato). Se il
//! settore 0 non ha la firma `0x55AA` (es. `fat.img` attuale: BPB raw, disco
//! non partizionato) → zero partizioni, nessun errore: il whole-disk resta
//! l'unico nodo e `/fat` continua a montarlo come oggi.

/// Voce di partizione primaria.
pub struct PartInfo {
    /// Byte tipo MBR (0x0B/0x0C = FAT32, 0x83 = Linux, ...).
    pub ptype: u8,
    /// Primo LBA della partizione.
    pub start: u32,
    /// Settori della partizione.
    pub sectors: u32,
}

/// Firma MBR a fine settore.
const MBR_SIG0: u8 = 0x55;
const MBR_SIG1: u8 = 0xAA;

/// Parsa le voci primarie dal settore 0. Ritorna le partizioni non vuote
/// (tipo != 0 e size != 0), al massimo 4.
pub fn parse_mbr(sector0: &[u8; 512], out: &mut alloc::vec::Vec<PartInfo>) {
    if sector0[510] != MBR_SIG0 || sector0[511] != MBR_SIG1 {
        return;
    }
    for i in 0..4 {
        let base = 0x1BE + i * 16;
        let ptype = sector0[base + 4];
        let start = u32::from_le_bytes([
            sector0[base + 8],
            sector0[base + 9],
            sector0[base + 10],
            sector0[base + 11],
        ]);
        let sectors = u32::from_le_bytes([
            sector0[base + 12],
            sector0[base + 13],
            sector0[base + 14],
            sector0[base + 15],
        ]);
        if ptype != 0 && sectors != 0 {
            out.push(PartInfo { ptype, start, sectors });
        }
    }
}
