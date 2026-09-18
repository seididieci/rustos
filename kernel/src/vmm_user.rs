//! Address space per-processo (separazione minima, Fase 6.1).
//!
//! Strategia A: ogni processo user ha un proprio PML4 che **condivide la
//! mappa kernel** (copia dei puntatori PML4 → PDPT/PD kernel, entry con
//! U=0 → non accessibili da ring 3) e mappa una regione user dedicata nella
//! fascia alta (`USER_BASE`), con i propri livelli e PTE `USER_ACCESSIBLE`.
//!
//! Il kernel e' higher-half (ADR-0020, Fase 27): PML4[0] = 0 a runtime, il
//! basso canonico ospita le mappe utente (Fase 28). Dettagli in
//! `docs/04-memory.md`.

mod layout;
mod heap_brk;
mod vma;
mod shm;
mod rings;
mod paging;
mod teardown;

pub use layout::{USER_BASE, USER_CODE, USER_FS_BUFFER, USER_RESP_RING, USER_HEAP_BASE, USER_HEAP_LIMIT, MMAP_BASE, MMAP_END};
// Compat: erano `pub` prima dello split (nessun uso interno attuale).
#[allow(unused_imports)]
pub use layout::{USER_STACK_TOP, USER_STACK_FRAMES, USER_STACK_GUARD};
pub use heap_brk::{heap_brk, set_heap_brk};
pub use vma::{vma_lookup, vma_map, vma_unmap, vma_protect, is_user_range};
pub use shm::{shm_create, shm_region, shm_ref};
pub use rings::alloc_ring_pages;
pub use paging::{active_cr3, flush_page, init, kernel_cr3, new_address_space, map_user_region, map_user_region_owned, map_user_region_owned_ro, map_user_region_shared, map_user_leaf, map_user_leaf_shared, setup_user_stack};
pub use teardown::teardown_user_space;
