//! Port I/O (x86 `in`/`out`) eseguito dal ring 3.
//!
//! Il processo `userkbd` puo' usare queste istruzioni solo sulle porte abilitate
//! dalla sua I/O bitmap nel TSS per-processo (ADR-0006): PS/2 0x60-0x64
//! (dichiarate in `io_ranges` in `user_binary.rs`). Ogni altra porta genera #GP.

#[inline(always)]
pub unsafe fn outb(port: u16, val: u8) {
    unsafe {
        core::arch::asm!("out dx, al", in("dx") port, in("al") val, options(nostack, nomem));
    }
}

#[inline(always)]
pub unsafe fn inb(port: u16) -> u8 {
    let val: u8;
    unsafe {
        core::arch::asm!("in al, dx", out("al") val, in("dx") port, options(nostack, nomem));
    }
    val
}
