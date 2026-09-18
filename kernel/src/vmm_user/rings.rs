// Split from vmm_user.rs (byte-identical move; see facade).
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

/// True se `phys` e' una pagina ring registrata da QUALUNQUE processo (Fase
/// 35, hardening `map_physical`/`map_in`): le ring sono il data-plane FS e
/// vengono mappate/iniettate legittimamente tra client, userfs e driver.
pub fn is_ring_page(phys: u64) -> bool {
    if phys == 0 {
        return false;
    }
    let n = RING_MAX_PROCS * RING_PAIRS_MAX * 2;
    for i in 0..n {
        if unsafe { core::ptr::addr_of!(RING_PHYS[i]).read() } == phys {
            return true;
        }
    }
    false
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
pub(super) fn free_ring_pages(pid: usize) {
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
