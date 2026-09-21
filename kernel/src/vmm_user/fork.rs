//! Walk COW dell'address space per `fork` (Fase 34, ADR-0024).
//!
//! `fork_share` duplica le foglie user del padre nello spazio del figlio:
//!   - foglie `owned` → condivise in COW (ref++, entrambi i lati `RO`+`COW`);
//!   - foglie non-owned (text image, shm, iniettate) → specchiate identiche;
//!   - finestre ring (`USER_FS_BUFFER`/`USER_RESP_RING`) e staging DMA
//!     (`USER_DMA_VA`) → SALTATE (il figlio non eredita ne' i ring FS ne' la
//!     staging device-mem: usarli Killed col page-fault, mai corruzione);
//!   - foglie large-page (PS) → errore (lo user usa solo 4 KiB; fallire forte
//!     invece di divergere in silenzio).
//!
//! Fallibile (OOM nelle tabelle del figlio → false): il padre resta consistente
//! (le pagine gia' convertite a COW si privatizzano al write) e il chiamante
//! distrugge lo spazio parziale del figlio (i `deref` bilanciano da soli).
//! Ordine per pagina owned: converti padre → mappa figlio → `ref_inc`
//! (assert: saturazione irraggiungibile con 32 processi): su OOM nessun ghost
//! ref (l'inc avviene solo a mappa riuscita).

use super::layout::{USER_FS_BUFFER, USER_RESP_RING, USER_DMA_VA};
use super::paging::{kernel_cr3, map_leaf_raw, read_leaf, share_parent_leaf, PTE_PRESENT};
use super::teardown::raw_entry;

/// Condivide l'address space `parent_cr3` in `child_cr3` (entrambi spazi user
/// validi; il figlio tipicamente fresco da `new_address_space`). Ritorna false
/// su OOM (spazio figlio parziale: il chiamante fa teardown).
pub fn fork_share(parent_cr3: u64, child_cr3: u64) -> bool {
    let kernel_pml4 = kernel_cr3();
    for i in 0..512 {
        let e = unsafe { raw_entry(parent_cr3, i) };
        if e & PTE_PRESENT == 0 {
            continue;
        }
        // Sottoalbero condiviso col kernel (U=0): non del processo.
        let k = unsafe { raw_entry(kernel_pml4, i) } & super::layout::PTE_ADDR_MASK;
        if super::layout::PTE_ADDR_MASK & e == k {
            continue;
        }
        if !fork_pdp(parent_cr3, child_cr3, e, i) {
            return false;
        }
    }
    true
}

fn fork_pdp(parent_cr3: u64, child_cr3: u64, pml4e: u64, i: usize) -> bool {
    let pdp = super::layout::PTE_ADDR_MASK & pml4e;
    for j in 0..512 {
        let e = unsafe { raw_entry(pdp, j) };
        if e & PTE_PRESENT == 0 {
            continue;
        }
        if !fork_pd(parent_cr3, child_cr3, e, i, j) {
            return false;
        }
    }
    true
}

fn fork_pd(parent_cr3: u64, child_cr3: u64, pdpe: u64, i: usize, j: usize) -> bool {
    let pd = super::layout::PTE_ADDR_MASK & pdpe;
    for k in 0..512 {
        let e = unsafe { raw_entry(pd, k) };
        if e & PTE_PRESENT == 0 {
            continue;
        }
        if !fork_pt(parent_cr3, child_cr3, e, i, j, k) {
            return false;
        }
    }
    true
}

fn fork_pt(parent_cr3: u64, child_cr3: u64, pde: u64, i: usize, j: usize, k: usize) -> bool {
    // Foglia large-page 2M (PS, bit 7): lo user non ne ha mai (solo 4 KiB).
    if pde & 0x80 != 0 {
        crate::serial_println!("[fork] large page in spazio user: rifiuto");
        return false;
    }
    let pt = super::layout::PTE_ADDR_MASK & pde;
    for l in 0..512 {
        let va = ((i as u64) << 39) | ((j as u64) << 30) | ((k as u64) << 21) | ((l as u64) << 12);
        let e = unsafe { raw_entry(pt, l) };
        if e & PTE_PRESENT == 0 {
            continue;
        }
        // Large-page 1G non possibile a questo livello (saremmo nel ramo PS
        // sopra); le PT contengono solo foglie 4 KiB.
        if va == USER_FS_BUFFER || va == USER_RESP_RING
            || (va >= USER_DMA_VA
                && va < USER_DMA_VA + syscall_numbers::DMA_PAGES_MAX as u64 * 0x1000)
        {
            continue; // ring FS / staging DMA: non ereditati (il figlio li faulta)
        }
        // Foglia owned → COW simmetrico; altrimenti mirror identico.
        match share_parent_leaf(parent_cr3, va) {
            Some((phys, child_flags)) => {
                if !map_leaf_raw(child_cr3, va, phys, child_flags) {
                    return false; // OOM: niente inc, niente ghost ref
                }
                assert!(crate::phys_mem::ref_inc(phys), "fork: refcount saturo");
            }
            None => {
                match read_leaf(parent_cr3, va) {
                    Some((phys, flags)) => {
                        if !map_leaf_raw(child_cr3, va, phys, flags) {
                            return false;
                        }
                    }
                    None => {}
                }
            }
        }
    }
    true
}
