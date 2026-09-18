//! Loader ELF64 (Fase 31): carica i segmenti `PT_LOAD` di un ELF x86_64 nello
//! spazio user `cr3` con i flag dell'ELF (W^X: `R E`→RX, `R`→RO, `RW`→RW, NX
//! su tutto tranne il codice), all'indirizzo di link (`p_vaddr`). Carichiamo
//! sempre al vaddr di link, quindi le `R_X86_64_RELATIVE` (gia' applicate dal
//! linker con `--apply-dynamic-relocs`) restano valide: **nessuna reloc a
//! runtime**.
//!
//! Due fasi separate per non leakare frame su un ELF malformato:
//! `validate` (nessuna allocazione) e `load` (mappa; OOM = panic come il resto
//! dello spawn). Usato sia dai binari embedded sia da `spawn_image` (ELF letto
//! da disco: input non fidato → validazione stretta).

use crate::vmm_user::{USER_CODE, USER_FS_BUFFER};

const PT_LOAD: u32 = 1;
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const EM_X86_64: u16 = 62;
const ET_EXEC: u16 = 2;
const ET_DYN: u16 = 3;
const PF_X: u32 = 1;
const PF_W: u32 = 2;
const PAGE: u64 = 0x1000;
/// Program header massimi accettati (i nostri binari ne hanno 4-7).
const MAX_PHNUM: usize = 16;
/// Pagine massime dell'immagine (2 MiB: regione [USER_CODE, USER_FS_BUFFER)).
const MAX_PAGES: usize = 512;

/// Un segmento `PT_LOAD` validato.
#[derive(Clone, Copy)]
struct Segment {
    offset: usize,
    vaddr: u64,
    filesz: usize,
    memsz: u64,
    flags: u32,
}

/// Immagine ELF validata (nessuna allocazione fatta).
pub struct Layout {
    entry: u64,
    base: u64,
    npages: usize,
    segments: [Segment; MAX_PHNUM],
    nseg: usize,
}

fn rd16(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}
fn rd32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}
fn rd64(b: &[u8], off: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[off..off + 8]);
    u64::from_le_bytes(a)
}

/// Valida l'ELF e ritorna il layout (nessuna allocazione): `None` se
/// malformato/insicuro (magic, classe, macchina, bound, W+X, entry).
pub fn validate(bytes: &[u8]) -> Option<Layout> {
    if bytes.len() < 64 {
        return None;
    }
    if &bytes[0..4] != b"\x7fELF" {
        return None;
    }
    if bytes[4] != ELFCLASS64 || bytes[5] != ELFDATA2LSB || bytes[6] != 1 {
        return None;
    }
    let e_type = rd16(bytes, 16);
    if e_type != ET_EXEC && e_type != ET_DYN {
        return None;
    }
    if rd16(bytes, 18) != EM_X86_64 {
        return None;
    }
    let e_entry = rd64(bytes, 24);
    let e_phoff = rd64(bytes, 32) as usize;
    let e_ehsize = rd16(bytes, 52);
    let e_phentsize = rd16(bytes, 54) as usize;
    let e_phnum = rd16(bytes, 56) as usize;
    if e_ehsize != 64 || e_phentsize != 56 || e_phnum == 0 || e_phnum > MAX_PHNUM {
        return None;
    }
    if e_phoff.checked_add(e_phnum * 56)? > bytes.len() {
        return None;
    }

    let mut segments = [Segment { offset: 0, vaddr: 0, filesz: 0, memsz: 0, flags: 0 }; MAX_PHNUM];
    let mut nseg = 0usize;
    let mut min_va = u64::MAX;
    let mut max_va = 0u64;
    let mut entry_ok = false;
    for i in 0..e_phnum {
        let ph = e_phoff + i * 56;
        if rd32(bytes, ph) != PT_LOAD {
            continue;
        }
        let p_flags = rd32(bytes, ph + 4);
        let p_offset = rd64(bytes, ph + 8) as usize;
        let p_vaddr = rd64(bytes, ph + 16);
        let p_filesz = rd64(bytes, ph + 32) as usize;
        let p_memsz = rd64(bytes, ph + 40);
        let p_align = rd64(bytes, ph + 48);
        if p_filesz as u64 > p_memsz || p_memsz == 0 {
            return None;
        }
        if p_offset.checked_add(p_filesz)? > bytes.len() {
            return None;
        }
        if p_align != 0 && (p_align & (p_align - 1)) != 0 {
            return None;
        }
        let vend = p_vaddr.checked_add(p_memsz)?;
        if p_vaddr < USER_CODE || vend > USER_FS_BUFFER {
            return None;
        }
        if p_vaddr < min_va {
            min_va = p_vaddr;
        }
        if vend > max_va {
            max_va = vend;
        }
        if p_flags & PF_X != 0 && e_entry >= p_vaddr && e_entry < vend {
            entry_ok = true;
        }
        segments[nseg] = Segment { offset: p_offset, vaddr: p_vaddr, filesz: p_filesz, memsz: p_memsz, flags: p_flags };
        nseg += 1;
    }
    if nseg == 0 || !entry_ok {
        return None;
    }

    let base = min_va & !(PAGE - 1);
    let end = max_va.checked_add(PAGE - 1)? & !(PAGE - 1);
    let npages = (end.checked_sub(base)? / PAGE) as usize;
    if npages == 0 || npages > MAX_PAGES {
        return None;
    }

    // Rifiuto W+X: nessuna pagina puo' essere scrivibile ed eseguibile insieme
    // (invariante di sicurezza; un ELF che lo chiede e' rifiutato). Le pagine
    // coperte da un segmento sono [vaddr, vaddr+memsz) (include il bss).
    let mut page_flags = [0u8; MAX_PAGES];
    for s in &segments[..nseg] {
        let mut bit = 0u8;
        if s.flags & PF_W != 0 {
            bit |= 1;
        }
        if s.flags & PF_X != 0 {
            bit |= 2;
        }
        if bit == 0 {
            continue;
        }
        let first = ((s.vaddr - base) / PAGE) as usize;
        let last = ((s.vaddr + s.memsz - 1 - base) / PAGE) as usize;
        for p in first..=last {
            page_flags[p] |= bit;
        }
    }
    for p in 0..npages {
        if page_flags[p] == 3 {
            return None; // pagina W+X
        }
    }

    Some(Layout { entry: e_entry, base, npages, segments, nseg })
}

/// Carica l'immagine validata in `cr3`: alloca un blocco contiguo di `npages`,
/// azzera, copia i segmenti (file bytes), mappa ogni pagina con i flag
/// dell'ELF (RX/RO/RW + NX, owned). OOM → panic (come il resto dello spawn).
///
/// # Safety
/// `cr3` deve essere un address space appena creato da `new_address_space`;
/// `layout` deve venire da `validate` sullo stesso `bytes`.
pub unsafe fn load(cr3: u64, bytes: &[u8], layout: &Layout) {
    let phys = crate::phys_mem::alloc_contiguous(layout.npages).expect("oom per l'ELF");
    let base_ptr = crate::addr::phys_to_virt(phys) as *mut u8;
    unsafe {
        core::ptr::write_bytes(base_ptr, 0, layout.npages * PAGE as usize);
    }
    // Copia i file bytes di ogni segmento all'offset (vaddr - base): i buchi
    // e il bss (memsz - filesz) restano zero (blocco azzerato sopra).
    for s in &layout.segments[..layout.nseg] {
        let dst = unsafe { base_ptr.add((s.vaddr - layout.base) as usize) };
        let src = &bytes[s.offset..s.offset + s.filesz];
        unsafe {
            core::ptr::copy_nonoverlapping(src.as_ptr(), dst, s.filesz);
        }
    }
    // Mappa per pagina con i flag risultanti (nessuna pagina W+X, garantito).
    for p in 0..layout.npages {
        let va = layout.base + (p as u64) * PAGE;
        let mut writable = false;
        let mut executable = false;
        for s in &layout.segments[..layout.nseg] {
            if va + PAGE > s.vaddr && va < s.vaddr + s.memsz {
                if s.flags & PF_W != 0 {
                    writable = true;
                }
                if s.flags & PF_X != 0 {
                    executable = true;
                }
            }
        }
        unsafe {
            crate::vmm_user::map_user_leaf(cr3, va, phys + (p as u64) * PAGE, writable, executable);
        }
    }
    let _ = layout.entry;
}

/// Entry point dell'immagine validata.
pub fn entry(layout: &Layout) -> u64 {
    layout.entry
}
