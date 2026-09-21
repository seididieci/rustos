//! runhello — primo programma lanciabile dalla shell (Fase 37.2).
//!
//! Stampa gli argv ricevuti (uno per riga) su seriale ed esce 0; se un
//! argomento e' `fail` esce 3 (dopo aver stampato). Serve alla shell (`run`)
//! e a `test-shell.py` come target foreground/background con exit code
//! osservabile. Output su seriale (come i test), non sul terminale VGA:
//! non apre alcun device.

#![no_std]
#![no_main]

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
    libr::exit(if fail { 3 } else { 0 });
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    println!("[runhello] panic");
    libr::exit(1)
}
