// Split from sched_rt.rs (byte-identical move; see facade).
use core::sync::atomic::AtomicBool;
use spin::Mutex;
use crate::context::CpuContext;
use super::queue::Scheduler;

pub(super) static INITIALIZED: AtomicBool = AtomicBool::new(false);
pub(super) static SCHED: Mutex<Option<Scheduler>> = Mutex::new(None);



/// Contesto del "processo" di boot (main). Non e' un vero PCB: serve solo a
/// salvare lo stato di main al primo switch, quando `current` e' ancora None.
/// Main non viene mai rischedulato (resta in `hlt` nel suo loop).
static mut BOOT_CONTEXT: CpuContext = CpuContext::ZERO;

pub(super) fn switch_to(
    prev: Option<usize>,
    next: usize,
    mut guard: spin::MutexGuard<Option<Scheduler>>,
) {
    {
        let sched = guard.as_mut().expect("scheduler non inizializzato");
        sched.current = Some(next);
        sched.ticks_current = 0;
    }

    let (cur_ptr, next_ptr): (*mut CpuContext, *mut CpuContext) = {
        let sched = guard.as_mut().expect("scheduler non inizializzato");
        let base = sched.processes.as_mut_ptr();
        let cur = match prev {
            Some(pi) => unsafe { &mut (*base.add(pi)).saved as *mut CpuContext },
            None => core::ptr::addr_of_mut!(BOOT_CONTEXT),
        };
        let next = unsafe { &mut (*base.add(next)).saved as *mut CpuContext };
        (cur, next)
    };

    let (next_cr3, next_kstack_top, next_tss_sel) = unsafe {
        let sched = guard.as_mut().expect("scheduler non inizializzato");
        let n = &*sched.processes.as_ptr().add(next);
        (n.cr3, n.kernel_stack_top, n.tss_sel)
    };

    crate::syscall::set_current(next, next_kstack_top, next_cr3);

    drop(guard);

    crate::gdt::load_process_tss(next_tss_sel);
    unsafe {
        core::arch::asm!("mov cr3, {}", in(reg) next_cr3, options(nostack, preserves_flags));
    }

    unsafe { crate::context::switch_to(cur_ptr, next_ptr) };
}
