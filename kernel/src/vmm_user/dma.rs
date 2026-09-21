// Split from vmm_user.rs (new module; see facade).
/// Staging DMA per processo (Fase 38.1, ATA DMA): UNA allocazione single-slot
/// di frame fisici contigui (PRD + dati), via `SYS_DMA_ALLOC`.
///
/// Disegno come i ring (`rings.rs`): mapping NON-owned (free esattamente una
/// volta via record, mai double-free col walk owned), record per-PID, free a
/// teardown/exec, mai ereditato dal fork (il figlio faulta come i ring).
/// Differenze dai ring: frame CONTIGUI (`alloc_contiguous` — il device li
/// attraversa in hardware) + count nel record (1..=DMA_PAGES_MAX).
const DMA_MAX_PROCS: usize = 128;
/// (phys_base, count): (0, 0) = slot libero.
static mut DMA_PHYS: [(u64, u64); DMA_MAX_PROCS] = [(0, 0); DMA_MAX_PROCS];

/// Alloca `pages` (1..=`syscall_numbers::DMA_PAGES_MAX`) frame contigui
/// azzerati al processo `pid`. `None` se lo slot e' occupato (single-slot:
/// una sola staging per processo), OOM, o `pages` fuori range. Il mapping
/// alla VA (`USER_DMA_VA`) e' a carico del chiamante (syscall).
pub fn alloc_dma_pages(pid: usize, pages: usize) -> Option<(u64, usize)> {
    if pages == 0 || pages > syscall_numbers::DMA_PAGES_MAX || pid >= DMA_MAX_PROCS {
        return None;
    }
    let (base, _) = unsafe { *core::ptr::addr_of!(DMA_PHYS[pid]) };
    if base != 0 {
        return None; // single-slot: gia' allocata
    }
    let phys = crate::phys_mem::alloc_contiguous(pages)?;
    unsafe {
        core::ptr::write_bytes(crate::addr::phys_to_virt(phys) as *mut u8, 0, pages * 4096);
        *core::ptr::addr_of_mut!(DMA_PHYS[pid]) = (phys, pages as u64);
    }
    Some((phys, pages))
}

/// Libera la staging DMA del processo `pid` e azzera il record (teardown e
/// exec: single path col walk owned, che salta il mapping NON-owned).
/// `pub(crate)` per exec (la nuova immagine rialloca se serve).
pub(crate) fn free_dma_pages(pid: usize) {
    if pid >= DMA_MAX_PROCS {
        return;
    }
    let (base, count) = unsafe { *core::ptr::addr_of!(DMA_PHYS[pid]) };
    if base != 0 {
        crate::phys_mem::free_contiguous(base, count as usize);
        unsafe {
            *core::ptr::addr_of_mut!(DMA_PHYS[pid]) = (0, 0);
        }
    }
}
