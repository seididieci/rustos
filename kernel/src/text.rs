//! Shared text (Fase 32): immagini immutabili (codice `RX` + rodata `RO`)
//! condivise tra le istanze dello stesso binario, con refcount. Il loader ELF
//! (`crate::elf`) carica `[base, rw_off)` da qui e `[rw_off, end)` privatamente.
//!
//! Identita': hash FNV-1a dell'ELF + **verifica byte-per-byte** del contenuto
//! immutabile su hit (input da disco non fidato: una collisione di hash
//! mapperebbe codice sbagliato, quindi non ci si fida del solo hash). Tabella
//! statica (mai heap), stile `SHM_TABLE`; accesso serializzato dallo SCHED lock
//! (spawn e reclaim). Slot pieni → fallback al load privato (mai spawn fallito).
//! Scope: refcount-only (condivide tra istanze concorrenti, libera a 0).

use core::sync::atomic::{AtomicU64, Ordering};
use crate::elf::{Layout, PAGE};

/// Immagini condivise massime (slot statici).
const TEXT_MAX: usize = 16;

/// `(phys, pages, hash, base, rw_off, refs)`; `phys == 0` = slot libero.
static mut TEXT_TABLE: [(u64, u64, u64, u64, u64, u32); TEXT_MAX] =
    [(0, 0, 0, 0, 0, 0); TEXT_MAX];

static HITS: AtomicU64 = AtomicU64::new(0);
static MISSES: AtomicU64 = AtomicU64::new(0);
/// Somma dei refcount vivi (immagini * riferimenti).
static LIVE: AtomicU64 = AtomicU64::new(0);

/// FNV-1a 64-bit sull'ELF (identita' del binario).
fn hash(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Confronta il contenuto immutabile del nuovo ELF con l'immagine in cache
/// (`phys`): copre esattamente il range condiviso `[base, rw_off)`. Sicuro
/// anche in caso di collisione di hash (se i byte differiscono, rifiuta).
unsafe fn verify(bytes: &[u8], l: &Layout, phys: u64) -> bool {
    let img = crate::addr::phys_to_virt(phys) as *const u8;
    for s in &l.segments[..l.nseg] {
        if s.vaddr >= l.rw_off {
            continue; // segmento tutto nel privato
        }
        let n = (s.filesz as u64).min(l.rw_off - s.vaddr) as usize;
        for i in 0..n {
            let cached = unsafe { *img.add((s.vaddr - l.base) as usize + i) };
            if cached != bytes[s.offset + i] {
                return false;
            }
        }
    }
    true
}

/// Copia il contenuto immutabile dei segmenti in `phys` (blocco azzerato).
unsafe fn fill_shared(bytes: &[u8], l: &Layout, phys: u64) {
    let img = crate::addr::phys_to_virt(phys) as *mut u8;
    for s in &l.segments[..l.nseg] {
        if s.vaddr >= l.rw_off {
            continue;
        }
        let n = (s.filesz as u64).min(l.rw_off - s.vaddr) as usize;
        let dst = unsafe { img.add((s.vaddr - l.base) as usize) };
        unsafe { core::ptr::copy_nonoverlapping(bytes[s.offset..s.offset + n].as_ptr(), dst, n); }
    }
}

/// Acquisisce (o crea) l'immagine condivisa per `bytes`/`layout`. Ritorna
/// l'id (>= 1) o `None` (nessuna parte condivisibile, tabella piena, OOM →
/// fallback privato a carico del chiamante). `refs` incrementato.
///
/// # Safety
/// Chiamato dal loader con lo SCHED lock trattenuto (spawn/reclaim).
pub unsafe fn acquire(bytes: &[u8], l: &Layout) -> Option<u32> {
    let pages = ((l.rw_off - l.base) / PAGE) as usize;
    if pages == 0 {
        return None;
    }
    let h = hash(bytes);
    for i in 0..TEXT_MAX {
        let e = unsafe { *core::ptr::addr_of!(TEXT_TABLE[i]) };
        if e.0 != 0
            && e.2 == h
            && e.1 == pages as u64
            && e.3 == l.base
            && e.4 == l.rw_off
            && unsafe { verify(bytes, l, e.0) }
        {
            unsafe { *core::ptr::addr_of_mut!(TEXT_TABLE[i].5) += 1; }
            HITS.fetch_add(1, Ordering::Relaxed);
            LIVE.fetch_add(1, Ordering::Relaxed);
            return Some(i as u32 + 1);
        }
    }
    // Miss: costruisci l'immagine.
    let phys = crate::phys_mem::alloc_contiguous(pages)?;
    unsafe {
        core::ptr::write_bytes(crate::addr::phys_to_virt(phys) as *mut u8, 0, pages * PAGE as usize);
        fill_shared(bytes, l, phys);
    }
    for i in 0..TEXT_MAX {
        let e = unsafe { *core::ptr::addr_of!(TEXT_TABLE[i]) };
        if e.0 == 0 {
            unsafe {
                *core::ptr::addr_of_mut!(TEXT_TABLE[i]) = (phys, pages as u64, h, l.base, l.rw_off, 1);
            }
            MISSES.fetch_add(1, Ordering::Relaxed);
            LIVE.fetch_add(1, Ordering::Relaxed);
            return Some(i as u32 + 1);
        }
    }
    crate::phys_mem::free_contiguous(phys, pages);
    None
}

/// Mappa il range immutabile `[base, rw_off)` di `id` in `cr3` come pagine
/// condivise read-only (`RX` dove c'e' codice, `RO` altrove), NON owned.
///
/// # Safety
/// `cr3` valido; `id` da `acquire`; `layout` coerente con l'immagine.
pub unsafe fn map_shared(cr3: u64, l: &Layout, id: u32) {
    if id == 0 || id as usize > TEXT_MAX {
        return;
    }
    let e = unsafe { *core::ptr::addr_of!(TEXT_TABLE[id as usize - 1]) };
    if e.0 == 0 {
        return;
    }
    let pages = ((l.rw_off - l.base) / PAGE) as usize;
    for p in 0..pages {
        let va = l.base + (p as u64) * PAGE;
        let (_w, x) = crate::elf::page_flags(l, va);
        unsafe {
            crate::vmm_user::map_user_leaf_shared(cr3, va, e.0 + (p as u64) * PAGE, x);
        }
    }
}

/// Rilascia un riferimento: a 0 libera i frame contigui e azzera lo slot.
pub fn release(id: u32) {
    if id == 0 || id as usize > TEXT_MAX {
        return;
    }
    let idx = id as usize - 1;
    let e = unsafe { *core::ptr::addr_of!(TEXT_TABLE[idx]) };
    if e.0 == 0 {
        return;
    }
    LIVE.fetch_sub(1, Ordering::Relaxed);
    let refs = e.5.saturating_sub(1);
    if refs == 0 {
        crate::phys_mem::free_contiguous(e.0, e.1 as usize);
        unsafe { *core::ptr::addr_of_mut!(TEXT_TABLE[idx]) = (0, 0, 0, 0, 0, 0); }
    } else {
        unsafe { *core::ptr::addr_of_mut!(TEXT_TABLE[idx].5) = refs; }
    }
}

/// `(hits, misses, live)`: contatori per la syscall `text_stats` (debug/test).
pub fn stats() -> (u64, u64, u64) {
    (
        HITS.load(Ordering::Relaxed),
        MISSES.load(Ordering::Relaxed),
        LIVE.load(Ordering::Relaxed),
    )
}
