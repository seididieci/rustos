//! Port I/O (x86 `in`/`out`) eseguito dal ring 3.
//!
//! Il processo `userdisk` puo' usare queste istruzioni solo sulle porte
//! abilitate dalla sua I/O bitmap nel TSS per-processo (ADR-0006): ATA
//! primario 0x1F0-0x1F7, 0x3F6-0x3F7 + secondario 0x170-0x177, 0x376-0x377
//! (Fase 16, enumerazione). Ogni altra porta genera #GP.

#![allow(dead_code)]

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

#[inline(always)]
pub unsafe fn inw(port: u16) -> u16 {
    let val: u16;
    unsafe {
        core::arch::asm!("in ax, dx", out("ax") val, in("dx") port, options(nostack, nomem));
    }
    val
}

#[inline(always)]
pub unsafe fn outw(port: u16, val: u16) {
    unsafe {
        core::arch::asm!("out dx, ax", in("dx") port, in("ax") val, options(nostack, nomem));
    }
}
