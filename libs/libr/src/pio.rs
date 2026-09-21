//! Port I/O x86 (`in`/`out`) eseguito dal ring 3 (A4: prima duplicato in
//! `userdisk/src/io.rs` e `userkbd/src/io.rs`, identici per `inb`/`outb`).
//!
//! Ogni processo puo' usare queste istruzioni solo sulle porte abilitate dalla
//! propria I/O bitmap nel TSS per-processo (ADR-0006); ogni altra porta genera
//! #GP. I driver dichiarano le proprie porte in `io_ranges` (`user_binary.rs`).

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

// 38.0d — 32 bit per lo spazio di configurazione PCI (0xCF8/0xCFC).
// Stesso pattern sopra: valgono le stesse regole I/O bitmap (ADR-0006).

#[inline(always)]
pub unsafe fn outl(port: u16, val: u32) {
    unsafe {
        core::arch::asm!("out dx, eax", in("dx") port, in("eax") val, options(nostack, nomem));
    }
}

#[inline(always)]
pub unsafe fn inl(port: u16) -> u32 {
    let val: u32;
    unsafe {
        core::arch::asm!("in eax, dx", out("eax") val, in("dx") port, options(nostack, nomem));
    }
    val
}
