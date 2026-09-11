//! userhogheap — riproduttore: genera molti page-fault demand-zero sull'heap
//! lazy, ciclando allocazioni multi-pagina senza fare IPC. Usato col reader di
//! /dev/zero per esporre il lost-wakeup quando heap lazy + device remoto
//! coesistono.

#![no_std]
#![no_main]

extern crate alloc;
use alloc::vec;
use alloc::vec::Vec;

use libr;

/// Grandezze (in byte) usate a rotazione, tutte multi-pagina.
const SIZES: [usize; 4] = [64 * 1024, 300_000, 1024 * 1024, 128 * 1024];

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let _ = libr::print_string(b"[hogheap] starting\n");

    let mut round: usize = 0;
    loop {
        let sz = SIZES[round % SIZES.len()];
        let mut v: Vec<u8> = vec![0u8; sz];
        for i in 0..sz {
            v[i] = (i % 251) as u8;
        }
        let ok = v[0] == 0 && v[sz - 1] == ((sz - 1) % 251) as u8;
        drop(v);

        round += 1;
        if round % 40 == 0 {
            if ok {
                let _ = libr::print_string(b"[hogheap] round done\n");
            } else {
                let _ = libr::print_string(b"[hogheap] CHECK FAIL\n");
            }
        }
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    let _ = libr::print_string(b"[hogheap] panic\n");
    libr::exit(1)
}
