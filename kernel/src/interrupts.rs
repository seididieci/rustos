//! IDT del kernel: eccezioni CPU sincrone + interrupt hardware (Fase 3).
//!
//! Le entry 0-31 sono eccezioni CPU; le entry 32-47 (0x20-0x2F) sono gli
//! interrupt hardware rimappati dal PIC 8259. Tutti i restanti IRQ non
//! gestiti vengono intercettati da un handler generico.

use spin::Lazy;
use x86_64::structures::idt::{
    InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode,
};

static IDT: Lazy<InterruptDescriptorTable> = Lazy::new(|| {
    let mut idt = InterruptDescriptorTable::new();

    // ── Eccezioni CPU (0-31) ────────────────────────────────────────
    idt.breakpoint.set_handler_fn(breakpoint_handler);
    idt.general_protection_fault.set_handler_fn(gpf_handler);
    unsafe {
        idt.double_fault
            .set_handler_fn(double_fault_handler)
            .set_stack_index(crate::gdt::DOUBLE_FAULT_IST_INDEX);
    }
    idt.page_fault.set_handler_fn(page_fault_handler);

    // ── Interrupt hardware (32-47) ──────────────────────────────────
    idt[0x20].set_handler_fn(timer_handler); // IRQ 0 → PIT
    idt[0x21].set_handler_fn(keyboard_handler); // IRQ 1 → tastiera PS/2

    // IRQ 2-7 del master, 8-15 dello slave: handler generico.
    for i in 0x22..=0x2F {
        idt[i].set_handler_fn(unhandled_irq_handler);
    }

    idt
});

pub fn init() {
    IDT.load();
    crate::serial_println!("[idt ] installata (16 IRQ hardware abilitate)");
}

// ── Eccezioni CPU ───────────────────────────────────────────────────

extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    crate::serial_println!(
        "[int ] #BREAKPOINT @ {:#x} (ritorno all'istruzione successiva)",
        stack_frame.instruction_pointer.as_u64()
    );
}

extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    let addr = x86_64::registers::control::Cr2::read();
    let fault_addr = addr.map(|a| a.as_u64()).unwrap_or(0);
    let pid = crate::syscall::current_id() as usize;

    // Demand-zero dell'heap on-demand (test lazy): una pagina sotto il
    // `heap_brk` del processo non ancora materializzata viene mappata lazy con
    // un frame zero (vale anche per fault supervisor).
    if !error_code.contains(PageFaultErrorCode::PROTECTION_VIOLATION)
        && fault_addr >= crate::vmm_user::USER_HEAP_BASE
    {
        let brk = crate::vmm_user::heap_brk(pid);
        if fault_addr < brk {
            let page = fault_addr & !0xfff;
            if let Some(frame) = crate::phys_mem::alloc() {
                unsafe { core::ptr::write_bytes(crate::addr::phys_to_virt(frame) as *mut u8, 0, 4096); }
                let cr3 = crate::vmm_user::active_cr3();
                unsafe { crate::vmm_user::map_user_region_owned(cr3, page, frame, 1); }
                unsafe { flush_page(page) };
                return;
            }
            crate::serial_println!("[int ] heap demand-zero: OOM @ {:#x}", fault_addr);
            halt();
        }
    }

    crate::serial_println!(
        "[int ] #PAGE FAULT @ {:#x}, err={:?}",
        fault_addr,
        error_code
    );
    let (pnb, pnl) = crate::sched::process_name(pid);
    let pname = core::str::from_utf8(&pnb[..pnl as usize]).unwrap_or("???");
    crate::serial_println!(
        "[int ] rip={:#x} rsp={:#x} pid={} '{}'",
        stack_frame.instruction_pointer.as_u64(),
        stack_frame.stack_pointer.as_u64(),
        pid,
        pname,
    );
    halt();
}

/// Invalida la TLB per una singola pagina (dopo un demand-map).
unsafe fn flush_page(addr: u64) {
    unsafe {
        core::arch::asm!("invlpg [{}]", in(reg) addr, options(nostack, preserves_flags));
    }
}

extern "x86-interrupt" fn gpf_handler(stack_frame: InterruptStackFrame, error_code: u64) {
    crate::serial_println!(
        "[int ] #GENERAL PROTECTION err={} @ {:#x}",
        error_code,
        stack_frame.instruction_pointer.as_u64()
    );
    halt();
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    crate::serial_println!(
        "[int ] #DOUBLE FAULT @ {:#x} (stack IST attivo)",
        stack_frame.instruction_pointer.as_u64()
    );
    panic!("double fault");
}

// ── Interrupt hardware (32-47) ──────────────────────────────────────

extern "x86-interrupt" fn timer_handler(_stack_frame: InterruptStackFrame) {
    // EOI PRIMA dello scheduling: se on_tick fa uno switch e la CPU si sposta
    // in un altro processo, il PIC non deve restare in attesa di EOI con i
    // successivi timer bloccati.
    unsafe { crate::pic::end_of_interrupt(0x20) };
    crate::sched::on_tick();
}

extern "x86-interrupt" fn keyboard_handler(_stack_frame: InterruptStackFrame) {
    // Fase 15: routing puro + notify. Il driver PS/2 vive in userspace
    // (`userkbd`, servizio `Kbd`): legge lui la porta 0x60 al risveglio (ha
    // `io_ranges` dedicati). Il kernel non tocca piu' porte ne' code: risolve
    // l'owner per nome (restart-safe) e gli accoda una notify
    // (bridge interrupt→IPC: un wake senza messaggio non farebbe mai ritorno
    // da `recv()` — vedi `notify_irq`); EOI in ogni caso (mai wedge). Senza
    // driver registrato i tasti vanno persi finche' userkbd non parte.
    if let Some(owner) = crate::channels::lookup(syscall_numbers::Service::Kbd) {
        #[cfg(feature = "sched_debug")]
        crate::serial_println!("[irq1] wake kbd pid={}", owner);
        crate::sched::notify_irq(owner, syscall_numbers::IRQ_NOTIFY_KBD);
    } else {
        #[cfg(feature = "sched_debug")]
        crate::serial_println!("[irq1] Kbd non registrato");
    }
    unsafe { crate::pic::end_of_interrupt(0x21) };
}

extern "x86-interrupt" fn unhandled_irq_handler(_stack_frame: InterruptStackFrame) {
    crate::serial_println!("[int ] IRQ non gestito");
    unsafe { crate::pic::end_of_interrupt(0x20) };
}

fn halt() -> ! {
    use x86_64::instructions::hlt;
    loop {
        hlt();
    }
}
