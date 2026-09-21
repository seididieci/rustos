//! `exec` in-place — sostituzione dell'immagine del processo corrente
//! (Fase 37).
//!
//! Stesso PID/parent/priorita'/canali (fd server-side, code IPC e registrazioni
//! sopravvivono: sono indicizzati per canale, non per immagine); cade TUTTO
//! l'address space e ne viene caricato uno nuovo dai byte del chiamante
//! (copia owned in heap kernel: la sorgente user sparisce col teardown).
//! Stack nuovo con argv stile Linux (37.1; argc=0 senza args); `image_hash`
//! rimisurato (Strato 2: senza, `peer_info` mentirebbe e la regola same-image
//! 36.5 sarebbe bypassabile); porte I/O azzerate (least privilege, come il
//! fork). Il nome display resta quello vecchio in 37.0 (37.1 lo deriva da
//! argv[0] in shell, mai nel kernel).
//!
//! Ritorna `Ok(())` e NON ritorna al chiamante: il frame syscall salvato viene
//! riscritto (RIP→entry nuova, RSP→stack nuovo) e `sysretq` atterra nella nuova
//! immagine con `rax = 0`. `Err(())` = validazione fallita, processo intatto
//! (mai toccato nulla prima della validazione).
//!
//! OOM a load (come `create_user`, pre-esistente): `map_private` va in panic —
//! stessa proprieta' dello spawn, documentata per ADR-0028 (mai introdotto un
//! nuovo modo di fallire a meta').

use super::ctx::SCHED;

/// Pagina stack: lo stack user e' `USER_STACK_FRAMES` frame (16 KiB).
const STACK_BYTES: u64 = 4 * 0x1000;

/// Argomenti parsati dal blocco `[argc:8][payload]` (37.1.2): `strs` = coppie
/// (offset, len) nel payload, senza NUL. Il blocco e' la copia owned del
/// chiamante (heap kernel): sopravvive al teardown dello spazio user.
struct ParsedArgs<'a> {
    argc: u64,
    payload: &'a [u8],
    strs: alloc::vec::Vec<(usize, usize)>,
}

/// Parsifica+valida il blocco args PRIMA di toccare qualunque stato: `None` =
/// processo intatto. Formato: `[argc:8][esattamente argc stringhe
/// NUL-terminate concatenate, niente coda]`; bound `ARGS_MAX` + fit nello
/// stack (stringhe + array + argc + slack allineamento <= 16 KiB) — oltre =
/// rifiuto atomico (mai teardown a meta': il layout dopo non puo' fallire).
fn parse_args(block: &[u8]) -> Option<ParsedArgs<'_>> {
    if block.len() < 8 || block.len() as u64 > syscall_numbers::ARGS_MAX + 8 {
        return None;
    }
    let argc = u64::from_le_bytes(block[..8].try_into().ok()?);
    if argc > 1024 {
        return None;
    }
    let payload = &block[8..];
    let mut strs = alloc::vec::Vec::new();
    let mut off = 0usize;
    for _ in 0..argc {
        let end = payload.get(off..)?.iter().position(|&b| b == 0)?;
        strs.push((off, end));
        off += end + 1;
    }
    if off != payload.len() {
        return None;
    }
    let total = payload.len() as u64 + 8 * (argc + 2) + 8 + 16;
    if total > STACK_BYTES {
        return None;
    }
    Some(ParsedArgs { argc, payload, strs })
}

/// Stende lo stack argv in ordine Linux sotto `stack_top` (37.1.2): stringhe
/// in alto, poi array `argv[]` + NULL + envp NULL, argc in basso; ritorna il
/// nuovo rsp (punta ad argc, `rsp % 16 == 8`). `argc=0` = le sole 3 parole
/// (stesso layout di `setup_user_stack`, che qui viene sovrascritto).
/// Scrittura via VA user: CR3 proprio attivo in exec (mai altrove).
/// Infallibile per costruzione (fit pre-verificato in `parse_args`).
unsafe fn layout_argv(stack_top: u64, argc: u64, payload: &[u8], strs: &[(usize, usize)]) -> u64 {
    // Indirizzi VA delle stringhe (heap kernel, mai stack kernel: l'array
    // statico da 8 KiB rischierebbe l'overflow dei 16 KiB di stack).
    let mut addrs = alloc::vec::Vec::new();
    let mut sp_str = stack_top;
    for &(off, len) in strs.iter() {
        sp_str -= len as u64 + 1;
        unsafe {
            core::ptr::copy_nonoverlapping(
                payload[off..].as_ptr(),
                sp_str as *mut u8,
                len + 1,
            );
        }
        addrs.push(sp_str);
    }
    let arr = (sp_str & !15) - 8 * (argc + 2);
    for (i, &a) in addrs.iter().enumerate() {
        unsafe { core::ptr::write((arr + i as u64 * 8) as *mut u64, a) };
    }
    unsafe {
        core::ptr::write((arr + argc * 8) as *mut u64, 0); // argv NULL
        core::ptr::write((arr + (argc + 1) * 8) as *mut u64, 0); // envp NULL
        let rsp = arr - 8;
        core::ptr::write(rsp as *mut u64, argc);
        rsp
    }
}

/// Sostituisce l'immagine del processo corrente con `bytes` (copia owned del
/// chiamante, gia' validata come range user) e gli argomenti `args_block`
/// (`None` = nessun argv → argc=0; `Some` = blocco `[argc:8][payload]`,
/// copiato in heap kernel come i byte). SCHED lock trattenuto per tutta
/// l'operazione (come `fork_current`); IF=0 in syscall, niente preemption e
/// niente blocking nel mezzo (mai context switch su spazio dimezzato).
pub fn exec_current(bytes: &[u8], args_block: Option<&[u8]>) -> Result<(), ()> {
    // 1. Validazione PRIMA di toccare qualunque stato (ELF malformato o args
    // malformati = -1, processo intatto, come `create_user` prima di allocare).
    let layout = crate::elf::validate(bytes).ok_or(())?;
    let hash = syscall_numbers::image_hash(bytes);
    let parsed = match args_block {
        Some(b) => Some(parse_args(b).ok_or(())?),
        None => None,
    };

    let mut guard = SCHED.lock();
    let sched = guard.as_mut().expect("scheduler non inizializzato");
    let me = sched.current.expect("exec senza processo corrente");

    // Snapshot scalari (il borrow finisce qui).
    let (cr3, top, slot, old_text) = {
        let p = &sched.processes[me];
        // Solo processi user (CR3 propria, mai quella kernel).
        if p.cr3 == crate::vmm_user::kernel_cr3() {
            return Err(());
        }
        (p.cr3, p.kernel_stack_top, p.tss_slot, p.text_id)
    };

    // 2. Svuota la meta' user TENENDO il PML4 (stesso CR3, meta' kernel
    // intatta), poi TLB flush (entry vecchie stale sullo stesso CR3).
    unsafe { crate::vmm_user::exec_clear_user(cr3); }
    unsafe {
        let (frame, flags) = x86_64::registers::control::Cr3::read();
        x86_64::registers::control::Cr3::write(frame, flags);
    }
    // 3. Reset bookkeeping: heap, VMA (+ref shm rilasciati), ring (i frame
    // verranno riallocati al primo handshake lazy), text image vecchia.
    crate::vmm_user::set_heap_brk(me, 0);
    crate::vmm_user::vma_clear(me);
    crate::vmm_user::free_ring_pages(me);
    if old_text != 0 {
        crate::text::release(old_text);
    }

    // 4. Carica la nuova immagine + stack nuovo (stesso percorso dello spawn:
    // condivisione text, W^X per-segmento, NX ovunque tranne il codice).
    // `setup_user_stack` mappa e basta (le 3 parole argc=0 che scrive per lo
    // spawn vengono sovrascritte qui sotto: il layout parte da USER_STACK_TOP).
    let new_text = unsafe { crate::elf::load(cr3, bytes, &layout) };
    let _ = unsafe { crate::vmm_user::setup_user_stack(cr3) };
    let new_rsp = match &parsed {
        Some(p) => unsafe {
            layout_argv(crate::vmm_user::USER_STACK_TOP, p.argc, p.payload, &p.strs)
        },
        None => unsafe { layout_argv(crate::vmm_user::USER_STACK_TOP, 0, &[], &[]) },
    };

    // 5. PCB: nuova identita' (text + hash), stessa persona (pid/parent/prio/
    // canali/req_next/CBS intatti — nessuno li tocca).
    {
        let p = &mut sched.processes[me];
        p.text_id = new_text;
        p.image_hash = hash;
    }

    // 6. TSS: bitmap I/O azzerata (idempotente). Nessuno puo' chiedere porte
    // via exec (solo init via spawn_image): least privilege di default.
    crate::gdt::configure_tss(slot, x86_64::VirtAddr::new(top), &[]);

    // 7. Riscrive il frame syscall salvato sullo stack kernel (`top` == rsp0,
    // vedi `set_current` in ctx.rs): RIP→entry nuova, RSP→stack nuovo (punta
    // ad argc, vedi sopra). r11 (rflags user, IF=1) e' preservato; `rax` = 0.
    let entry = crate::elf::entry(&layout);
    unsafe {
        core::ptr::write((top - crate::syscall::SAVED_RCX) as *mut u64, entry);
        core::ptr::write((top - crate::syscall::SAVED_USER_RSP) as *mut u64, new_rsp);
    }
    crate::serial_println!(
        "[exec] pid={} nuova immagine entry={:#x} hash={:#x}",
        me, entry, hash
    );
    Ok(())
}
