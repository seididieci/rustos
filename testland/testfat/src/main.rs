//! usertestfat — Test program for Phase 9.2 (FAT32 via IPC) + Fase 20 (scrivibile).
//!
//! Tests: readdir "/fat", read "/fat/HELLO.TXT", read "/fat/SUB/NOTES.TXT",
//! overwrite + restore (pristino per i test dopo), create+grow multicluster,
//! /dev/null, /dev/zero.

#![no_std]
#![no_main]

use libr;
use libr::{println, print_str};

fn read_all(fd: i64, buf: &mut [u8]) -> i64 {
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

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let pid = libr::getpid();
    println!("[testfat] starting, pid={}", pid);
    let mut all_ok = true;

    // Test 1: readdir "/fat" — attese HELLO.TXT, README.TXT, SUB + BIN, TEST
    // (Fase 21: servizi da disco iniettati a build via mcopy).
    println!("[testfat] Test 1: readdir /fat");
    let mut entries = [0u8; 2048];
    let count = libr::readdir("/fat", &mut entries, 2048);
    println!("[testfat] readdir count={}", count);
    if count > 0 {
        // Raccoglie fino a 8 nomi (stesso bound del loop originario, A4).
        let mut names: [&str; 8] = [""; 8];
        let mut n_names = 0usize;
        libr::test::each_name(&entries, count as usize, |s| {
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
    } else {
        all_ok = false;
        println!("[testfat] readdir /fat: FAIL (count={})", count);
    }

    // Test 2: leggere /fat/HELLO.TXT
    println!("[testfat] Test 2: read /fat/HELLO.TXT");
    let fd = libr::open("/fat/HELLO.TXT", 0);
    println!("[testfat] open fd={}", fd);
    if fd >= 0 {
        let mut buf = [0u8; 128];
        let n = read_all(fd, &mut buf);
        let want = b"Hello from Velordor FAT32!\n";
        all_ok &= expect("HELLO.TXT", &buf[..n as usize], want);
        let _ = libr::close(fd);
    } else {
        all_ok = false;
    }

    // Test 3: leggere file in sotto-directory
    println!("[testfat] Test 3: read /fat/SUB/NOTES.TXT");
    let fd = libr::open("/fat/SUB/NOTES.TXT", 0);
    println!("[testfat] open fd={}", fd);
    if fd >= 0 {
        let mut buf = [0u8; 128];
        let n = read_all(fd, &mut buf);
        all_ok &= expect("SUB/NOTES.TXT", &buf[..n as usize], b"Subdirectory note.\n");
        let _ = libr::close(fd);
    } else {
        all_ok = false;
    }

    // Test 4: overwrite su /fat (Fase 20, scrivibile) + restore pristino.
    // HELLO.TXT resta identica dopo il test (contenuto E size): i test dopo
    // (usertests, shell) la leggono come fixture.
    println!("[testfat] Test 4: overwrite + restore /fat/HELLO.TXT");
    let orig = b"Hello from Velordor FAT32!\n";
    let fd = libr::open("/fat/HELLO.TXT", 0);
    if fd >= 0 {
        let n = libr::write_fs(fd, b"modified", 8);
        let wok = n == 8;
        all_ok &= expect("overwrite 8B", &[wok as u8], &[1]);
        let _ = libr::close(fd);
        // Read-back: i primi 8 byte nuovi, il resto originale.
        let fd = libr::open("/fat/HELLO.TXT", 0);
        println!("[testfat] reopen fd={}", fd);
        let mut buf = [0u8; 32];
        let n = read_all(fd, &mut buf);
        println!("[testfat] reread n={}", n);
        let mut want = [0u8; 27]; // orig = 27 B (contati)
        want[..8].copy_from_slice(b"modified");
        want[8..].copy_from_slice(&orig[8..]);
        let rok = n == 27 && buf[..27] == want;
        all_ok &= expect("read-back overwrite", &[rok as u8], &[1]);
        let _ = libr::close(fd);
        // Restore pristino (stessa size: solo overwrite, mai grow qui).
        // Riapre: l'offset del fd letto e' a EOF, la write appenderebbe.
        let fd = libr::open("/fat/HELLO.TXT", 0);
        let n = libr::write_fs(fd, orig, orig.len());
        let bok = n == orig.len() as i64;
        all_ok &= expect("restore write", &[bok as u8], &[1]);
        let _ = libr::close(fd);
        let fd = libr::open("/fat/HELLO.TXT", 0);
        let mut buf = [0u8; 32];
        let n = read_all(fd, &mut buf);
        all_ok &= expect("HELLO.TXT pristino", &buf[..n as usize], orig);
        let _ = libr::close(fd);
    } else {
        all_ok = false;
    }

    // Test 5: /dev/null — write ok, read ritorna 0 byte
    println!("[testfat] Test 5: /dev/null");
    let fd = libr::open("/dev/null", 0);
    println!("[testfat] open /dev/null fd={}", fd);
    if fd >= 0 {
        let n = libr::write_fs(fd, b"test", 4);
        let write_ok = n >= 0;
        all_ok &= expect("/dev/null write", &[write_ok as u8], &[1]);
        let mut buf = [0xFFu8; 16];
        let n = libr::read_fs(fd, &mut buf, 16);
        let read_ok = n == 0;
        all_ok &= expect("/dev/null read=0", &[read_ok as u8], &[1]);
        let _ = libr::close(fd);
    } else {
        all_ok = false;
        println!("[testfat] /dev/null: FAIL (open fd={})", fd);
    }

    // Test 6: /dev/zero — read ritorna zeri
    println!("[testfat] Test 6: /dev/zero");
    let fd = libr::open("/dev/zero", 0);
    println!("[testfat] open /dev/zero fd={}", fd);
    if fd >= 0 {
        let mut buf = [0xFFu8; 16];
        let n = libr::read_fs(fd, &mut buf, 16);
        let read_ok = n == 16 && buf == [0u8; 16];
        all_ok &= expect("/dev/zero read=16 zeros", &[read_ok as u8], &[1]);
        let _ = libr::close(fd);
    } else {
        all_ok = false;
        println!("[testfat] /dev/zero: FAIL (open fd={})", fd);
    }

    // Test 7: create + grow multicluster (Fase 20.3/20.4): file nuovo da
    // vuoto a 9000 B (> 1 cluster da 4 KiB: allocazione + size update),
    // read-back con pattern. Il file resta (niente unlink su FAT, fuori
    // scope): gli assert dopo usano solo presenza/contenuto, mai conteggi.
    println!("[testfat] Test 7: create + grow 9000B /fat/TFATW.TXT");
    let fd = libr::open("/fat/TFATW.TXT", libr::O_CREAT);
    println!("[testfat] create fd={}", fd);
    if fd >= 0 {
        let mut chunk = [0u8; 1000];
        let mut wok = true;
        for k in 0..9 {
            for i in 0..1000 {
                chunk[i] = ((k * 1000 + i) % 251) as u8;
            }
            let n = libr::write_fs(fd, &chunk, 1000);
            if n != 1000 {
                wok = false;
                break;
            }
        }
        all_ok &= expect("write 9x1000B", &[wok as u8], &[1]);
        let _ = libr::close(fd);
        // Size via stat + read-back integrale.
        let mut st = libr::Stat { size: 0, kind: 0, readonly: false };
        let sok = libr::stat("/fat/TFATW.TXT", &mut st) == 0
            && st.is_file()
            && st.size == 9000;
        all_ok &= expect("stat size=9000", &[sok as u8], &[1]);
        let fd = libr::open("/fat/TFATW.TXT", 0);
        let mut back = [0u8; 9000];
        let mut got = 0usize;
        while got < 9000 {
            let n = libr::read_fs(fd, &mut back[got..], 9000 - got);
            if n <= 0 {
                break;
            }
            got += n as usize;
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
    } else {
        all_ok = false;
        println!("[testfat] create /fat/TFATW.TXT: FAIL (open fd={})", fd);
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
