//! Context switch a basso livello: salva/ripristina lo stato CPU di un processo.
//!
//! Layout: ogni processo ha uno stack kernel dedicato. Quando viene interrotto
//! dal timer, sul suo stack resta il frame della CPU (push hardware) usato dal
//! `iretq` dell'epilogo del timer handler. Il context switch scambia solo i
//! registri callee-saved + RSP: i caller-saved sono gia' salvati sullo stack di
//! ciascun processo dall'ABI quando ha chiamato funzioni.
//!
//! `CpuContext` contiene tutto cio' che va tenuto tra uno switch e l'altro.

use core::arch::naked_asm;

/// Stato CPU di un processo in attesa di essere ripreso.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct CpuContext {
    pub rbx: u64,
    pub rbp: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    /// Puntatore alla zona sullo stack da cui riprendere.
    pub rsp: u64,
}

impl CpuContext {
    pub const ZERO: CpuContext = CpuContext {
        rbx: 0,
        rbp: 0,
        r12: 0,
        r13: 0,
        r14: 0,
        r15: 0,
        rsp: 0,
    };
}

/// Salva lo stato del processo corrente in `cur` e ripristina `next`.
///
/// Nota: questa funzione **non** torna mai nel chiamante originale ma nel
/// processo `next` (che era stato interrotto nel suo propio `switch_to` o
/// parte dal trampoline di avvio). `cur`/`next` devono puntare a
/// `CpuContext` stabili.
///
/// # Safety
/// Richiede `cur`/`next` validi e stack di `next` pronto.
#[unsafe(naked)]
pub unsafe extern "C" fn switch_to(cur: *mut CpuContext, next: *const CpuContext) {
    naked_asm!(
        // ABI: rdi = cur, rsi = next
        "mov [rdi + 0], rbx",
        "mov [rdi + 8], rbp",
        "mov [rdi + 16], r12",
        "mov [rdi + 24], r13",
        "mov [rdi + 32], r14",
        "mov [rdi + 40], r15",
        "mov [rdi + 48], rsp",
        // carica il nuovo contesto
        "mov rsp, [rsi + 48]",
        "mov rbx, [rsi + 0]",
        "mov rbp, [rsi + 8]",
        "mov r12, [rsi + 16]",
        "mov r13, [rsi + 24]",
        "mov r14, [rsi + 32]",
        "mov r15, [rsi + 40]",
        "ret",
    );
}

/// Entry usata dal primo avvio di un processo: esegue `iretq` sul frame CPU
/// fittizio preparato sullo stack, avviando cosi' la funzione con IF=1.
///
/// Deve essere `naked`: qualsiasi prologue (es. push) sposterebbe RSP e
/// l'`iretq` leggerebbe il frame nella posizione sbagliata.
///
/// Nota GS: l'entry syscall non usa `swapgs` (accede a PERCPU rip-relative),
/// quindi non serve normalizzare lo stato GS prima di entrare in ring 3.
#[unsafe(naked)]
unsafe extern "C" fn process_entry_trampoline() -> ! {
    naked_asm!("iretq");
}

/// Preparazione dello stack per un processo mai avviato.
///
/// Crea in cima a `stack_top` un frame CPU fittizio (rip=entry, cs=kernel,
/// rflags=IF, rsp=stack_top, ss=0) preceduto da un indirizzo di ritorno verso
/// il trampoline. Quando il `CpuContext` viene usato da `switch_to`, il `ret`
/// salta al trampoline, che `iretq` nella `entry` con gli interrupt abilitati.
///
/// # Safety
/// `stack_top` deve essere l'indirizzo alto di uno stack kernel riservato
/// valido in memoria (>= 64 byte).
pub unsafe fn new_context(stack_top: u64, entry: u64) -> CpuContext {
    use x86_64::registers::rflags::RFlags;
    use x86_64::structures::gdt::SegmentSelector;
    use x86_64::structures::idt::InterruptStackFrame;
    use x86_64::VirtAddr;

    let sel = crate::gdt::selectors();

    // Frame CPU fittizio a `stack_top - 40` (InterruptStackFrame = 40 byte).
    let frame_addr = stack_top - 40;
    let frame = InterruptStackFrame::new(
        VirtAddr::new(entry),
        sel.code,
        RFlags::from_bits_truncate(0x202), // IF=1
        VirtAddr::new(stack_top),
        SegmentSelector(0),
    );

    // Indirizzo di ritorno del trampoline appena sotto il frame.
    let ret_addr = frame_addr - 8;
    unsafe {
        core::ptr::write(frame_addr as *mut InterruptStackFrame, frame);
        core::ptr::write(ret_addr as *mut u64, process_entry_trampoline as *const () as usize as u64);
    }

    CpuContext {
        rbx: 0,
        rbp: 0,
        r12: 0,
        r13: 0,
        r14: 0,
        r15: 0,
        rsp: ret_addr,
    }
}

/// Preparazione dello stack per un processo **user** mai avviato (Fase 6.2).
///
/// Come `new_context`, ma il frame CPU trasferisce in ring 3: `CS`/`SS` sono i
/// selettori user (DPL 3, RPL 3), `RSP` e' lo stack user (in alto), `RIP` e'
/// l'entry nel segmento di codice user. Il trampoline `iretq` effettua il
/// cambio di privilegio.
///
/// Il `CpuContext` risultante punta (via rsp) al trampoline sul **kernel**
/// stack; quando il processo viene ripreso da `switch_to` l'`iretq` salta in
/// ring 3. Da li' la preemption (timer) usa `TSS.RSP0` per rientrare a ring 0.
///
/// # Safety
/// `kernel_stack_top` deve essere l'indirizzo alto dello stack kernel riservato
/// (>= 64 byte); `entry`/`user_stack_top` devono essere mappati U=1 nello
/// address space del processo.
pub unsafe fn new_context_user(
    kernel_stack_top: u64,
    entry: u64,
    user_stack_top: u64,
) -> CpuContext {
    use x86_64::registers::rflags::RFlags;
    use x86_64::structures::gdt::SegmentSelector;
    use x86_64::structures::idt::InterruptStackFrame;
    use x86_64::VirtAddr;

    let sel = crate::gdt::selectors();
    let rpl3 = 0x3; // Requester Privilege Level = 3

    // Frame CPU di interrupt (5×8 = 40 byte) a ring 0 (kernel stack).
    let frame_addr = kernel_stack_top - 40;
    let frame = InterruptStackFrame::new(
        VirtAddr::new(entry),
        SegmentSelector(sel.user_code.0 | rpl3),
        RFlags::from_bits_truncate(0x202), // IF=1
        VirtAddr::new(user_stack_top),
        SegmentSelector(sel.user_data.0 | rpl3),
    );

    // Indirizzo di ritorno del trampoline appena sotto il frame.
    let ret_addr = frame_addr - 8;
    unsafe {
        core::ptr::write(frame_addr as *mut InterruptStackFrame, frame);
        core::ptr::write(ret_addr as *mut u64, process_entry_trampoline as *const () as usize as u64);
    }

    CpuContext {
        rbx: 0,
        rbp: 0,
        r12: 0,
        r13: 0,
        r14: 0,
        r15: 0,
        rsp: ret_addr,
    }
}
