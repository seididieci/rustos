//! `exec` in-place — sostituzione dell'immagine del processo corrente
//! (Fase 37).
//!
//! Stesso PID/parent/priorita'/canali (fd server-side, code IPC e registrazioni
//! sopravvivono: sono indicizzati per canale, non per immagine); cade TUTTO
//! l'address space e ne viene caricato uno nuovo dai byte del chiamante
//! (copia owned in heap kernel: la sorgente user sparisce col teardown).
//! Stack nuovo con argc=0 in cima (37.1: argv); `image_hash` rimisurato
//! (Strato 2: senza, `peer_info` mentirebbe e la regola same-image 36.5
//! sarebbe bypassabile); porte I/O azzerate (least privilege, come il fork).
//! Il nome display resta quello vecchio in 37.0 (37.1 lo deriva da argv[0]).
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

/// Sostituisce l'immagine del processo corrente con `bytes` (copia owned del
/// chiamante, gia' validata come range user). SCHED lock trattenuto per tutta
/// l'operazione (come `fork_current`); IF=0 in syscall, niente preemption e
/// niente blocking nel mezzo (mai context switch su spazio dimezzato).
pub fn exec_current(bytes: &[u8]) -> Result<(), ()> {
    // 1. Validazione PRIMA di toccare qualunque stato (ELF malformato = -1,
    // processo intatto, come `create_user` prima di allocare).
    let layout = crate::elf::validate(bytes).ok_or(())?;
    let hash = syscall_numbers::image_hash(bytes);

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
    let new_text = unsafe { crate::elf::load(cr3, bytes, &layout) };
    let stack_top = unsafe { crate::vmm_user::setup_user_stack(cr3) };
    // argc=0 in cima (37.1: argv; `rsp % 16 == 8` all'entry, ABI come da CALL).
    unsafe {
        core::ptr::write((stack_top - 8) as *mut u64, 0u64);
    }

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
    // vedi `set_current` in ctx.rs): RIP→entry nuova, RSP→stack nuovo.
    // r11 (rflags user, IF=1) e' preservato; `rax` = 0 (ritorno del handler).
    let entry = crate::elf::entry(&layout);
    unsafe {
        core::ptr::write((top - crate::syscall::SAVED_RCX) as *mut u64, entry);
        core::ptr::write((top - crate::syscall::SAVED_USER_RSP) as *mut u64, stack_top - 8);
    }
    crate::serial_println!(
        "[exec] pid={} nuova immagine entry={:#x} hash={:#x}",
        me, entry, hash
    );
    Ok(())
}
