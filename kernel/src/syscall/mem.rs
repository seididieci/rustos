// Split from syscall.rs (byte-identical move; see facade).
use core::ptr::addr_of;
use super::entry::{current_id, PERCPU};
use super::dispatch::apply_ipc;

/// mmap(hint, len, prot, flags): mappa anonima privata nel basso canonico
/// (Fase M0, zero-fill lazy come `sbrk`: VA subito, frame al primo fault).
/// Ritorna la base o -1. M1: `prot` = NONE/R/RW (W solo ed EXEC rifiutati);
/// `flags` 0 (hint consigliato, 0 = scelta kernel) o `MMAP_FIXED`.
pub(super) fn sys_mmap(hint: u64, len: usize, prot: u64, flags: u64) -> i64 {
    use syscall_numbers::{MMAP_FIXED, PROT_NONE, PROT_READ, PROT_WRITE};
    let prot_ok = prot == PROT_NONE || prot == PROT_READ || prot == PROT_READ | PROT_WRITE;
    if !prot_ok {
        return -1; // M1: NONE/R/RW (W solo, EXEC e altri bit rifiutati)
    }
    if flags & !MMAP_FIXED != 0 {
        return -1; // flag sconosciuti (file-backed in M2a, shared in M3)
    }
    let fixed = flags & MMAP_FIXED != 0;
    if fixed && hint == 0 {
        return -1; // FIXED senza hint non ha senso
    }
    let cur = current_id() as usize;
    match crate::vmm_user::vma_map(cur, hint, len as u64, fixed, prot as u8) {
        Some(base) => base as i64,
        None => -1,
    }
}

/// mprotect(addr, len, prot): cambia le protezioni di VMA intere (Fase M1).
/// Stesse regole di `munmap` (copertura esatta, parziali = -1 senza stato) +
/// `prot` validato come `mmap`. A NONE le pagine cadono (smappa+libera) e il
/// riuso rimaterializza zero; RO↔RW flippa il bit W in place. 0 o -1.
pub(super) fn sys_mprotect(addr: u64, len: usize, prot: u64) -> i64 {
    use syscall_numbers::{PROT_NONE, PROT_READ, PROT_WRITE};
    let prot_ok = prot == PROT_NONE || prot == PROT_READ || prot == PROT_READ | PROT_WRITE;
    if !prot_ok {
        return -1;
    }
    let cr3 = unsafe { (*(addr_of!(PERCPU))).current_cr3 };
    if cr3 == 0 {
        return -1; // cr3 non impostata
    }
    let cur = current_id() as usize;
    if crate::vmm_user::vma_protect(cur, cr3, addr, len as u64, prot as u8) {
        0
    } else {
        -1
    }
}

/// munmap(addr, len): smappa VMA intere (Fase M0, niente split). 0 o -1.
pub(super) fn sys_munmap(addr: u64, len: usize) -> i64 {
    let cr3 = unsafe { (*(addr_of!(PERCPU))).current_cr3 };
    if cr3 == 0 {
        return -1; // cr3 non impostata
    }
    let cur = current_id() as usize;
    if crate::vmm_user::vma_unmap(cur, cr3, addr, len as u64) {
        0
    } else {
        -1
    }
}

/// map_physical(phys_addr, virt_addr, count): mappa `count` pagine fisiche
/// a partire da `phys_addr` all'indirizzo virtuale `virt_addr` nello spazio
/// del chiamante. Usato dal console server (VGA), da userfs (ring req/resp
/// di un client, dai phys registrati via `FS_BUF_REG`) e dalla test suite
/// (pagina scratch MAP_TEST_PHYS).
pub(super) fn sys_map_physical(phys_addr: u64, virt_addr: u64, count: usize) -> i64 {
    const PAGE_SIZE: u64 = 0x1000;
    const MAX_PAGES: usize = 256;

    if phys_addr & (PAGE_SIZE - 1) != 0 {
        return -1; // phys_addr non allineato a pagina
    }
    if virt_addr < crate::vmm_user::USER_BASE {
        return -1; // virt_addr fuori spazio user
    }
    if count == 0 || count > MAX_PAGES {
        return -1;
    }

    let cr3 = unsafe { (*(addr_of!(PERCPU))).current_cr3 };
    if cr3 == 0 {
        return -1; // cr3 non impostata
    }
    unsafe {
        crate::vmm_user::map_user_region(cr3, virt_addr, phys_addr, count);
    }
    // La PTE puo' gia' esistere (es. userfs rimappa la finestra FS a ogni
    // client): invalida la TLB perche' il processo continua a girare dopo la
    // syscall e non deve riusare la traduzione vecchia.
    for i in 0..count {
        crate::vmm_user::flush_page(virt_addr + (i as u64) * PAGE_SIZE);
    }
    0
}

/// sys_sbrk(inc): estende (solo crescita) l'heap del processo corrente di `inc`
/// byte, arrotondati alla pagina. NON mappa nulla: riserva solo VA aggiornando
/// il `heap_brk`. Le pagine sotto il break vengono materializzate lazy dal
/// page-fault handler (demand-zero) al primo accesso.
/// Ritorna il vecchio `heap_brk` (inizio della nuova regione) o -1 se
/// l'estensione non e' possibile (overflow / oltre il tetto soft).
pub(super) fn sys_sbrk(inc: u64) -> i64 {
    const PAGE: u64 = 0x1000;
    let cur = current_id() as usize;
    let old = crate::vmm_user::heap_brk(cur);
    if inc == 0 {
        return old as i64;
    }
    // Arrotonda a pagina (saturando: inc enormi falliscono dopo).
    let n = inc.saturating_add(PAGE - 1) & !(PAGE - 1);
    let new = match old.checked_add(n) {
        Some(v) => v,
        None => return -1,
    };
    if new > crate::vmm_user::USER_HEAP_LIMIT {
        return -1;
    }

    crate::vmm_user::set_heap_brk(cur, new);
    old as i64
}

// ── Ring buffer SPSC per-processo (Fase 10.2) ─────────────────────
//
// Ogni processo alloca COPPIE fresche di pagine ring (request + response)
// via `SYS_RING_ALLOC` (Fase 16: multi-coppia, es. userdisk FS+DISK),
// mappate a `USER_FS_BUFFER` (request) e `USER_RESP_RING` (response).
// Il chiamante registra entrambi gli indirizzi fisici presso userfs con
// una IPC `FS_BUF_REG`. Le operazioni FS sono IPC dirette client→userfs
// con trasferimento dati via ring buffer.

/// ring_alloc(): alloca due pagine ring (request + response) per il processo
/// corrente, le mappa a `USER_FS_BUFFER` e `USER_RESP_RING`, e ritorna gli
/// indirizzi fisici via IpcResult (req_phys in rax, resp_phys in rdi). -1 su OOM o errore.
/// Ogni chiamata da' pagine FRESCHE (Fase 16: un processo puo' allocare piu'
/// coppie, es. userdisk FS+DISK). Il mapping e' NON-owned: il free avviene via
/// record a teardown (`free_ring_pages`), mai double-free col walk owned.
pub(super) fn sys_ring_alloc() -> i64 {
    let cur = current_id() as usize;
    let (req_phys, resp_phys) = match crate::vmm_user::alloc_ring_pages(cur) {
        Some(p) => p,
        None => {
            crate::serial_println!("[syscall] ring_alloc: oom");
            return -1;
        }
    };
    let cr3 = unsafe { (*(addr_of!(PERCPU))).current_cr3 };
    if cr3 == 0 {
        return -1;
    }
    unsafe {
        crate::vmm_user::map_user_region(cr3, crate::vmm_user::USER_FS_BUFFER, req_phys, 1);
        crate::vmm_user::map_user_region(cr3, crate::vmm_user::USER_RESP_RING, resp_phys, 1);
    }
    crate::vmm_user::flush_page(crate::vmm_user::USER_FS_BUFFER);
    crate::vmm_user::flush_page(crate::vmm_user::USER_RESP_RING);
    // Restituiamo entrambi gli indirizzi fisici via IpcResult.
    apply_ipc(crate::sched::IpcResult { rax: req_phys as i64, rdi: resp_phys, rsi: 0, rdx: 0, r10: 0 })
}

/// map_in(chan, phys, virt, count): mappa `count` pagine fisiche a partire da
/// `phys` all'indirizzo virtuale `virt` nello spazio del PEER del canale
/// `chan` (ADR-0008). Mapper generico cross-process: usato da userfs per
/// iniettare la response ring del client in un driver remoto (devfs/console).
///
/// Validazione: il peer deve essere un processo user, phys allineata a pagina,
/// virt nello spazio user, count <= 16. Ritorna 0 o -1.
pub(super) fn sys_map_in(chan: usize, phys: u64, virt_addr: u64, count: usize) -> i64 {
    const PAGE_SIZE: u64 = 0x1000;
    const MAX_PAGES: usize = 16;

    if phys & (PAGE_SIZE - 1) != 0 {
        return -1;
    }
    if virt_addr < crate::vmm_user::USER_BASE {
        return -1;
    }
    if count == 0 || count > MAX_PAGES {
        return -1;
    }
    // Il target e' il peer del canale: deve essere un processo user esistente
    // (page table propria). Channel 0 = canale di nascita.
    let me = current_id() as usize;
    let real = if chan == syscall_numbers::CHANNEL_PARENT as usize {
        crate::sched::parent_channel(me)
    } else {
        Some(chan)
    };
    let target_pid = match real.and_then(|c| crate::channels::peer(c, me)) {
        Some(p) => p,
        None => return -1,
    };
    let cr3 = match crate::sched::process_cr3(target_pid) {
        Some(cr3) if cr3 != crate::vmm_user::kernel_cr3() => cr3,
        _ => {
            crate::serial_println!("[syscall] map_in: peer {} non e' un processo user", target_pid);
            return -1;
        }
    };
    unsafe {
        crate::vmm_user::map_user_region(cr3, virt_addr, phys, count);
    }
    for i in 0..count {
        crate::vmm_user::flush_page(virt_addr + (i as u64) * PAGE_SIZE);
    }
    0
}
