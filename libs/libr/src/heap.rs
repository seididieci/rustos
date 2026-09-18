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
//! La lista e' ORDINATA per indirizzo (`push_free` inserisce al posto giusto
//! e fonde solo coi vicini): free O(n), mai O(n²) — il cliff 24.2 (coalesce
//! totale a ogni free) e' eliminato senza cambiare semantica di allocazione.

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

/// Aggiunge un blocco (indirizzo `addr`, dimensione `len`) alla free-list
/// MANTENENDOLA ORDINATA per indirizzo, e fonde solo coi vicini fisici
/// (predecessore/successore). La lista resta sempre totalmente coalescente —
/// stesso invariante di prima, ma free O(n) invece di O(n²): la vecchia
/// `coalesce()` riscansionava tutto a OGNI free (il cliff 24.2: +1 blocco a op
/// FAT → O(n²) su tutte le op successive). Nessun cambio di semantica per
/// `first_fit` (che resta first-fit O(n) sulla lista ordinata).
unsafe fn push_free(addr: usize, len: usize) {
    unsafe {
        // Trova il punto di inserzione (prev < addr < cur).
        let head_slot = ptr::addr_of_mut!(FREE_HEAD);
        let mut prev: *mut Header = ptr::null_mut();
        let mut cur = *head_slot;
        while !cur.is_null() && (cur as usize) < addr {
            prev = cur;
            cur = (*cur).next;
        }
        // Inserisci tra prev e cur.
        let b = addr as *mut Header;
        (*b).size = len;
        (*b).next = cur;
        if prev.is_null() {
            *head_slot = b;
        } else {
            (*prev).next = b;
        }
        // Fondi col predecessore se fisicamente adiacente.
        let mut node = b;
        if !prev.is_null() && (prev as usize) + (*prev).size == addr {
            (*prev).size += len;
            (*prev).next = cur;
            node = prev;
        }
        // Fondi col successore se fisicamente adiacente.
        let node_end = (node as usize) + (*node).size;
        if !cur.is_null() && node_end == cur as usize {
            (*node).size += (*cur).size;
            (*node).next = (*cur).next;
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

/// Diagnostica heap (introdotta per 24.2, mantenuta): (blocchi liberi, byte
/// liberi) nella free-list.
pub fn heap_stats() -> (usize, usize) {
    unsafe {
        let mut n = 0usize;
        let mut bytes = 0usize;
        let mut cur = FREE_HEAD;
        while !cur.is_null() {
            n += 1;
            bytes += (*cur).size;
            cur = (*cur).next;
            if n > 1_000_000 {
                break; // lista corrotta: non impiccare il chiamante
            }
        }
        (n, bytes)
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
