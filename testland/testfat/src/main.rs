//! usertestfat — Test program for Phase 9.2 (FAT32 read-only via IPC).
//!
//! Tests: readdir "/fat", read "/fat/HELLO.TXT", read "/fat/SUB/NOTES.TXT",
//! write su /fat deve fallire (read-only).

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

    // Test 1: readdir "/fat" — attese HELLO.TXT, README.TXT, SUB
    println!("[testfat] Test 1: readdir /fat");
    let mut entries = [0u8; 2048];
    let count = libr::readdir("/fat", &mut entries, 2048);
    println!("[testfat] readdir count={}", count);
    if count > 0 {
        let mut i = 0;
        let mut names: [&str; 8] = [""; 8];
        let mut n_names = 0usize;
        while i < entries.len() && n_names < 8 {
            if entries[i] == 0 {
                i += 1;
                continue;
            }
            let start = i;
            while i < entries.len() && entries[i] != 0 {
                i += 1;
            }
            if i > start {
                let s = core::str::from_utf8(&entries[start..i]).unwrap_or("?");
                names[n_names] = s;
                n_names += 1;
            }
            if i < entries.len() && entries[i] == 0 {
                i += 1;
            }
            if i < entries.len() && entries[i] == 0 {
                break;
            }
        }
        let has_hello = names[..n_names].contains(&"HELLO.TXT");
        let has_readme = names[..n_names].contains(&"README.TXT");
        let has_sub = names[..n_names].contains(&"SUB");
        let ok = count == 3 && has_hello && has_readme && has_sub;
        println!("[testfat] entries: {:?}", &names[..n_names]);
        all_ok &= expect("readdir /fat (3 entry)", &[ok as u8], &[1]);
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
        let want = b"Hello from rustOS FAT32!\n";
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

    // Test 4: write su /fat deve fallire (read-only)
    println!("[testfat] Test 4: write /fat read-only");
    let fd = libr::open("/fat/HELLO.TXT", 0);
    if fd >= 0 {
        let n = libr::write_fs(fd, b"modified", 8);
        let ok = n < 0;
        all_ok &= expect("write su /fat rifiutata", &[ok as u8], &[1]);
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

    if all_ok {
        println!("[testfat] PASS 6/6");
    } else {
        println!("[testfat] FAIL");
    }
    println!("[testfat] all tests done");
    let _ = libr::send(libr::CHANNEL_PARENT, 0x7E, 0, 0); // init: test finito (spawn sequenziale)
    libr::exit(0)
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    println!("[testfat] panic");
    libr::exit(1)
}
