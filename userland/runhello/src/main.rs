//! runhello — primo programma lanciabile dalla shell (Fase 37.2).
//!
//! Stampa gli argv ricevuti (uno per riga) su seriale ed esce 0; se un
//! argomento e' `fail` esce 3 (dopo aver stampato). Con stdin redirectato
//! (`<`, Fase 40.4d) stampa anche i byte letti (riga `runhello: stdin:...`);
//! senza redirect nessuno effetto (stdin_byte = None subito). Serve alla shell
//! (`run`) e a `test-shell.py` come target foreground/background con exit code
//! osservabile. Output su seriale (come i test), non sul terminale VGA:
//! non apre alcun device.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use libr;
use libr::println;

libr::entry!(real_main);
fn real_main(sp: u64) -> ! {
    let mut fail = false;
    match libr::args_from_stack(sp) {
        Some(args) => {
            let mut i = 1u64;
            while let Some(a) = args.get(i) {
                match core::str::from_utf8(a) {
                    Ok(s) => {
                        println!("runhello: {}", s);
                        if s == "fail" {
                            fail = true;
                        }
                    }
                    Err(_) => println!("runhello: <non utf8>"),
                }
                i += 1;
            }
        }
        None => println!("runhello: argv illeggibili"),
    }
    // Drain stdin redirectato (Fase 40.4d): byte per byte fino a EOF; niente
    // retry (file, mai device-a-caratteri qui). Senza redirect il primo
    // stdin_byte e' gia' None: zero righe, zero effetti.
    {
        let mut stdin = Vec::new();
        loop {
            match libr::stdin_byte() {
                Some(b) => stdin.push(b),
                None => break,
            }
        }
        if !stdin.is_empty() {
            match core::str::from_utf8(&stdin) {
                Ok(s) => println!("runhello: stdin:{}", s),
                Err(_) => println!("runhello: stdin:<non utf8>"),
            }
        }
    }
    libr::exit(if fail { 3 } else { 0 });
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    println!("[runhello] panic");
    libr::exit(1)
}
