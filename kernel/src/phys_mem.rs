//! Physical frame allocator — bitmap dinamica, 1 frame = 4 KiB.
//!
//! La bitmap è piazzata subito dopo `_kernel_end` e dimensionata a runtime
//! in base all'indirizzo fisico più alto trovato nella memory map.

use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

pub const FRAME_SIZE: u64 = 4096;

static TOTAL: AtomicU64 = AtomicU64::new(0);
static FREE: AtomicU64 = AtomicU64::new(0);

/// Puntatore alla bitmap e sua dimensione (inizializzati da `init`).
static mut BITMAP_PTR: *mut u8 = core::ptr::null_mut();
static mut BITMAP_BYTES: usize = 0;

static LOCK: Mutex<()> = Mutex::new(());

// ── Funzioni bitmap ─────────────────────────────────────────────────

fn is_used(frame: usize) -> bool {
    unsafe {
        let byte = *BITMAP_PTR.add(frame / 8);
        byte & (1 << (frame % 8)) != 0
    }
}

fn mark_used(frame: usize) {
    unsafe {
        let p = BITMAP_PTR.add(frame / 8);
        *p |= 1 << (frame % 8);
    }
}

fn mark_free(frame: usize) {
    unsafe {
        let p = BITMAP_PTR.add(frame / 8);
        *p &= !(1 << (frame % 8));
    }
}

// ── Inizializzazione ───────────────────────────────────────────────

pub fn init(
    memmap: &[crate::boot_info::HvmMemmapEntry],
    kernel_start_virt: u64,
    kernel_end_virt: u64,
) {
    use crate::addr::{kern_virt_to_phys, phys_to_virt};
    // I bound dell'immagine arrivano come VIRT (linker); la contabilita'
    // frame e' in PHYS (H0: identici; H1: scarto KERNEL_OFFSET).
    let kernel_start = kern_virt_to_phys(kernel_start_virt);
    let kernel_end = kern_virt_to_phys(kernel_end_virt);
    // 1. Trova l'indirizzo fisico più alto dalla memory map.
    let max_addr = memmap
        .iter()
        .filter(|e| e.kind == crate::boot_info::MEM_RAM)
        .map(|e| e.addr + e.size)
        .max()
        .unwrap_or(256 * 1024 * 1024);

    let total = (max_addr + FRAME_SIZE - 1) / FRAME_SIZE;
    TOTAL.store(total, Ordering::Relaxed);

    let bitmap_bytes = ((total as usize) + 7) / 8;

    // 2. Piazza la bitmap subito dopo _kernel_end (page-aligned). La bitmap
    // vive in RAM generica: indirizzo PHYS per la contabilita', VIRT
    // (direct map) per accedervi. `bitmap_end` resta VIRT (base heap).
    let bitmap_phys = align_up(kernel_end, FRAME_SIZE);

    unsafe {
        BITMAP_PTR = phys_to_virt(bitmap_phys) as *mut u8;
        BITMAP_BYTES = bitmap_bytes;

        // 3. Fill 0xFF = tutti usati.
        let slice = core::slice::from_raw_parts_mut(BITMAP_PTR, BITMAP_BYTES);
        slice.fill(0xFF);
    }

    // 4. Libera le regioni MEM_RAM.
    for entry in memmap {
        if entry.kind == crate::boot_info::MEM_RAM {
            let start_frame = (entry.addr / FRAME_SIZE) as usize;
            let end_frame = ((entry.addr + entry.size) / FRAME_SIZE) as usize;
            let end = (end_frame as u64).min(total) as usize;
            for f in start_frame..end {
                mark_free(f);
                FREE.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    // 5. Ri-marca usati: kernel, page tables, PD, bitmap.
    let ks = (kernel_start / FRAME_SIZE) as usize;
    let ke = (align_up(kernel_end, FRAME_SIZE) / FRAME_SIZE) as usize;
    for f in ks..ke {
        if !is_used(f) {
            mark_used(f);
            FREE.fetch_sub(1, Ordering::Relaxed);
        }
    }

    // Tabelle di boot (H1: PML4 + PDPT/PD/PT low + PDPT_K/PD_K +
    // PDPT_DIRECT + 32 PD direct = 39 pagine) in 0x90000–0x100000.
    // LMA fisse (CR3 phys); la VMA e' alta ma la contabilita' e' in PHYS.
    let pt_start = 0x90000 / FRAME_SIZE as usize;  // frame 36
    let pt_end = 0x100000 / FRAME_SIZE as usize;    // frame 256
    for f in pt_start..pt_end {
        if !is_used(f) {
            mark_used(f);
            FREE.fetch_sub(1, Ordering::Relaxed);
        }
    }

    // Bitmap stessa (contabilita' in PHYS)
    let bfn = (bitmap_phys / FRAME_SIZE) as usize;
    let bfe = (align_up(bitmap_phys + bitmap_bytes as u64, FRAME_SIZE) / FRAME_SIZE) as usize;
    for f in bfn..bfe {
        if !is_used(f) {
            mark_used(f);
            FREE.fetch_sub(1, Ordering::Relaxed);
        }
    }

    // VGA buffer (0xB8000): frame 186
    if !is_used(186) {
        mark_used(186);
        FREE.fetch_sub(1, Ordering::Relaxed);
    }

    crate::serial_println!(
        "[pmm] {} frame totali, {} liberi ({} MiB)",
        total,
        FREE.load(Ordering::Relaxed),
        FREE.load(Ordering::Relaxed) * 4 / 1024
    );
}

/// Ritorna l'indirizzo di fine bitmap (page-aligned), utile per posizionare l'heap.
pub fn bitmap_end() -> u64 {
    let start = unsafe { BITMAP_PTR as u64 };
    let bytes = unsafe { BITMAP_BYTES } as u64;
    align_up(start + bytes, FRAME_SIZE)
}

/// Marca come USATI i frame nell'intervallo fisico [start, start+len): il
/// frame allocator non li restituira' mai piu'. Necessario per regioni carvate
/// fuori dal bitmap (es. il kernel heap), altrimenti finirebbero in mano ai
/// processi e verrebbero sovrascritti.
pub fn reserve(start: u64, len: u64) {
    let mut f0 = (start / FRAME_SIZE) as usize;
    let f1 = ((align_up(start + len, FRAME_SIZE)) / FRAME_SIZE) as usize;
    while f0 < f1 {
        if !is_used(f0) {
            mark_used(f0);
            FREE.fetch_sub(1, Ordering::Relaxed);
        }
        f0 += 1;
    }
}

// ── Allocazione ────────────────────────────────────────────────────

pub fn alloc() -> Option<u64> {
    let _lock = LOCK.lock();
    let total = TOTAL.load(Ordering::Relaxed) as usize;

    // Salta frame 0 (BIOS IVT, real-mode IDT): mai usabile come pagina utente.
    for i in 1..total {
        if !is_used(i) {
            mark_used(i);
            FREE.fetch_sub(1, Ordering::Relaxed);
            return Some((i as u64) * FRAME_SIZE);
        }
    }
    None
}

/// Alloca `n` frame fisici CONTIGUI. Ritorna l'indirizzo del primo frame,
/// oppure `None` se non c'e' un blocco contiguo di `n` frame liberi.
pub fn alloc_contiguous(n: usize) -> Option<u64> {
    if n == 0 {
        return None;
    }
    let _lock = LOCK.lock();
    let total = TOTAL.load(Ordering::Relaxed) as usize;

    let mut i = 0usize;
    while i <= total - n {
        if !is_used(i) {
            let mut ok = true;
            for j in 1..n {
                if is_used(i + j) {
                    ok = false;
                    i += j; // salta il blocco occupato trovato
                    break;
                }
            }
            if ok {
                for j in 0..n {
                    mark_used(i + j);
                }
                FREE.fetch_sub(n as u64, Ordering::Relaxed);
                return Some((i as u64) * FRAME_SIZE);
            }
        } else {
            i += 1;
        }
    }
    None
}

pub fn free(frame: u64) {
    let _lock = LOCK.lock();
    let i = (frame / FRAME_SIZE) as usize;
    assert!((i as u64) < TOTAL.load(Ordering::Relaxed), "frame fuori range: {:#x}", frame);
    assert!(is_used(i), "frame già libero: {:#x}", frame);
    mark_free(i);
    FREE.fetch_add(1, Ordering::Relaxed);
}

/// Libera `count` frame CONTIGUI a partire da `start` (Fase 14, teardown dei
/// processi: stack kernel, binario copiato, etc.). Il blocco deve essere stato
/// allocato contiguo (`alloc_contiguous`).
pub fn free_contiguous(start: u64, count: usize) {
    let _lock = LOCK.lock();
    let f0 = (start / FRAME_SIZE) as usize;
    for i in 0..count {
        let idx = f0 + i;
        assert!((idx as u64) < TOTAL.load(Ordering::Relaxed), "frame fuori range: {:#x}", start);
        assert!(is_used(idx), "frame già libero: {:#x}", (idx as u64) * FRAME_SIZE);
        mark_free(idx);
    }
    FREE.fetch_add(count as u64, Ordering::Relaxed);
}

pub fn free_frames() -> u64 {
    FREE.load(Ordering::Relaxed)
}

/// Usato solo dai selftest (`#[cfg(feature = "selftest")]` in main.rs).
#[allow(dead_code)]
pub fn used_frames() -> u64 {
    TOTAL.load(Ordering::Relaxed) - FREE.load(Ordering::Relaxed)
}

fn align_up(value: u64, alignment: u64) -> u64 {
    (value + alignment - 1) & !(alignment - 1)
}
