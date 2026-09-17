//! Address space per-processo (separazione minima, Fase 6.1).
//!
//! Strategia A: ogni processo user ha un proprio PML4 che **condivide la
//! mappa kernel** (copia dei puntatori PML4 → PDPT/PD kernel, entry con
//! U=0 → non accessibili da ring 3) e mappa una regione user dedicata nella
//! fascia alta (`USER_BASE`), con i propri livelli e PTE `USER_ACCESSIBLE`.
//!
//! Nota: kernel resta identity map (bassa); il higher-half e' rimandato a una
//! fase futura (ADR-0005 / docs/03-memory.md).

use core::sync::atomic::{AtomicU64, Ordering};

pub const USER_BASE: u64 = 0x0000_4000_0000_0000;
const USER_PRESENT_WRITABLE: u64 = 0x4 | 0x3; // U + P + W
/// Bit "owned" software sulla PTE (bit 9 AVL, Fase 14/ADR-0010): la pagina
/// e' DI PROPRIETA' di questo processo (codice copiato, stack user, ring,
/// heap demand-zero) e va liberata al teardown. Le pagine iniettate da altri
/// (`map_physical`/`map_in`: VGA, ring di un client, scratch) NON hanno il
/// bit: il loro owner (il processo che le ha allocate) le libera.
const USER_OWNED: u64 = 0x200;
const PAGE_SIZE: u64 = 0x1000;

/// Indirizzo virtuale del codice user (inizio della regione user).
pub const USER_CODE: u64 = USER_BASE;

/// Indirizzo virtuale della finestra request ring del processo corrente.
pub const USER_FS_BUFFER: u64 = USER_BASE + 0x200_000;

/// Indirizzo virtuale della finestra response ring del processo corrente.
pub const USER_RESP_RING: u64 = USER_BASE + 0x210_000;

/// Top dello stack user (cresce verso il basso, qui sopra il codice).
/// Esteso a 4 MiB per accommodare VGA, FS buffer, e stack.
pub const USER_STACK_TOP: u64 = USER_BASE + 0x400_000;
/// Numero di frame (4 KiB) dello stack user.
pub const USER_STACK_FRAMES: usize = 4;

/// Base dello heap on-demand dei processi user: parte vuota subito sopra lo
/// stack e cresce verso l'alto via `sbrk` (syscall 25). Le pagine sotto il
/// `heap_brk` corrente vengono materializzate lazy dal page-fault handler
/// (demand-zero): nessun frame riservato a priori.
pub const USER_HEAP_BASE: u64 = USER_STACK_TOP;

/// Tetto "soft" dello heap: 512 GiB di VA dentro il primo entry PML4 user
/// (non e' un cap pratico: la memoria fisica viene assegnata solo quando le
/// pagine vengono toccate). Serve solo a evitare overflow patologici.
pub const USER_HEAP_LIMIT: u64 = USER_BASE + 0x20_0000_0000;

/// Numero massimo di processi tracciati per lo heap.
const MAX_PROCS: usize = 128;

/// Ring buffer SPSC per processo (PID → fino a N coppie (req_phys, resp_phys)).
/// Allocate dalla syscall `SYS_RING_ALLOC`, UNA COPPIA FRESCA A OGNI CHIAMATA
/// (Fase 16: userdisk ne alloca due — FS + DISK — e la cache single-pair
/// restituiva le stesse pagine due volte, con cross-talk totale tra i ring).
/// Il teardown libera tutte le coppie registrate: il mapping della syscall
/// (sempre a USER_FS_BUFFER/RESP_RING) e' NON-owned apposta, cosi' il free
/// avviene esattamente una volta via record (mai double-free col walk owned).
const RING_MAX_PROCS: usize = 128;
/// Coppie per PID (Fase 16: 4 basano per FS+DISK+riserva). (0, 0) = slot libero.
const RING_PAIRS_MAX: usize = 4;
/// Coppie (req_ring_phys, resp_ring_phys) per PID. Slot dispari = req, pari = resp.
static mut RING_PHYS: [u64; RING_MAX_PROCS * RING_PAIRS_MAX * 2] =
    [0; RING_MAX_PROCS * RING_PAIRS_MAX * 2];

/// `heap_brk` per processo (PID → indice). 0 = mai cresciuto → USER_HEAP_BASE.
/// Accesso single-core; aggiornato da `sbrk`, letto dal page-fault handler.
static mut HEAP_BRK: [u64; MAX_PROCS] = [0; MAX_PROCS];

/// Ritorna il `heap_brk` corrente del processo `pid` (>= USER_HEAP_BASE).
pub fn heap_brk(pid: usize) -> u64 {
    if pid < MAX_PROCS {
        let v = unsafe { *core::ptr::addr_of!(HEAP_BRK[pid]) };
        if v == 0 { USER_HEAP_BASE } else { v }
    } else {
        USER_HEAP_BASE
    }
}

/// Imposta il `heap_brk` del processo `pid`.
pub fn set_heap_brk(pid: usize, val: u64) {
    if pid < MAX_PROCS {
        unsafe { *core::ptr::addr_of_mut!(HEAP_BRK[pid]) = val; }
    }
}

/// Ritorna il CR3 attivo (del processo correntemente in esecuzione).
pub fn active_cr3() -> u64 {
    read_cr3()
}

/// Verifica che l'intervallo [addr, addr+len) sia interamente nello spazio
/// user legalmente accessibile dal kernel: dal codice (USER_CODE) fino al
/// `heap_brk` corrente del processo (lo heap committato via `sbrk`; le pagine
/// sotto il break vengono materializzate lazy dal page-fault handler anche per
/// fault supervisor). Gli indirizzi oltre il break sono rifiutati.
pub fn is_user_range(addr: u64, len: usize) -> bool {
    let (start, end) = (addr, addr.checked_add(len as u64));
    match end {
        Some(end) => {
            let brk = heap_brk(crate::syscall::current_id() as usize);
            start >= USER_CODE && end <= brk
        }
        None => false,
    }
}

/// Indirizzo fisico del PML4 corrente (lettura CR3).
static ACTIVE_PML4: AtomicU64 = AtomicU64::new(0);

/// Indici dei 4 livelli per un indirizzo virtuale.
fn pml4_index(vaddr: u64) -> usize { ((vaddr >> 39) & 0x1FF) as usize }
fn pdpt_index(vaddr: u64) -> usize { ((vaddr >> 30) & 0x1FF) as usize }
fn pd_index(vaddr: u64) -> usize { ((vaddr >> 21) & 0x1FF) as usize }
fn pt_index(vaddr: u64) -> usize { ((vaddr >> 12) & 0x1FF) as usize }

/// Legge l'attuale CR3 (PML4 del kernel / active).
fn read_cr3() -> u64 {
    let cr3: u64;
    unsafe { core::arch::asm!("mov {}, cr3", out(reg) cr3) };
    cr3 & !0xFFF // clear low flags
}

/// Alloca una coppia FRESCA di pagine ring del processo `pid` (Fase 16: mai
/// cache-hit — ogni chiamata da' pagine nuove). Ritorna `(req_phys, resp_phys)`
/// oppure `None` se mancano frame o slot record. Le pagine vengono zero-fill.
/// Il mapping nello spazio del processo e' a carico del chiamante (syscall).
pub fn alloc_ring_pages(pid: usize) -> Option<(u64, u64)> {
    // Fuori record (irraggiungibile in pratica: MAX_PIDS=32 « 128): alloca
    // senza tracciare. Nota: queste pagine non sarebbero liberate a teardown
    // (mapping non-owned + nessun record); prima lo erano via walk owned.
    // Accettato: il path non si verifica mai, e semplifica il caso comune.
    if pid >= RING_MAX_PROCS {
        let req = crate::phys_mem::alloc()?;
        let resp = crate::phys_mem::alloc()?;
        unsafe {
            core::ptr::write_bytes(crate::addr::phys_to_virt(req) as *mut u8, 0, 4096);
            core::ptr::write_bytes(crate::addr::phys_to_virt(resp) as *mut u8, 0, 4096);
        }
        return Some((req, resp));
    }
    let base = pid * RING_PAIRS_MAX * 2;
    let mut slot = None;
    for i in 0..RING_PAIRS_MAX {
        let req = unsafe { *core::ptr::addr_of!(RING_PHYS[base + i * 2]) };
        let resp = unsafe { *core::ptr::addr_of!(RING_PHYS[base + i * 2 + 1]) };
        if req == 0 && resp == 0 {
            slot = Some(i);
            break;
        }
    }
    let i = slot?;
    let req = crate::phys_mem::alloc()?;
    let resp = crate::phys_mem::alloc()?;
    unsafe {
        core::ptr::write_bytes(crate::addr::phys_to_virt(req) as *mut u8, 0, 4096);
        core::ptr::write_bytes(crate::addr::phys_to_virt(resp) as *mut u8, 0, 4096);
        *core::ptr::addr_of_mut!(RING_PHYS[base + i * 2]) = req;
        *core::ptr::addr_of_mut!(RING_PHYS[base + i * 2 + 1]) = resp;
    }
    Some((req, resp))
}

/// Libera tutte le coppie ring registrate del processo `pid` e azzera il
/// record (teardown: single path col walk owned, che salta i mapping ring
/// perche' NON-owned — vedi `sys_ring_alloc`).
fn free_ring_pages(pid: usize) {
    if pid >= RING_MAX_PROCS {
        return;
    }
    let base = pid * RING_PAIRS_MAX * 2;
    for i in 0..RING_PAIRS_MAX {
        let req = unsafe { *core::ptr::addr_of!(RING_PHYS[base + i * 2]) };
        let resp = unsafe { *core::ptr::addr_of!(RING_PHYS[base + i * 2 + 1]) };
        if req != 0 {
            crate::phys_mem::free(req);
        }
        if resp != 0 {
            crate::phys_mem::free(resp);
        }
        unsafe {
            *core::ptr::addr_of_mut!(RING_PHYS[base + i * 2]) = 0;
            *core::ptr::addr_of_mut!(RING_PHYS[base + i * 2 + 1]) = 0;
        }
    }
}

/// Invalida la TLB per la pagina virtuale `vaddr` (necessario dopo aver
/// sovrascritto una PTE gia' presente: es. la finestra FS che userfs rimappa
/// a ogni client). Single-core: nessun shootdown, basta `invlpg` locale.
pub fn flush_page(vaddr: u64) {
    unsafe {
        core::arch::asm!("invlpg [{}]", in(reg) vaddr, options(nostack, preserves_flags));
    }
}

/// Durante `vmm::init` il CR3 di boot resta quello di boot (0x90000).
/// Inizializza la CR3 base ("kernel_cr3") usata dai processi kernel.
pub fn init() {
    ACTIVE_PML4.store(read_cr3(), Ordering::Relaxed);
    crate::serial_println!("[vm_user] base cr3 = {:#x}", read_cr3());
}

/// CR3 condivisa del kernel (usata dai processi kernel e come base).
pub fn kernel_cr3() -> u64 {
    ACTIVE_PML4.load(Ordering::Relaxed)
}

/// Legge una entry della page table a un dato indirizzo fisico di livello.
/// `table_phys` e' fisico: l'accesso passa dalla direct map (`addr.rs`, H0).
unsafe fn entry_at(table_phys: u64, idx: usize) -> u64 {
    let ptr = (crate::addr::phys_to_virt(table_phys) + (idx as u64) * 8) as *const u64;
    unsafe { (*ptr) & !0xFFF }
}

/// Imposta una entry e ritorna l'indirizzo fisico del livello puntato.
unsafe fn set_entry(table_phys: u64, idx: usize, value: u64) {
    let ptr = (crate::addr::phys_to_virt(table_phys) + (idx as u64) * 8) as *mut u64;
    // Conserva i flag esistenti se la voce e' gia' presente? No: sovrascrive.
    unsafe { core::ptr::write_volatile(ptr, value); }
}

/// Zera un frame (512 entry) appena allocato.
unsafe fn zero_frame(phys: u64) {
    unsafe { core::ptr::write_bytes(crate::addr::phys_to_virt(phys) as *mut u8, 0, 4096); }
}

/// Crea un nuovo address space per un processo user.
///
/// Ritorna l'indirizzo fisico del PML4 (da caricare in CR3), oppure `None`
/// se mancano frame. Il PML4 condivide la mappa kernel (U=0) e riserva la
/// regione user in alta con i propri PDPT/PD/PT (U=1).
pub fn new_address_space() -> Option<u64> {
    let kernel_pml4 = kernel_cr3();

    // PML4 del processo: copia dell'active (condivide la mappa kernel).
    let pm = crate::phys_mem::alloc()?;
    unsafe {
        zero_frame(pm);
        core::ptr::copy_nonoverlapping(
            crate::addr::phys_to_virt(kernel_pml4) as *const u64,
            crate::addr::phys_to_virt(pm) as *mut u64,
            512,
        );
    }

    // Regione user: alloco i 3 livelli sotto USER_BASE.
    let pdp = crate::phys_mem::alloc()?;
    let pd = crate::phys_mem::alloc()?;
    let pt = crate::phys_mem::alloc()?;
    unsafe {
        zero_frame(pdp);
        zero_frame(pd);
        zero_frame(pt);
    }

    // PML4[USER_BASE pml4 idx] → pdp (supervisor, ma serve U per l'accesso
    // user alle pagine sotto: impostiamo U sul PDPT e la catena discendente
    // per far passare i permessi user. Qui è una page table: U qui è irrilevante
    // per l'accesso, conta solo su PD/PT. Mettiamo P|W).
    let pm_idx = pml4_index(USER_BASE);
    let pdp_idx = pdpt_index(USER_BASE);
    let pd_idx = pd_index(USER_BASE);
    let pt_idx = pt_index(USER_BASE);

    unsafe {
        set_entry(pm, pm_idx, pdp | USER_PRESENT_WRITABLE);
        set_entry(pdp, pdp_idx, pd | USER_PRESENT_WRITABLE);
        set_entry(pd, pd_idx, pt | USER_PRESENT_WRITABLE);
    }

    // Nota: USER_BASE cade al confine di PDPT/PD/PT con indici 0, quindi i
    // tre frame bastano; se si espandesse oltre 1 GiB servirebbero altri PT.
    let _ = (pdp_idx, pd_idx, pt_idx);

    Some(pm)
}

/// Mappa `count` frame fisici contigui a partire da `phys` all'indirizzo
/// virtuale `vaddr` nello spazio user del processo `cr3`. Le pagine sono
/// user-accessible (U=1), writable e present. **NON** marca il bit `owned`:
/// questo e' il percorso delle pagine "estranee" iniettate nel processo
/// (syscall `map_physical`/`map_in`), che restano di proprieta' di chi le ha
/// allocate.
///
/// # Safety
/// Richiede `cr3` valido e `vaddr` dentro la regione user del processo.
pub unsafe fn map_user_region(cr3: u64, vaddr: u64, phys: u64, count: usize) {
    unsafe { map_user_region_flags(cr3, vaddr, phys, count, USER_PRESENT_WRITABLE) }
}

/// Come `map_user_region`, ma marca le PTE con il bit `owned`: usato per le
/// pagine di proprieta' del processo (codice copiato, stack user, ring della
/// syscall `ring_alloc`, heap demand-zero). Saranno liberate dal teardown
/// dell'address space (Fase 14).
///
/// # Safety
/// Richiede `cr3` valido e `vaddr` dentro la regione user del processo.
pub unsafe fn map_user_region_owned(cr3: u64, vaddr: u64, phys: u64, count: usize) {
    unsafe { map_user_region_flags(cr3, vaddr, phys, count, USER_PRESENT_WRITABLE | USER_OWNED) }
}

unsafe fn map_user_region_flags(cr3: u64, vaddr: u64, phys: u64, count: usize, flags: u64) {
    let cur = cr3;
    let mut addr = vaddr;

    for _ in 0..count {
        // Livello 1: PML4
        let l1 = unsafe { entry_at(cur, pml4_index(addr)) };
        let pdp = if l1 == 0 {
            let f = crate::phys_mem::alloc().expect("oom page table");
            unsafe { zero_frame(f); }
            unsafe { set_entry(cur, pml4_index(addr), f | USER_PRESENT_WRITABLE); }
            f
        } else { l1 };

        // Livello 2: PDPT
        let l2 = unsafe { entry_at(pdp, pdpt_index(addr)) };
        let pd = if l2 == 0 {
            let f = crate::phys_mem::alloc().expect("oom page table");
            unsafe { zero_frame(f); }
            unsafe { set_entry(pdp, pdpt_index(addr), f | USER_PRESENT_WRITABLE); }
            f
        } else { l2 };

        // Livello 3: PD (pagine 4 KiB, niente large page nello user)
        let l3 = unsafe { entry_at(pd, pd_index(addr)) };
        let pt = if l3 == 0 {
            let f = crate::phys_mem::alloc().expect("oom page table");
            unsafe { zero_frame(f); }
            unsafe { set_entry(pd, pd_index(addr), f | USER_PRESENT_WRITABLE); }
            f
        } else { l3 };

        // Livello 4: PTE → pagina fisica (USER + P + W [+ owned])
        let pp = phys + ((addr - vaddr) / PAGE_SIZE) * PAGE_SIZE;
        unsafe { set_entry(pt, pt_index(addr), pp | flags); }

        addr += PAGE_SIZE;
    }
}

/// Prepara la memoria di un processo user: mappa il codice `code_phys` (per
/// `code_frames` frame) a `USER_CODE`, alloca+mappa lo stack user a
/// `USER_STACK_TOP`. Le due pagine ring (`USER_FS_BUFFER` = request,
/// `USER_RESP_RING` = response) NON sono mappate qui: ogni processo le alloca
/// e le mappa lazy al primo uso via la syscall `SYS_RING_ALLOC` (Fase 10.2,
/// ring SPSC per-processo al posto della vecchia FS buffer page).
/// Ritorna il RSP iniziale (`USER_STACK_TOP`).
///
/// # Safety
/// `cr3` e' un address space creato da `new_address_space`; `code_phys` deve
/// puntare a frame fisici validi contenenti il codice user.
pub unsafe fn setup_user_memory(cr3: u64, code_phys: u64, code_frames: usize) -> u64 {
    unsafe { map_user_region_owned(cr3, USER_CODE, code_phys, code_frames); }

    let stack_base = USER_STACK_TOP - (USER_STACK_FRAMES as u64 * PAGE_SIZE);
    let stack_phys = crate::phys_mem::alloc_contiguous(USER_STACK_FRAMES)
        .expect("oom per lo stack user");
    unsafe { map_user_region_owned(cr3, stack_base, stack_phys, USER_STACK_FRAMES); }

    USER_STACK_TOP
}

// ── Teardown dell'address space (Fase 14, ADR-0010) ────────────────
//
// Quando un processo muore (exit/kill) e viene reclamato, l'address space
// user va distrutto per riusare i frame. Il PML4 di un processo condivide la
// mappa kernel (entry U=0 copiate dal PML4 di boot) e possiede una regione
// user privata (sotto l'indice PML4 di USER_BASE). Il walker:
//   - per ogni entry PML4 PRESENT e DIVERSA da quella del kernel → sottoalbero
//     privato: libera tutte le page-table frames (PDPT/PD/PT) e le PTE foglia
//     marcate `USER_OWNED`;
//   - le PTE foglia NON owned (pagine iniettate: VGA, ring di altri processi,
//     scratch `MAP_TEST_PHYS`) NON vengono liberate: le libera il loro owner.
// Non deve mai girare mentre si usa ancora il `cr3` del processo (solo su
// processi Terminated, mai su `current`).

const PTE_PRESENT: u64 = 0x1;

/// Legge una entry di page table grezza (con i flag).
unsafe fn raw_entry(table_phys: u64, idx: usize) -> u64 {
    unsafe { core::ptr::read_volatile((crate::addr::phys_to_virt(table_phys) + (idx as u64) * 8) as *const u64) }
}

/// Libera le PTE foglia sotto una tabella di livello 3 (PT): solo quelle
/// `owned` (le altre restano al proprietario).
unsafe fn free_pt_leaves(pt_phys: u64) {
    for i in 0..512 {
        let e = unsafe { raw_entry(pt_phys, i) };
        if e & PTE_PRESENT != 0 {
            if e & USER_OWNED != 0 {
                crate::phys_mem::free(e & !0xFFF);
            }
        }
    }
}

/// Libera le tabelle sotto un PD (PD → PT → foglie owned).
unsafe fn free_pd_tree(pd_phys: u64) {
    for i in 0..512 {
        let e = unsafe { raw_entry(pd_phys, i) };
        if e & PTE_PRESENT != 0 {
            let pt = e & !0xFFF;
            unsafe { free_pt_leaves(pt) };
            crate::phys_mem::free(pt);
        }
    }
}

/// Libera le tabelle sotto un PDPT (PDPT → PD → PT → foglie owned).
unsafe fn free_pdp_tree(pdp_phys: u64) {
    for i in 0..512 {
        let e = unsafe { raw_entry(pdp_phys, i) };
        if e & PTE_PRESENT != 0 {
            let pd = e & !0xFFF;
            unsafe { free_pd_tree(pd) };
            crate::phys_mem::free(pd);
        }
    }
}

/// Distrugge l'address space user del processo `pid`: libera i frame delle
/// page table private e le pagine `owned` (codice/stack/heap/ring), e azzera
/// le strutture bookkeeping per-processo (`HEAP_BRK`, `RING_PHYS`) cosi' un
/// eventuale riuso del pid parte pulito.
///
/// # Safety
/// `cr3` deve essere l'address space di un processo Terminated che non verra'
/// piu' schedulato.
pub unsafe fn teardown_user_space(cr3: u64, pid: usize) {
    let kernel_pml4 = kernel_cr3();
    unsafe {
        for i in 0..512 {
            let e = raw_entry(cr3, i);
            if e & PTE_PRESENT == 0 {
                continue;
            }
            let tbl = e & !0xFFF;
            // Entry condivise col kernel (mappa U=0): NON sono del processo.
            let k = raw_entry(kernel_pml4, i) & !0xFFF;
            if tbl == k {
                continue;
            }
            free_pdp_tree(tbl);
            crate::phys_mem::free(tbl);
        }
        crate::phys_mem::free(cr3);
    }
    if pid < MAX_PROCS {
        unsafe { *core::ptr::addr_of_mut!(HEAP_BRK[pid]) = 0; }
    }
    // Ring: free via record (le PTE ring sono NON-owned, il walk le salta).
    free_ring_pages(pid);
}
