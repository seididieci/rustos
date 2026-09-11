//! Kernel heap allocator — linked_list_allocator come global allocator.
//!
//! L'heap è posizionato dopo kernel + bitmap. Dopo `init()`, `Box<T>`,
//! `Vec<T>` e `String` funzionano.
//!
//! Nota: la regione dell'heap deve essere RISERVATA nel frame allocator fisico
//! (vedi `main.rs` / `phys_mem::reserve`), altrimenti i frame che la compongono
//! verrebbero dati ai processi e sovrascriverebbero la free-list.

use linked_list_allocator::LockedHeap;

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

pub const HEAP_SIZE: usize = 4 * 1024 * 1024; // 4 MiB

pub fn init(heap_start: u64) {
    let heap_end = heap_start + HEAP_SIZE as u64;

    unsafe {
        ALLOCATOR.lock().init(heap_start as *mut u8, HEAP_SIZE);
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
