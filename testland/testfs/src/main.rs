//! usertestfs — Test program for Phase 9.1 (ramfs via IPC).
//!
//! Tests: open + read "hello.txt", readdir "/", write + read verification.

#![no_std]
#![no_main]

extern crate alloc;
use alloc::vec;
use alloc::vec::Vec;

use libr;
use libr::{println, print_str};

libr::entry!(real_main);
fn real_main(_sp: u64) -> ! {
    let pid = libr::getpid();
    println!("[testfs] starting, pid={}", pid);
    let mut all_ok = true;

    // Test 1: Open and read "hello.txt" (pre-populated by userfs)
    println!("[testfs] Test 1: read hello.txt");
    let fd = libr::open("hello.txt", 0);
    match fd {
        Ok(fd) => {
            println!("[testfs] open fd={}", fd);
            let mut buf = [0u8; 256];
            match libr::read_fs(fd, &mut buf, 256) {
                Ok(n) => {
                    print_str!("[testfs] read {} bytes: ", n);
                    if n > 0 {
                        libr::write_raw(buf.as_ptr(), n);
                    }
                    println!();
                }
                Err(e) => {
                    println!("[testfs] read failed: {:?}", e);
                }
            }
            let _ = libr::close(fd);
        }
        Err(e) => {
            println!("[testfs] open failed: {:?}", e);
        }
    }

    // Test 2: Readdir "/"
    println!("[testfs] Test 2: readdir /");
    let mut entries = [0u8; 1024];
    let count = libr::readdir("/", &mut entries, 1024);
    match count {
        Ok(count) => {
            println!("[testfs] readdir count={}", count);
            // Print entries (null-terminated strings, traversal in `libr`, A4).
            if count > 0 {
                libr::test::each_name(&entries, count, |name| {
                    print_str!("[testfs]   ");
                    libr::write_raw(name.as_ptr(), name.len());
                    println!();
                });
            }
        }
        Err(e) => {
            println!("[testfs] readdir failed: {:?}", e);
        }
    }

    // Test 3: Write a file and read it back
    println!("[testfs] Test 3: write + read verification");
    let fd2 = libr::open("test_write.txt", libr::O_CREAT);
    match fd2 {
        Ok(fd2) => {
            println!("[testfs] open fd={}", fd2);
            let msg = b"Hello from testfs!\n";
            match libr::write_fs(fd2, msg, msg.len()) {
                Ok(n) => println!("[testfs] wrote {} bytes", n),
                Err(e) => println!("[testfs] write failed: {:?}", e),
            }
            let _ = libr::close(fd2);

            // Read it back
            match libr::open("test_write.txt", 0) {
                Ok(fd3) => {
                    let mut buf2 = [0u8; 256];
                    match libr::read_fs(fd3, &mut buf2, 256) {
                        Ok(n2) => {
                            print_str!("[testfs] read back {} bytes: ", n2);
                            if n2 > 0 {
                                libr::write_raw(buf2.as_ptr(), n2);
                            }
                            println!();

                            // Verify
                            let ok = n2 == msg.len() && &buf2[..n2] == msg;
                            all_ok &= ok;
                            if ok {
                                println!("[testfs] verification: PASS");
                            } else {
                                println!("[testfs] verification: FAIL");
                            }
                        }
                        Err(e) => {
                            all_ok = false;
                            println!("[testfs] verification: FAIL (read {:?})", e);
                        }
                    }
                    let _ = libr::close(fd3);
                }
                Err(_) => {
                    all_ok = false;
                    println!("[testfs] verification: FAIL (open for read)");
                }
            }
        }
        Err(_) => {
            all_ok = false;
            println!("[testfs] verification: FAIL (open for write)");
        }
    }

    // Test 4: mkdir + readdir verification (Fase 9.4.3)
    println!("[testfs] Test 4: mkdir prova");
    let r = libr::mkdir("prova");
    match r {
        Ok(()) => {
            println!("[testfs] mkdir ret=0");
            let mut entries2 = [0u8; 1024];
            match libr::readdir("/", &mut entries2, 1024) {
                Ok(c2) => {
                    println!("[testfs] readdir count={}", c2);
                    let mut found = false;
                    let mut i = 0;
                    while i < entries2.len() && entries2[i] != 0 {
                        let start = i;
                        while i < entries2.len() && entries2[i] != 0 {
                            i += 1;
                        }
                        if &entries2[start..i] == b"prova" {
                            found = true;
                        }
                        if i < entries2.len() && entries2[i] == 0 {
                            i += 1;
                        }
                    }
                    if found {
                        all_ok &= true;
                        println!("[testfs] mkdir prova: PASS");
                    } else {
                        all_ok = false;
                        println!("[testfs] mkdir prova: FAIL");
                    }
                }
                Err(e) => {
                    all_ok = false;
                    println!("[testfs] mkdir prova: FAIL (readdir {:?})", e);
                }
            }
        }
        Err(_) => {
            all_ok = false;
            println!("[testfs] mkdir prova: FAIL");
        }
    }

    // Test 5: heap lazy on-demand (sbrk riserva VA, page fault demand-zero
    // materializza): Vec grande multi-pagina (300 KB) allocato/riempito/
    // verificato/liberato mentre gli altri processi fanno IPC.
    println!("[testfs] Test 5: heap lazy 300KB");
    let mut big: Vec<u8> = vec![0u8; 300_000];
    for i in 0..big.len() {
        big[i] = (i % 251) as u8;
    }
    let mut ok5 = true;
    for i in (0..big.len()).step_by(997) {
        if big[i] != (i % 251) as u8 {
            ok5 = false;
            break;
        }
    }
    drop(big);

    let small: Vec<u8> = vec![0xAB; 64];
    ok5 = ok5 && small.iter().all(|&b| b == 0xAB);
    all_ok &= ok5;
    if ok5 {
        println!("[testfs] heap lazy 300KB: PASS");
    } else {
        println!("[testfs] heap lazy 300KB: FAIL");
    }

    if all_ok {
        println!("[testfs] PASS 5/5");
    } else {
        println!("[testfs] FAIL");
    }
    println!("[testfs] all tests done");
    let _ = libr::send(libr::CHANNEL_PARENT, libr::TEST_DONE, 0, 0); // init: test finito (spawn sequenziale)
    libr::exit(0)
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    println!("[testfs] panic");
    libr::exit(1)
}
