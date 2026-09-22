//! usertestfat — Test program for Phase 9.2 (FAT32 via IPC) + Fase 20 (scrivibile).
//!
//! Tests: readdir "/fat", read "/fat/HELLO.TXT", read "/fat/SUB/NOTES.TXT",
//! overwrite + restore (pristino per i test dopo), create+grow multicluster,
//! /dev/null, /dev/zero.

#![no_std]
#![no_main]

use libr;
use libr::{println, print_str};

fn read_all(fd: i64, buf: &mut [u8]) -> Result<usize, libr::Error> {
    libr::read_fs(fd, buf, buf.len())
}

/// Verifica che il contenuto letto combaci con l'atteso.
fn expect(name: &str, got: &[u8], want: &[u8]) -> bool {
    let ok = got.len() == want.len() && got == want;
    print_str!("[testfat] {}: ", name);
    if ok {
        println!("PASS");
    } else {
        println!("FAIL (got {:?}, want {:?})", core::str::from_utf8(got).unwrap_or("?"),
            core::str::from_utf8(want).unwrap_or("?"));
    }
    ok
}

libr::entry!(real_main);
fn real_main(_sp: u64) -> ! {
    let pid = libr::getpid();
    println!("[testfat] starting, pid={}", pid);
    let mut all_ok = true;

    // Test 1: readdir "/fat" — attese HELLO.TXT, README.TXT, SUB + BIN, TEST
    // (Fase 21: servizi da disco iniettati a build via mcopy).
    println!("[testfat] Test 1: readdir /fat");
    let mut entries = [0u8; 2048];
    match libr::readdir("/fat", &mut entries, 2048) {
        Ok(count) => {
            println!("[testfat] readdir count={}", count);
            // Raccoglie fino a 8 nomi (stesso bound del loop originario, A4).
            let mut names: [&str; 8] = [""; 8];
            let mut n_names = 0usize;
            libr::test::each_name(&entries, count, |s| {
                if n_names < 8 {
                    names[n_names] = s;
                    n_names += 1;
                }
            });
            let has_hello = names[..n_names].contains(&"HELLO.TXT");
            let has_readme = names[..n_names].contains(&"README.TXT");
            let has_sub = names[..n_names].contains(&"SUB");
            let has_bin = names[..n_names].contains(&"BIN");
            let has_test = names[..n_names].contains(&"TEST");
            let ok = count == 5 && has_hello && has_readme && has_sub && has_bin && has_test;
            println!("[testfat] entries: {:?}", &names[..n_names]);
            all_ok &= expect("readdir /fat (5 entry)", &[ok as u8], &[1]);
        }
        Err(_) => {
            all_ok = false;
            println!("[testfat] readdir /fat: FAIL");
        }
    }

    // Test 2: leggere /fat/HELLO.TXT
    println!("[testfat] Test 2: read /fat/HELLO.TXT");
    match libr::open("/fat/HELLO.TXT", 0) {
        Ok(fd) => {
            println!("[testfat] open fd={}", fd);
            let mut buf = [0u8; 128];
            match read_all(fd, &mut buf) {
                Ok(n) => {
                    let want = b"Hello from Velordor FAT32!\n";
                    all_ok &= expect("HELLO.TXT", &buf[..n], want);
                }
                Err(e) => {
                    all_ok = false;
                    println!("[testfat] HELLO.TXT read: FAIL ({:?})", e);
                }
            }
            let _ = libr::close(fd);
        }
        Err(_) => {
            all_ok = false;
        }
    }

    // Test 3: leggere file in sotto-directory
    println!("[testfat] Test 3: read /fat/SUB/NOTES.TXT");
    match libr::open("/fat/SUB/NOTES.TXT", 0) {
        Ok(fd) => {
            println!("[testfat] open fd={}", fd);
            let mut buf = [0u8; 128];
            match read_all(fd, &mut buf) {
                Ok(n) => {
                    all_ok &= expect("SUB/NOTES.TXT", &buf[..n], b"Subdirectory note.\n");
                }
                Err(e) => {
                    all_ok = false;
                    println!("[testfat] NOTES.TXT read: FAIL ({:?})", e);
                }
            }
            let _ = libr::close(fd);
        }
        Err(_) => {
            all_ok = false;
        }
    }

    // Test 4: overwrite su /fat (Fase 20, scrivibile) + restore pristino.
    // HELLO.TXT resta identica dopo il test (contenuto E size): i test dopo
    // (usertests, shell) la leggono come fixture.
    println!("[testfat] Test 4: overwrite + restore /fat/HELLO.TXT");
    let orig = b"Hello from Velordor FAT32!\n";
    match libr::open("/fat/HELLO.TXT", 0) {
        Ok(fd) => {
            let wok = libr::write_fs(fd, b"modified", 8) == Ok(8);
            all_ok &= expect("overwrite 8B", &[wok as u8], &[1]);
            let _ = libr::close(fd);
            // Read-back: i primi 8 byte nuovi, il resto originale.
            match libr::open("/fat/HELLO.TXT", 0) {
                Ok(fd) => {
                    println!("[testfat] reopen fd={}", fd);
                    let mut buf = [0u8; 32];
                    match read_all(fd, &mut buf) {
                        Ok(n) => {
                            println!("[testfat] reread n={}", n);
                            let mut want = [0u8; 27]; // orig = 27 B (contati)
                            want[..8].copy_from_slice(b"modified");
                            want[8..].copy_from_slice(&orig[8..]);
                            let rok = n == 27 && buf[..27] == want;
                            all_ok &= expect("read-back overwrite", &[rok as u8], &[1]);
                        }
                        Err(e) => {
                            all_ok &= expect("read-back overwrite", &[], &[1]);
                            println!("[testfat] reread: FAIL ({:?})", e);
                        }
                    }
                    let _ = libr::close(fd);
                }
                Err(_) => {
                    all_ok = false;
                }
            }
            // Restore pristino (stessa size: solo overwrite, mai grow qui).
            // Riapre: l'offset del fd letto e' a EOF, la write appenderebbe.
            match libr::open("/fat/HELLO.TXT", 0) {
                Ok(fd) => {
                    let bok = libr::write_fs(fd, orig, orig.len()) == Ok(orig.len());
                    all_ok &= expect("restore write", &[bok as u8], &[1]);
                    let _ = libr::close(fd);
                    match libr::open("/fat/HELLO.TXT", 0) {
                        Ok(fd) => {
                            let mut buf = [0u8; 32];
                            match read_all(fd, &mut buf) {
                                Ok(n) => {
                                    all_ok &= expect("HELLO.TXT pristino", &buf[..n], orig);
                                }
                                Err(e) => {
                                    all_ok = false;
                                    println!("[testfat] pristino read: FAIL ({:?})", e);
                                }
                            }
                            let _ = libr::close(fd);
                        }
                        Err(_) => {
                            all_ok = false;
                        }
                    }
                }
                Err(_) => {
                    all_ok = false;
                }
            }
        }
        Err(_) => {
            all_ok = false;
        }
    }

    // Test 5: /dev/null — write ok, read ritorna 0 byte
    println!("[testfat] Test 5: /dev/null");
    match libr::open("/dev/null", 0) {
        Ok(fd) => {
            println!("[testfat] open /dev/null fd={}", fd);
            let write_ok = libr::write_fs(fd, b"test", 4).is_ok();
            all_ok &= expect("/dev/null write", &[write_ok as u8], &[1]);
            let mut buf = [0xFFu8; 16];
            let read_ok = libr::read_fs(fd, &mut buf, 16) == Ok(0);
            all_ok &= expect("/dev/null read=0", &[read_ok as u8], &[1]);
            let _ = libr::close(fd);
        }
        Err(_) => {
            all_ok = false;
            println!("[testfat] /dev/null: FAIL (open)");
        }
    }

    // Test 6: /dev/zero — read ritorna zeri
    println!("[testfat] Test 6: /dev/zero");
    match libr::open("/dev/zero", 0) {
        Ok(fd) => {
            println!("[testfat] open /dev/zero fd={}", fd);
            let mut buf = [0xFFu8; 16];
            let read_ok = match libr::read_fs(fd, &mut buf, 16) {
                Ok(n) => n == 16 && buf == [0u8; 16],
                Err(_) => false,
            };
            all_ok &= expect("/dev/zero read=16 zeros", &[read_ok as u8], &[1]);
            let _ = libr::close(fd);
        }
        Err(_) => {
            all_ok = false;
            println!("[testfat] /dev/zero: FAIL (open)");
        }
    }

    // Test 7: create + grow multicluster (Fase 20.3/20.4): file nuovo da
    // vuoto a 9000 B (> 1 cluster da 4 KiB: allocazione + size update),
    // read-back con pattern. Il file resta (niente unlink su FAT, fuori
    // scope): gli assert dopo usano solo presenza/contenuto, mai conteggi.
    println!("[testfat] Test 7: create + grow 9000B /fat/TFATW.TXT");
    match libr::open("/fat/TFATW.TXT", libr::O_CREAT) {
        Ok(fd) => {
            println!("[testfat] create fd={}", fd);
            let mut chunk = [0u8; 1000];
            let mut wok = true;
            for k in 0..9 {
                for i in 0..1000 {
                    chunk[i] = ((k * 1000 + i) % 251) as u8;
                }
                if libr::write_fs(fd, &chunk, 1000) != Ok(1000) {
                    wok = false;
                    break;
                }
            }
            all_ok &= expect("write 9x1000B", &[wok as u8], &[1]);
            let _ = libr::close(fd);
            // Size via stat + read-back integrale.
            let mut st = libr::Stat { size: 0, kind: 0, readonly: false };
            let sok = libr::stat("/fat/TFATW.TXT", &mut st).is_ok()
                && st.is_file()
                && st.size == 9000;
            all_ok &= expect("stat size=9000", &[sok as u8], &[1]);
            match libr::open("/fat/TFATW.TXT", 0) {
                Ok(fd) => {
                    let mut back = [0u8; 9000];
                    let mut got = 0usize;
                    while got < 9000 {
                        match libr::read_fs(fd, &mut back[got..], 9000 - got) {
                            Ok(0) => break,
                            Ok(n) => got += n,
                            Err(_) => break,
                        }
                    }
                    let mut rok = got == 9000;
                    if rok {
                        for i in 0..9000 {
                            if back[i] != (i % 251) as u8 {
                                rok = false;
                                break;
                            }
                        }
                    }
                    all_ok &= expect("read-back 9000B pattern", &[rok as u8], &[1]);
                    let _ = libr::close(fd);
                }
                Err(_) => {
                    all_ok = false;
                }
            }
        }
        Err(_) => {
            all_ok = false;
            println!("[testfat] create /fat/TFATW.TXT: FAIL (open)");
        }
    }

    if all_ok {
        println!("[testfat] PASS 7/7");
    } else {
        println!("[testfat] FAIL");
    }
    println!("[testfat] all tests done");
    let _ = libr::send(libr::CHANNEL_PARENT, libr::TEST_DONE, 0, 0); // init: test finito (spawn sequenziale)
    libr::exit(0)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    if let Some(loc) = info.location() {
        println!("[testfat] panic @ {}:{}", loc.file(), loc.line());
    } else {
        println!("[testfat] panic");
    }
    libr::exit(1)
}
