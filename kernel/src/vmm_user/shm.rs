//! Shared memory (Fase M3): regioni fisiche condivise tra processi.
//!
//! `shm_create` alloca frame contigui azzerati e ritorna un id (>= 1);
//! `shm_map` mappa la regione in una VMA del processo (PTE non-owned,
//! pre-materializzate: niente demand-zero, le pagine esistono da subito e
//! sono le STESSE per tutti i mappatori) e incrementa un refcount; `munmap`
//! e il teardown del processo rilasciano il riferimento, e a 0 i frame
//! contigui vengono liberati. Tabella statica (mai heap), stesso stile di
//! `VMA_TABLE`/`RING_PHYS`.

use super::layout::PAGE_SIZE;

/// Regioni condivise massime (slot statici).
const SHM_MAX: usize = 16;
/// Pagine massime per regione (256 KiB): bound strutturale sui frame contigui.
const SHM_MAX_FRAMES: usize = 64;

/// `(phys_base, frames, refs)`; `phys == 0` = slot libero.
static mut SHM_TABLE: [(u64, u64, u32); SHM_MAX] = [(0, 0, 0); SHM_MAX];

/// Crea una regione condivisa di `len` byte (arrotondata a pagina, max
/// `SHM_MAX_FRAMES` pagine) con frame contigui azzerati. Ritorna l'id (>= 1)
/// o `None`. `refs` parte da 0: le mappature via `shm_map` la referenziano
/// (una creazione mai mappata resta finche' il processo non mappa: caso
/// d'uso normale e' create+map).
pub fn shm_create(len: u64) -> Option<u32> {
    if len == 0 {
        return None;
    }
    let frames = (len.checked_add(PAGE_SIZE - 1)? / PAGE_SIZE) as usize;
    if frames == 0 || frames > SHM_MAX_FRAMES {
        return None;
    }
    let phys = crate::phys_mem::alloc_contiguous(frames)?;
    unsafe {
        core::ptr::write_bytes(
            crate::addr::phys_to_virt(phys) as *mut u8,
            0,
            frames * PAGE_SIZE as usize,
        );
    }
    for i in 0..SHM_MAX {
        let e = unsafe { *core::ptr::addr_of!(SHM_TABLE[i]) };
        if e.0 == 0 {
            unsafe { *core::ptr::addr_of_mut!(SHM_TABLE[i]) = (phys, frames as u64, 0); }
            return Some(i as u32 + 1);
        }
    }
    crate::phys_mem::free_contiguous(phys, frames);
    None
}

/// `(phys_base, frames)` della regione `id`, se viva.
pub fn shm_region(id: u32) -> Option<(u64, u64)> {
    if id == 0 || id as usize > SHM_MAX {
        return None;
    }
    let e = unsafe { *core::ptr::addr_of!(SHM_TABLE[id as usize - 1]) };
    if e.0 == 0 {
        None
    } else {
        Some((e.0, e.1))
    }
}

/// Incrementa il refcount della regione `id` (una mappatura in piu').
pub fn shm_ref(id: u32) {
    if id == 0 || id as usize > SHM_MAX {
        return;
    }
    let idx = id as usize - 1;
    unsafe { *core::ptr::addr_of_mut!(SHM_TABLE[idx].2) += 1; }
}

/// Rilascia un riferimento alla regione `id`: a 0 libera i frame contigui e
/// azzera lo slot. Idempotente su id invalido/gia' libero.
pub fn shm_release(id: u32) {
    if id == 0 || id as usize > SHM_MAX {
        return;
    }
    let idx = id as usize - 1;
    let e = unsafe { *core::ptr::addr_of!(SHM_TABLE[idx]) };
    if e.0 == 0 {
        return;
    }
    let refs = e.2.saturating_sub(1);
    if refs == 0 {
        crate::phys_mem::free_contiguous(e.0, e.1 as usize);
        unsafe { *core::ptr::addr_of_mut!(SHM_TABLE[idx]) = (0, 0, 0); }
    } else {
        unsafe { *core::ptr::addr_of_mut!(SHM_TABLE[idx].2) = refs; }
    }
}
