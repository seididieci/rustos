//! Cache settoriale write-through in `userdisk` (Fase 25).
//!
//! Un solo strato di cache, nel driver che possiede i blocchi — mai nel
//! filesystem: due strati cacherebbero gli stessi 512 byte due volte (RAM
//! sprecata) e il client dovrebbe comunque invalidarsi sui metadati FAT.
//! Chiave fisica `(disco, lba_fisica)`: whole-disk e partizioni condividono la
//! stessa entry, quindi il data-plane `DISK_*` e il relay raw `DEV_*` sono
//! coerenti per costruzione. Il protocollo `DISK_*` non cambia: la cache e'
//! un dettaglio interno di `node_read(_multi)` / `node_write(_multi)`.
//!
//! Politica **write-through**: ogni scrittura va prima su disco (stesso PIO +
//! FLUSH di Fase 20, stessa durabilita') e solo a successo aggiorna la cache;
//! a fallimento la entry e' invalidata (mai dati sporchi in cache). Il campo
//! `dirty` e l'enum `Policy` esistono come hook per un futuro write-back
//! configurabile, oggi inutilizzati.
//!
//! Vincoli rispettati (lezioni 24.2): **zero allocazioni heap nel percorso
//! per-op** (array statico, niente `Vec`/`BTreeMap`), `userdisk` e'
//! single-threaded quindi `static mut` basta (stesso pattern dei ring,
//! nessun lock). Gli accessi usano raw pointer (`addr_of!`) perche' l'edition
//! 2024 rifiuta i riferimenti a `static mut`. Dimensione fissa `CACHE_SECTORS`
//! in un punto solo: la cache dinamica con reclaim richiede un contratto
//! kernel↔driver oggi inesistente (sbrk cresce soltanto) ed e' rimandata — i
//! contatori qui raccolti serviranno a dimensionarla.

/// Settori in cache (256 x 512 B = 128 KiB di dati in `.bss`).
pub const CACHE_SECTORS: usize = 256;

/// Politica di scrittura (hook futuro: oggi solo write-through).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// Ogni write va subito su disco + aggiorna/invalida la cache.
    WriteThrough,
    // Futuro: WriteBack { ... } — richiede flush epoch + gestione dirty.
}

/// Politica attiva (single source: cambiare qui per il futuro write-back).
pub const POLICY: Policy = Policy::WriteThrough;

/// Una entry: un settore fisico con bit di uso per il CLOCK.
#[derive(Clone, Copy)]
struct Entry {
    valid: bool,
    /// Indice disco fisico (da `detect`, non l'handle codificato).
    disk: u32,
    /// LBA fisica nel disco (base nodo + lba relativa).
    lba: u64,
    /// Bit di riferimento per il CLOCK (second chance).
    used: bool,
    /// Riservato al futuro write-back (sempre false in write-through).
    dirty: bool,
    data: [u8; 512],
}

const EMPTY: Entry = Entry {
    valid: false,
    disk: 0,
    lba: 0,
    used: false,
    dirty: false,
    data: [0u8; 512],
};

static mut ENTRIES: [Entry; CACHE_SECTORS] = [EMPTY; CACHE_SECTORS];
static mut HAND: usize = 0;

static mut HITS: u64 = 0;
static mut MISSES: u64 = 0;
static mut INSERTS: u64 = 0;
/// Prossima soglia di log (ogni `LOG_EVERY` accessi, per non spammare).
static mut NEXT_LOG_AT: u64 = 2048;
const LOG_EVERY: u64 = 2048;

/// Puntatore raw alla entry `i` (niente riferimenti a `static mut`).
fn entry_ptr(i: usize) -> *mut Entry {
    unsafe { core::ptr::addr_of_mut!(ENTRIES[i]) }
}

/// Cerca l'indice dell'entry (lineare su 256: sub-µs contro ~1.2 ms di PIO).
fn find(disk: usize, lba: u64) -> Option<usize> {
    let d = disk as u32;
    for i in 0..CACHE_SECTORS {
        let e = unsafe { &*entry_ptr(i) };
        if e.valid && !e.dirty && e.disk == d && e.lba == lba {
            return Some(i);
        }
    }
    None
}

/// Hit? Copia il settore in `out` (≥512 B). Conta hit/miss e logga throttled.
pub fn lookup_into(disk: usize, lba: u64, out: &mut [u8]) -> bool {
    let hit = match find(disk, lba) {
        Some(i) => {
            unsafe {
                let e = &mut *entry_ptr(i);
                out[..512].copy_from_slice(&e.data);
                e.used = true;
                HITS += 1;
            }
            true
        }
        None => {
            unsafe {
                MISSES += 1;
            }
            false
        }
    };
    maybe_log();
    hit
}

/// Solo presenza, senza copia ne' statistica (per delimitare i run di miss
/// senza doppio conteggio: hit e miss li registrano `lookup_into` e
/// `note_misses`).
pub fn contains(disk: usize, lba: u64) -> bool {
    find(disk, lba).is_some()
}

/// Registra `n` miss serviti via PIO (percorso multi: il miss non passa da
/// `lookup_into`, che resta l'unico a contare gli hit). Log throttled.
pub fn note_misses(n: u64) {
    unsafe {
        MISSES += n;
    }
    maybe_log();
}

/// Inserisce/aggiorna (write-through: chiamare solo a scrittura stabile).
/// Eviction CLOCK: prima entry con `used == false`, altrimenti azzera i bit
/// e riparte (al massimo 2 giri, bound statico).
pub fn insert_from(disk: usize, lba: u64, data: &[u8]) {
    let d = disk as u32;
    if let Some(i) = find(disk, lba) {
        unsafe {
            let e = &mut *entry_ptr(i);
            e.data.copy_from_slice(&data[..512]);
            e.used = true;
            e.dirty = false;
        }
        return;
    }
    unsafe {
        for _ in 0..2 * CACHE_SECTORS {
            let h = HAND;
            let e = &mut *entry_ptr(h);
            if !e.valid || !e.used {
                e.valid = true;
                e.disk = d;
                e.lba = lba;
                e.used = true;
                e.dirty = false;
                e.data.copy_from_slice(&data[..512]);
                HAND = (h + 1) % CACHE_SECTORS;
                INSERTS += 1;
                return;
            }
            e.used = false;
            HAND = (h + 1) % CACHE_SECTORS;
        }
        // Fallback impossibile in pratica (2 giri bastano sempre): riusa hand.
        let h = HAND;
        let e = &mut *entry_ptr(h);
        e.valid = true;
        e.disk = d;
        e.lba = lba;
        e.used = true;
        e.dirty = false;
        e.data.copy_from_slice(&data[..512]);
        HAND = (h + 1) % CACHE_SECTORS;
        INSERTS += 1;
    }
}

/// Invalida una entry (su errore IO: mai servire cio' che non e' stabile).
pub fn invalidate(disk: usize, lba: u64) {
    if let Some(i) = find(disk, lba) {
        unsafe {
            (*entry_ptr(i)).valid = false;
        }
    }
}

/// Invalida un run contiguo (su errore di un multi-settore).
pub fn invalidate_run(disk: usize, base: u64, n: usize) {
    for j in 0..n {
        invalidate(disk, base + j as u64);
    }
}

/// Contatori (hit, miss, insert) per dimensionare la futura cache dinamica.
pub fn stats() -> (u64, u64, u64) {
    unsafe { (HITS, MISSES, INSERTS) }
}

/// Riga di log throttled ogni `LOG_EVERY` accessi (seriale, mai spam).
fn maybe_log() {
    let (h, m, ins) = stats();
    unsafe {
        if h + m >= NEXT_LOG_AT {
            NEXT_LOG_AT += LOG_EVERY;
            libr::println!("[userdisk] cache hits={} misses={} inserts={}", h, m, ins);
        }
    }
}
