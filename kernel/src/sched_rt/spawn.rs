// Split from sched_rt.rs (byte-identical move; see facade).
use super::*;
use crate::process::Process;
use core::sync::atomic::Ordering;
use super::ctx::{SCHED, INITIALIZED};
use super::queue::Scheduler;

pub fn init() {
    let mut guard = SCHED.lock();
    *guard = Some(Scheduler::new());
    INITIALIZED.store(true, Ordering::Release);
    crate::serial_println!("[sched_rt] init (preemptive, 32-prio + RR, quantum {} tick)", QUANTUM_TICKS);
}

pub fn spawn(name: &'static str, priority: Priority, entry: crate::process::ProcessFn, parent: Option<usize>, parent_chan: Option<usize>) -> Option<usize> {
    let mut guard = SCHED.lock();
    let sched = guard.as_mut().expect("scheduler non inizializzato");

    let id = sched.alloc_pid()?;
    let process = match Process::create(name, priority, entry, parent, parent_chan, &[]) {
        Some(p) => p,
        None => {
            sched.release_pid(id);
            return None;
        }
    };
    sched.place_process(id, process);
    sched.set_ready(id);
    let p = &sched.processes[id];
    crate::serial_println!(
        "[sched_rt] process '{}' (id {}), {:?} | cr3={:#x} rsp0={:#x}",
        name, id, priority, p.cr3, p.kernel_stack_top
    );
    Some(id)
}

pub unsafe fn create_user(
    name: &'static str,
    priority: Priority,
    elf: &[u8],
    parent: Option<usize>,
    parent_chan: Option<usize>,
    io_ranges: &[(u16, u16)],
    detached: bool,
) -> Option<usize> {
    let mut guard = SCHED.lock();
    let sched = guard.as_mut().expect("scheduler non inizializzato");

    let id = sched.alloc_pid()?;
    let process = match unsafe {
        Process::create_user(name, priority, elf, parent, parent_chan, io_ranges, detached)
    } {
        Some(p) => p,
        None => {
            sched.release_pid(id);
            return None;
        }
    };
    sched.place_process(id, process);
    sched.set_ready(id);
    let p = &sched.processes[id];
    crate::serial_println!(
        "[sched_rt] USER process '{}' (id {}), {:?} | cr3={:#x} rsp0={:#x}",
        name, id, priority, p.cr3, p.kernel_stack_top
    );
    Some(id)
}
