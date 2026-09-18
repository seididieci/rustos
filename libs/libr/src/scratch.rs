//! Scratch arena per-op per i server userspace (Fase allocatori, passo C2).
//!
//! Temporanei con lifetime = una richiesta (payload IPC, split di path,
//! liste di nomi) NON vanno sullo heap globale: anche piccoli, ogni
//! alloc+free attraversa la free-list (e prima del passo A pagava coalesce
//! O(n²) a ogni free — il cliff 24.2). Qui: bump-pointer O(1), `reset()` O(1)
//! a fine richiesta, zero free individuali, zero frammentazione possibile.
//!
//! Backing: regione `sbrk` DEDICATA (mai free-list, mai heap globale), lazy
//! alla prima allocazione (chi non la usa non paga nulla), crescita per
//! raddoppio fino a `MAX_CHUNKS` chunk (mai liberati: high-water mark
//! ritenuto, deterministico). OOM vera → `None`, mai panic.
//!
//! Disciplina (per convenzione, come i ring SPSC e i singleton `Cell` dei
//! server): UN `reset()` in testa al loop di ogni server; tutti i borrow
//! muoiono entro la stessa iterazione. I server sono single-threaded: niente
//! lock. Tipi con `Drop` vietati di fatto (`T: Copy` negli slice: l'arena non
//! esegue mai distruttori). Allineamento ≤ 8, stessa regola dell'heap.

use core::mem;

/// Dimensione iniziale del backing (2 pagine; copre il payload massimo
/// userfs di 4096 B + split/list temporanei della stessa richiesta).
const INITIAL: usize = 8192;

/// Chunk massimi (con raddoppio: 8K→16K→…→1M circa; oltre = OOM loud).
const MAX_CHUNKS: usize = 8;

/// Chunk come (base, len). Mai liberati dopo la crescita.
static mut CHUNKS: [(usize, usize); MAX_CHUNKS] = [(0, 0); MAX_CHUNKS];
static mut NCHUNKS: usize = 0;
/// Bump pointer e fine del chunk corrente.
static mut PTR: usize = 0;
static mut END: usize = 0;

#[inline]
fn align_up(v: usize, align: usize) -> usize {
    (v + align - 1) & !(align - 1)
}

/// Assicura `n` byte con allineamento `align` nel chunk corrente, crescendo
/// se serve. Ritorna false solo a OOM vera (chunk esauriti o sbrk fallita).
fn ensure(n: usize, align: usize) -> bool {
    unsafe {
        let aligned = align_up(PTR, align);
        if aligned.saturating_add(n) <= END {
            PTR = aligned;
            return true;
        }
        // Crescita: raddoppio dell'ultimo chunk (o INITIAL), almeno `n`.
        let last_len = if NCHUNKS == 0 { 0 } else { CHUNKS[NCHUNKS - 1].1 };
        let mut grow = last_len.checked_mul(2).unwrap_or(usize::MAX).max(INITIAL);
        // Spazio per l'allineamento del bump iniziale del nuovo chunk.
        grow = grow.max(n.saturating_add(align));
        if NCHUNKS >= MAX_CHUNKS {
            return false;
        }
        let base = match crate::sbrk(grow) {
            Ok(b) => b,
            Err(_) => return false,
        };
        CHUNKS[NCHUNKS] = (base, grow);
        NCHUNKS += 1;
        // Base da sbrk e' page-aligned (kernel arrotonda): gia' allineata.
        let aligned = align_up(base, align);
        PTR = aligned;
        END = base.saturating_add(grow);
        aligned.saturating_add(n) <= END
    }
}

/// Rewind a inizio backing (tiene i chunk: high-water ritenuto). Chiamare in
/// testa al loop del server, MAI con borrow vivi (stessa iterazione).
pub fn reset() {
    unsafe {
        if NCHUNKS > 0 {
            PTR = CHUNKS[0].0;
        }
    }
}

/// Bumppa `n` byte (align 1). `None` solo a OOM vera.
pub fn alloc_bytes(n: usize) -> Option<&'static mut [u8]> {
    if n == 0 {
        return Some(&mut []);
    }
    if !ensure(n, 1) {
        return None;
    }
    unsafe {
        let p = PTR as *mut u8;
        PTR += n;
        Some(core::slice::from_raw_parts_mut(p, n))
    }
}

/// Copia `s` dentro e ritorna `&mut str` (mai heap). `None` a OOM.
pub fn alloc_str(s: &str) -> Option<&'static mut str> {
    let dst = alloc_bytes(s.len())?;
    dst.copy_from_slice(s.as_bytes());
    core::str::from_utf8_mut(dst).ok()
}

/// Bumppa spazio per `len` elementi `T: Copy` (mai Drop: l'arena non
/// distrugge). Allineato a `align_of::<T>()` (≤ 8, regola dell'heap).
/// `None` a OOM o align oltre 8.
///
/// Il borrow restituito ha lifetime `'s` SCELTA DAL CHIAMANTE (non `'static`):
/// il backing e' statico e la coercizione `'static → 's` e' sempre valida, ma
/// `T` puo' contenere borrow piu' corti (es. `&'a str` dai mount) — con
/// `&'static mut [T]` il compilatore esigerebbe `T: 'static`. Il vincolo
/// `T: 's` rende il tutto sound: il borrow non sopravvive ai suoi contenuti.
/// Resta la disciplina reset (il borrow non deve sopravvivere al `reset()`).
pub fn alloc_slice<'s, T: Copy + 's>(len: usize) -> Option<&'s mut [T]> {
    if mem::size_of::<T>() == 0 {
        // ZST: nessun byte da consumare, dangling e' sound per ZST.
        let p = core::ptr::NonNull::<T>::dangling().as_ptr();
        return Some(unsafe { core::slice::from_raw_parts_mut(p, len) });
    }
    if len == 0 {
        return Some(&mut []);
    }
    let size = len.checked_mul(mem::size_of::<T>())?;
    let align = mem::align_of::<T>();
    if align > 8 {
        return None;
    }
    if !ensure(size, align) {
        return None;
    }
    unsafe {
        let p = PTR as *mut T;
        PTR += size;
        Some(core::slice::from_raw_parts_mut(p, len))
    }
}
