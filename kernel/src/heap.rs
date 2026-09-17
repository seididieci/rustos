//! Kernel heap allocator — linked_list_allocator come global allocator.
//!
//! L'heap è posizionato dopo kernel + bitmap. Dopo `init()`, `Box<T>`,
//! `Vec<T>` e `String` funzionano.
//!
//! Nota: la regione dell'heap deve essere RISERVATA nel frame allocator fisico
//! (vedi `main.rs` / `phys_mem::reserve`), altrimenti i frame che la compongono
//! verrebbero dati ai processi e sovrascriverebbero la free-list.

use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicUsize, Ordering};

use linked_list_allocator::LockedHeap;

#[global_allocator]
static ALLOCATOR: AuditedHeap = AuditedHeap::new();

pub const HEAP_SIZE: usize = 4 * 1024 * 1024; // 4 MiB

/// Global allocator con contatore di byte outstanding (allocati meno
/// liberati), per l'invariante "no-alloc sui percorsi caldi": dopo il warmup
/// di boot (spawn dei servizi, capacita' `Vec` satura) il valore deve restare
/// piatto — i percorsi tick/syscall/IPC non allocano piu' (vedi AGENTS,
/// versione ibrida). Delega tutto al `LockedHeap`; il contatore costa 2
/// atomiche per op su percorsi gia' rari, nessun lock aggiuntivo.
struct AuditedHeap {
    inner: LockedHeap,
    outstanding: AtomicUsize,
    /// Allocazioni totali dall'avvio (monotono: se cresce in steady state,
    /// qualcosa alloca anche se poi libera — `outstanding` da solo non lo
    /// vedrebbe).
    allocs_total: AtomicUsize,
}

impl AuditedHeap {
    const fn new() -> Self {
        Self {
            inner: LockedHeap::empty(),
            outstanding: AtomicUsize::new(0),
            allocs_total: AtomicUsize::new(0),
        }
    }
}

unsafe impl GlobalAlloc for AuditedHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: contratta da `GlobalAlloc`; `&self` da static, come prima.
        let p = unsafe { self.inner.alloc(layout) };
        if !p.is_null() {
            self.outstanding.fetch_add(layout.size(), Ordering::Relaxed);
            self.allocs_total.fetch_add(1, Ordering::Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: come sopra; il layout in dealloc e' quello dell'alloc.
        unsafe { self.inner.dealloc(ptr, layout) };
        self.outstanding.fetch_sub(layout.size(), Ordering::Relaxed);
    }
}

/// Byte heap correntemente outstanding. Lettura atomica, nessun lock:
/// usabile anche nei log sotto IRQ (riga `[sched] tick=` con sched_debug).
/// Compilata sempre (infrastruttura dell'invariante), usata solo in debug.
#[cfg_attr(not(feature = "sched_debug"), allow(dead_code))]
pub fn outstanding() -> usize {
    ALLOCATOR.outstanding.load(Ordering::Relaxed)
}

/// Allocazioni totali dall'avvio (monotono). Con `outstanding`: entrambi
/// piatti in steady state = zero allocazioni, non solo zero leak.
#[cfg_attr(not(feature = "sched_debug"), allow(dead_code))]
pub fn allocs_total() -> usize {
    ALLOCATOR.allocs_total.load(Ordering::Relaxed)
}

pub fn init(heap_start: u64) {
    let heap_end = heap_start + HEAP_SIZE as u64;

    unsafe {
        ALLOCATOR.inner.lock().init(heap_start as *mut u8, HEAP_SIZE);
    }

    crate::serial_println!(
        "[heap] {:#x} - {:#x} ({} KiB)",
        heap_start,
        heap_end,
        HEAP_SIZE / 1024
    );
}

#[alloc_error_handler]
fn alloc_error(layout: core::alloc::Layout) -> ! {
    panic!("alloc error: {:?}", layout);
}
