//! userbench — micro-benchmark throughput client→block (Fase P0).
//!
//! Misura il percorso dati END-TO-END con il TSC (calibrato sul PIT), senza
//! cache che nascondano il collo di bottiglia (non ne esistono ancora: ogni
//! op attraversa IPC + userfs + userdisk + PIO). Piattaforma di riferimento:
//! KVM (`scripts/bench.sh`, N run con media: i tempi TCG non sono reali).
//!
//! Op misurate (costo crescente del percorso):
//!   b1 zero_1B .............. read 1 B da /dev/zero (solo IPC, niente disco)
//!   b2 sda_512B_seq ......... 200 read sequenziali da /dev/sda (IPC + PIO)
//!   b3 fat_small_orc ........ open+read+close di /fat/HELLO.TXT (find + IPC + PIO)
//!   b4 ramfs_4K_write/read .. write/read 4 KiB su ramfs (FS+IPC, niente disco)
//!   b5 fat_4K_oow ........... open+overwrite+close 4 KiB su /fat (write+FLUSH)
//!
//! Outputegrepabile: righe `[bench] <nome> iters=<n> cyc_op=<c> kb_s=<k>`
//! (c = cicli TSC per op, k = KiB/s). Fine: TEST_DONE a init (0x7E) + exit.

#![no_std]
#![no_main]

use libr;
use libr::{println, O_CREAT};

/// Esegue `warm` iterazioni di riscaldamento poi `iters` misurate, stampa la
/// riga `[bench]`. `bytes` = byte utili per iter (per i KiB/s).
fn run(name: &str, iters: u64, bytes: u64, hz: u64, mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..5 {
        if !f() {
            println!("[bench] {}: FAIL (warmup)", name);
            return false;
        }
    }
    let t0 = libr::rdtsc();
    let mut max_cyc = 0u64;
    for _ in 0..iters {
        let s = libr::rdtsc();
        if !f() {
            println!("[bench] {}: FAIL (iter)", name);
            return false;
        }
        let dt = libr::rdtsc().wrapping_sub(s);
        if dt > max_cyc {
            max_cyc = dt;
        }
    }
    let total = libr::rdtsc().wrapping_sub(t0);
    if total == 0 {
        println!("[bench] {}: FAIL (tsc fermo)", name);
        return false;
    }
    let cyc_op = total / iters;
    let kb_s = bytes * iters * hz / total / 1024;
    println!(
        "[bench] {} iters={} cyc_op={} max_cyc={} kb_s={}",
        name, iters, cyc_op, max_cyc, kb_s
    );
    true
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    println!("[bench] starting, pid={}", libr::getpid());
    let hz = libr::tsc_calibrate(20);
    println!("[bench] tsc_hz={}", hz);
    let mut ok = hz != 0;

    // b1: solo IPC — 1 B da /dev/zero (/dev/null da' EOF=0 per semantica
    // Unix: per misurare il round-trip IPC serve un device che risponde).
    if ok {
        let fd = libr::open("/dev/zero", 0);
        if fd < 0 {
            println!("[bench] zero_1B: FAIL (open fd={})", fd);
            ok = false;
        } else {
            let mut one = [0u8; 1];
            ok &= run("zero_1B", 2000, 1, hz, || libr::read_fs(fd, &mut one, 1) == 1);
            let _ = libr::close(fd);
        }
    }

    // b2: catena completa — 200 settori sequenziali raw da /dev/sda.
    if ok {
        let fd = libr::open("/dev/sda", 0);
        if fd < 0 {
            println!("[bench] sda_512B_seq: FAIL (open fd={})", fd);
            ok = false;
        } else {
            let mut sec = [0u8; 512];
            ok &= run("sda_512B_seq", 200, 512, hz, || libr::read_fs(fd, &mut sec, 512) == 512);
            let _ = libr::close(fd);
        }
    }

    // b3: file piccolo su FAT — open+read+close (find + IPC + PIO).
    if ok {
        let mut hello = [0u8; 32];
        ok &= run("fat_small_orc", 500, 25, hz, || {
            let fd = libr::open("/fat/HELLO.TXT", 0);
            if fd < 0 {
                return false;
            }
            let n = libr::read_fs(fd, &mut hello, 25);
            let _ = libr::close(fd);
            n == 25
        });
    }

    // b4: ramfs 4 KiB — stack FS+IPC senza disco (write poi read).
    if ok {
        let fd = libr::open("/BENCH.TMP", O_CREAT);
        if fd < 0 {
            println!("[bench] ramfs_4K: FAIL (create fd={})", fd);
            ok = false;
        } else {
            let wbuf = [0xA5u8; 4096];
            ok &= run("ramfs_4K_write", 100, 4096, hz, || libr::write_fs(fd, &wbuf, 4096) == 4096);
            let _ = libr::close(fd);
        }
    }
    if ok {
        let fd = libr::open("/BENCH.TMP", 0);
        if fd < 0 {
            println!("[bench] ramfs_4K_read: FAIL (open fd={})", fd);
            ok = false;
        } else {
            let mut rbuf = [0u8; 4096];
            ok &= run("ramfs_4K_read", 100, 4096, hz, || libr::read_fs(fd, &mut rbuf, 4096) == 4096);
            let _ = libr::close(fd);
            if libr::remove("/BENCH.TMP") != 0 {
                println!("[bench] ramfs cleanup: FAIL (remove)");
                ok = false;
            }
        }
    }

    // b5: overwrite 4 KiB su FAT — open+write+close (PIO + FLUSH per settore).
    // Il file resta nell'immagine (rigenerata a ogni run.sh): mai fixture altrui.
    if ok {
        let fd = libr::open("/fat/BENCH.TMP", O_CREAT);
        if fd < 0 {
            println!("[bench] fat_4K_oow: FAIL (create fd={})", fd);
            ok = false;
        } else {
            let _ = libr::close(fd);
            let wbuf = [0x5Au8; 4096];
            ok &= run("fat_4K_oow", 50, 4096, hz, || {
                let fd = libr::open("/fat/BENCH.TMP", 0);
                if fd < 0 {
                    return false;
                }
                let n = libr::write_fs(fd, &wbuf, 4096);
                let _ = libr::close(fd);
                n == 4096
            });
        }
    }

    if ok {
        println!("[bench] DONE ok=1");
    } else {
        println!("[bench] DONE ok=0");
    }
    let _ = libr::send(libr::CHANNEL_PARENT, 0x7E, 0, 0); // init: bench finito
    libr::exit(if ok { 0 } else { 1 })
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    if let Some(loc) = info.location() {
        println!("[bench] panic @ {}:{}", loc.file(), loc.line());
    } else {
        println!("[bench] panic");
    }
    libr::exit(1)
}
