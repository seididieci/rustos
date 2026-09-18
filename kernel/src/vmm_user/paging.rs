pub(super) const PTE_PRESENT: u64 = 0x1;

// Split from vmm_user.rs (byte-identical move; see facade).
use core::sync::atomic::{AtomicU64, Ordering};
use super::layout::{USER_BASE, USER_PRESENT_WRITABLE, PAGE_SIZE, USER_CODE, USER_STACK_TOP, USER_STACK_FRAMES};

/// Ritorna il CR3 attivo (del processo correntemente in esecuzione).
pub fn active_cr3() -> u64 {
    read_cr3()
}

/// Indirizzo fisico del PML4 corrente (lettura CR3).
static ACTIVE_PML4: AtomicU64 = AtomicU64::new(0);

/// Indici dei 4 livelli per un indirizzo virtuale.
pub(super) fn pml4_index(vaddr: u64) -> usize { ((vaddr >> 39) & 0x1FF) as usize }
pub(super) fn pdpt_index(vaddr: u64) -> usize { ((vaddr >> 30) & 0x1FF) as usize }
pub(super) fn pd_index(vaddr: u64) -> usize { ((vaddr >> 21) & 0x1FF) as usize }
pub(super) fn pt_index(vaddr: u64) -> usize { ((vaddr >> 12) & 0x1FF) as usize }

/// Legge l'attuale CR3 (PML4 del kernel / active).
fn read_cr3() -> u64 {
    let cr3: u64;
    unsafe { core::arch::asm!("mov {}, cr3", out(reg) cr3) };
    cr3 & !0xFFF // clear low flags
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
pub(super) unsafe fn entry_at(table_phys: u64, idx: usize) -> u64 {
    let ptr = (crate::addr::phys_to_virt(table_phys) + (idx as u64) * 8) as *const u64;
    unsafe { (*ptr) & !0xFFF }
}

/// Imposta una entry e ritorna l'indirizzo fisico del livello puntato.
pub(super) unsafe fn set_entry(table_phys: u64, idx: usize, value: u64) {
    let ptr = (crate::addr::phys_to_virt(table_phys) + (idx as u64) * 8) as *mut u64;
    // Conserva i flag esistenti se la voce e' gia' presente? No: sovrascrive.
    unsafe { core::ptr::write_volatile(ptr, value); }
}

/// Zera un frame (512 entry) appena allocato.
pub(super) unsafe fn zero_frame(phys: u64) {
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
/// user-accessible (U=1), writable, NON eseguibili (NX, M1) e present.
/// **NON** marca il bit `owned`:
/// questo e' il percorso delle pagine "estranee" iniettate nel processo
/// (syscall `map_physical`/`map_in`), che restano di proprieta' di chi le ha
/// allocate.
///
/// # Safety
/// Richiede `cr3` valido e `vaddr` dentro la regione user del processo.
pub unsafe fn map_user_region(cr3: u64, vaddr: u64, phys: u64, count: usize) {
    unsafe { map_user_region_flags(cr3, vaddr, phys, count, super::layout::USER_LEAF_RW) }
}

/// Come `map_user_region`, ma marca le PTE con il bit `owned`: usato per le
/// pagine di proprieta' del processo (codice copiato, stack user, ring della
/// syscall `ring_alloc`, heap demand-zero). Saranno liberate dal teardown
/// dell'address space (Fase 14). RW + NX (M1; il codice usa `..._exec`).
///
/// # Safety
/// Richiede `cr3` valido e `vaddr` dentro la regione user del processo.
pub unsafe fn map_user_region_owned(cr3: u64, vaddr: u64, phys: u64, count: usize) {
    unsafe { map_user_region_flags(cr3, vaddr, phys, count, super::layout::USER_LEAF_RW | super::layout::USER_OWNED) }
}

/// Come `map_user_region_owned`, ma read-only (M1): per le pagine di VMA
/// PROT_READ materializzate lazy dal fault handler. Scrittura → #PF con
/// protection-violation → kill (mai corruzione silenziosa).
///
/// # Safety
/// Come `map_user_region_owned`.
pub unsafe fn map_user_region_owned_ro(cr3: u64, vaddr: u64, phys: u64, count: usize) {
    unsafe { map_user_region_flags(cr3, vaddr, phys, count, super::layout::USER_LEAF_RO | super::layout::USER_OWNED) }
}

/// Mapping del binario user (M1): il binario e' FLAT (codice + .rodata +
/// .data + .bss in un'unica regione contigua copiata dall'embed): non
/// conoscendo il confine codice/dati serve ancora RWX (writable+executable).
/// NX e' comunque enforced su heap, stack, mmap e pagine iniettate: il W^X
/// del binario richiede i confini di sezione all'embed-time (M1b).
///
/// # Safety
/// Come `map_user_region_owned`.
pub unsafe fn map_user_region_owned_binary(cr3: u64, vaddr: u64, phys: u64, count: usize) {
    unsafe { map_user_region_flags(cr3, vaddr, phys, count, super::layout::USER_PRESENT_WRITABLE | super::layout::USER_OWNED) }
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

        // Livello 4: PTE → pagina fisica (flag dal chiamante: RW/RO/RX + NX
        // tranne il codice; owned per le pagine di proprieta').
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
    // M1: il binario flat resta RWX (vedi `map_user_region_owned_binary`).
    unsafe { map_user_region_owned_binary(cr3, USER_CODE, code_phys, code_frames); }

    let stack_base = USER_STACK_TOP - (USER_STACK_FRAMES as u64 * PAGE_SIZE);
    let stack_phys = crate::phys_mem::alloc_contiguous(USER_STACK_FRAMES)
        .expect("oom per lo stack user");
    unsafe { map_user_region_owned(cr3, stack_base, stack_phys, USER_STACK_FRAMES); }

    USER_STACK_TOP
}
