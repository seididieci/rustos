//! usertests — suite di regressione user completa (testland).
//!
//! Orchestratore spawnato da init alla fine del boot (dopo testfs/testfat,
//! prima della shell). Copre: syscall core, heap lazy demand-zero, ramfs,
//! device file (/dev/null, /dev/zero), map_physical (aliasing su pagina
//! scratch kernel), IPC sincrono mono/multi-client (reply_target, fix 9.2.2),
//! devfs remoto concorrente + churn heap (regressione lost-wakeup/overlap) e
//! scheduler (preemption ring-3, priorita').
//!
//! Reporting: riga `[usertests] PASS N/N` (o FAIL) + righe per singolo test.

#![no_std]
#![no_main]

extern crate alloc;
use alloc::vec;
use alloc::vec::Vec;

use libr::println;


mod helpers;
mod t_async;
mod t_basic;
mod t_fs;
mod t_ipc_sched;
mod t_lifecycle;
mod t_mapflap;
mod t_stable;
// ── main ─────────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let my_pid = libr::getpid();
    println!("[usertests] suite up, pid={}", my_pid);

    let mut total = 0u32;
    let mut ok = 0u32;

    helpers::report(&mut total, &mut ok, "t1  getpid", t_basic::t_getpid());
    helpers::report(&mut total, &mut ok, "t2  ticks monotonic", t_basic::t_ticks());
    helpers::report(&mut total, &mut ok, "t3  heap fresh zero", t_basic::t_heap_fresh_zero());
    helpers::report(&mut total, &mut ok, "t4  heap alloc reuse", t_basic::t_heap_reuse());
    helpers::report(&mut total, &mut ok, "t5  spawn + child getpid", t_basic::t_spawn_identity());
    helpers::report(&mut total, &mut ok, "t6  ramfs read hello.txt", t_basic::t_hello());
    helpers::report(&mut total, &mut ok, "t7  ramfs write multi-chunk", t_basic::t_ramfs_write_chunk());
    helpers::report(&mut total, &mut ok, "t8  ramfs mkdir + readdir", t_basic::t_ramfs_mkdir());
    helpers::report(&mut total, &mut ok, "t9  fs error paths", t_basic::t_fs_errors());
    helpers::report(&mut total, &mut ok, "t10 /dev/null", t_basic::t_dev_null());
    helpers::report(&mut total, &mut ok, "t11 /dev/zero", t_basic::t_dev_zero());
    helpers::report(&mut total, &mut ok, "t12 map_physical aliasing", t_basic::t_map_alias());
    helpers::report(&mut total, &mut ok, "t13 IPC single echo", t_ipc_sched::t_ipc_echo());
    helpers::report(&mut total, &mut ok, "t14 IPC multi-client", t_ipc_sched::t_ipc_multiclient());
    helpers::report(&mut total, &mut ok, "t15 devfs conc + heap churn", t_ipc_sched::t_devfs_concurrent_churn());
    helpers::report(&mut total, &mut ok, "t16 sched preempt ring3", t_ipc_sched::t_sched_preempt());
    helpers::report(&mut total, &mut ok, "t17 sched priority", t_ipc_sched::t_sched_priority());
    helpers::report(&mut total, &mut ok, "t18 cbs admission", t_ipc_sched::t_cbs_admission());
    helpers::report(&mut total, &mut ok, "t19 cbs bandwidth", t_ipc_sched::t_cbs_bandwidth());
    helpers::report(&mut total, &mut ok, "t20 fs async 1-in-volo", t_async::t_fs_async());
    helpers::report(&mut total, &mut ok, "t21 ipc async + backpressure", t_async::t_ipc_async());
    helpers::report(&mut total, &mut ok, "t22 lifecycle churn (riuso pid)", t_lifecycle::t_lifecycle_churn());
    helpers::report(&mut total, &mut ok, "t23 kill + exit notify", t_lifecycle::t_kill());
    helpers::report(&mut total, &mut ok, "t24 server death notify", t_lifecycle::t_server_death_notify());
    helpers::report(&mut total, &mut ok, "t25 driver death mount purge", t_lifecycle::t_driver_death_mount());
    helpers::report(&mut total, &mut ok, "t26 client death purge", t_lifecycle::t_client_death_purge());
    helpers::report(&mut total, &mut ok, "t27 devfs kill + init restart", t_lifecycle::t_devfs_restart());
    helpers::report(&mut total, &mut ok, "t28 userfs kill + full recovery", t_lifecycle::t_userfs_restart());
    helpers::report(&mut total, &mut ok, "t29 map flap isolation", t_mapflap::t_mapflap());
    helpers::report(&mut total, &mut ok, "t30 neighbor under flood", t_mapflap::t_neighbor());
    helpers::report(&mut total, &mut ok, "t31 kbd/tty presence", t_mapflap::t_kbd_presence());
    helpers::report(&mut total, &mut ok, "t32 disk kill + init restart", t_stable::t_disk());
    helpers::report(&mut total, &mut ok, "t33 mount/umount espliciti", t_fs::t_mount());
    helpers::report(&mut total, &mut ok, "t35 resolve nome->handle lato driver", t_fs::t_resolve());
    helpers::report(&mut total, &mut ok, "t36 UUID/LABEL + discovery stabile", t_stable::t_stable_id());
    helpers::report(&mut total, &mut ok, "t37 ps_info snapshot processi", t_fs::t_ps());
    helpers::report(&mut total, &mut ok, "t38 stat metadati senza open", t_fs::t_stat());
    helpers::report(&mut total, &mut ok, "t39 servizi da disco (/bin+/test)", t_fs::t_diskboot());
    helpers::report(&mut total, &mut ok, "t40 detach + reparent a init", t_stable::t_detach());
    helpers::report(&mut total, &mut ok, "t41 block_on echo async", t_async::t_task_block_on());
    helpers::report(&mut total, &mut ok, "t42 run 2-task + server died", t_async::t_task_run());
    helpers::report(&mut total, &mut ok, "t43 join annidato 3-task", t_async::t_task_join_nested());
    helpers::report(&mut total, &mut ok, "t44 mmap anonimo basso", t_basic::t_mmap());
    helpers::report(&mut total, &mut ok, "t45 mprotect + fault kill", t_basic::t_mprotect());
    helpers::report(&mut total, &mut ok, "t46 memoria condivisa (shm)", t_basic::t_shm());
    helpers::report(&mut total, &mut ok, "t47 shared text (RX/RO condivisi)", t_basic::t_text());
    helpers::report(&mut total, &mut ok, "t48 COW su shm (shared-read + isolamento)", t_basic::t_cow());
    helpers::report(&mut total, &mut ok, "t49 fork COW (isolamento padre/figlio)", t_lifecycle::t_fork());
    helpers::report(&mut total, &mut ok, "t50 hardening (kill/register/map ostili)", t_stable::t_hardening());
    // t34 per ULTIMO: i drop sono irrevocabili sul canale di usertests.
    helpers::report(&mut total, &mut ok, "t34 diritti per-canale lato server", t_fs::t_rights());

    println!("[usertests] SUMMARY {}/{} PASS", ok, total);
    let _ = libr::send(libr::CHANNEL_PARENT, libr::TEST_DONE, ok as u64, 0); // init: test finito
    if ok == total {
        println!("[usertests] PASS {}/{}", ok, total);
        libr::exit(0);
    } else {
        println!("[usertests] FAIL {}/{}", total - ok, total);
        libr::exit(1);
    }
}

#[panic_handler]
fn panic_handler(_info: &core::panic::PanicInfo) -> ! {
    println!("[usertests] panic");
    libr::exit(1)
}
