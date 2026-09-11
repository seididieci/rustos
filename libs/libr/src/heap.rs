//! Allocatore di heap on-demand per i processi user (strada B).
//!
//! UNICO allocatore di tutto il userland, vive in `libr` come
//! `#[global_allocator]`: ogni binario che linka `libr` lo usa automaticamente.
//!
//! Gestisce un'arena che NON e' pre-allocata: parte vuota e cresce via la
//! syscall `sbrk` (25) quando non trova blocchi liberi sufficienti. Il kernel
//! si limita a riservare VA (`heap_brk`); le pagine vengono materializzate
//! lazy dal page-fault handler al primo accesso (demand-zero, come
//! `brk`/`mmap` di Linux): nessuna riserva statica nel binario, la RAM fisica
//! e' proporzionale alle pagine realmente toccate.
//!
//! Algoritmo: free-list first-fit con split e coalescenza dei blocchi
//! fisicamente adiacenti (una volta liberati non tornano al kernel).

use core::alloc::{GlobalAlloc, Layout};
use core::ptr;

/// Header di un blocco (in testa a ogni blocco allocato o libero).
/// `size` = dimensione TOTALE del blocco (header incluso).
#[repr(C)]
struct Header {
    size: usize,
    next: *mut Header,
}

const HEADER: usize = core::mem::size_of::<Header>();

/// Testa della free-list dei blocchi liberi.
static mut FREE_HEAD: *mut Header = ptr::null_mut();

#[inline]
fn align8(n: usize) -> usize {
    (n + 7) & !7
}

/// Alloca un blocco di `need` byte payload dalla free-list (con split).
/// Ritorna il puntatore al payload, o `None` se nessun blocco basta.
unsafe fn first_fit(need: usize) -> Option<*mut u8> {
    unsafe {
        let head_slot = ptr::addr_of_mut!(FREE_HEAD);
        let mut prev: *mut Header = ptr::null_mut();
        let mut cur = *head_slot;
        while !cur.is_null() {
            let block_size = (*cur).size;
            let old_next = (*cur).next;
            if cur as usize + HEADER + need <= cur as usize + block_size {
                let used = HEADER + need;
                if block_size - used >= HEADER {
                    // Split: il resto diventa un nuovo blocco libero.
                    let new_free = (cur as usize + used) as *mut Header;
                    (*new_free).size = block_size - used;
                    (*new_free).next = old_next;
                    if prev.is_null() {
                        *head_slot = new_free;
                    } else {
                        (*prev).next = new_free;
                    }
                } else {
                    // Blocco intero.
                    if prev.is_null() {
                        *head_slot = old_next;
                    } else {
                        (*prev).next = old_next;
                    }
                }
                (*cur).size = used;
                (*cur).next = ptr::null_mut();
                return Some((cur as usize + HEADER) as *mut u8);
            }
            prev = cur;
            cur = old_next;
        }
        None
    }
}

/// Aggiunge un blocco (indirizzo `addr`, dimensione `len`) in testa alla
/// free-list e lancia la coalescenza.
unsafe fn push_free(addr: usize, len: usize) {
    unsafe {
        let b = addr as *mut Header;
        (*b).size = len;
        (*b).next = *ptr::addr_of!(FREE_HEAD);
        *ptr::addr_of_mut!(FREE_HEAD) = b;
        coalesce();
    }
}

/// Fonde i blocchi liberi fisicamente adiacenti (O(n^2), n piccolo).
unsafe fn coalesce() {
    unsafe {
        let head = ptr::addr_of_mut!(FREE_HEAD);
        loop {
            let mut cur = *head;
            let mut merged = false;
            while !cur.is_null() {
                // Prossimo blocco libero fisicamente adiacente a `cur`?
                let next_phys = (cur as usize + (*cur).size) as *mut Header;
                let mut scan = *head;
                let mut found = false;
                while !scan.is_null() {
                    if scan as usize == next_phys as usize {
                        found = true;
                        break;
                    }
                    scan = (*scan).next;
                }
                if found {
                    // Assorbi next_phys in cur e rimuovilo dalla lista.
                    (*cur).size += (*next_phys).size;
                    let mut p2: *mut Header = ptr::null_mut();
                    let mut c2 = *head;
                    while !c2.is_null() {
                        if c2 as usize == next_phys as usize {
                            if p2.is_null() {
                                *head = (*c2).next;
                            } else {
                                (*p2).next = (*c2).next;
                            }
                            break;
                        }
                        p2 = c2;
                        c2 = (*c2).next;
                    }
                    merged = true;
                    // Ricomincia da capo (cur e' cresciuto).
                    break;
                }
                cur = (*cur).next;
            }
            if !merged {
                break;
            }
        }
    }
}

/// `alloc`: primo tentativo first-fit; se fallisce estende l'heap via `sbrk`
/// e ritenta una volta. Ritorna `null` se non c'e' memoria.
unsafe fn heap_alloc(layout: Layout) -> *mut u8 {
    debug_assert!(layout.align() <= 8, "heap: align > 8 non supportato");
    if layout.align() > 8 || layout.size() == 0 {
        return ptr::null_mut();
    }
    let need = align8(layout.size()).max(1);
    if let Some(p) = unsafe { first_fit(need) } {
        return p;
    }
    // Crescita: chiedi al kernel un'estensione (arrotondata alla pagina).
    let want = need + HEADER;
    let grow = want.saturating_add(0xFFF) & !0xFFF;
    let grow = grow.max(0x1000);
    if let Ok(old) = crate::sbrk(grow) {
        unsafe { push_free(old, grow) };
        if let Some(p) = unsafe { first_fit(need) } {
            return p;
        }
    }
    ptr::null_mut()
}

/// `dealloc`: reinserisce il blocco nella free-list (con coalescenza).
unsafe fn heap_free(ptr: *mut u8) {
    unsafe {
        let b = (ptr as usize - HEADER) as *mut Header;
        let size = (*b).size;
        push_free(b as usize, size);
    }
}

/// Allocatore globale del processo: una sola istanza per binario.
pub struct HeapAlloc;

unsafe impl GlobalAlloc for HeapAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe { heap_alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        unsafe { heap_free(ptr) }
    }
}

#[global_allocator]
pub static ALLOCATOR: HeapAlloc = HeapAlloc;
