#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![feature(alloc_error_handler)]

extern crate alloc;

mod boot_info;
mod boot_tables;
mod channels;
mod context;
mod gdt;
mod heap;
mod idle;
mod interrupts;
mod pic;
mod phys_mem;
mod pit;
mod process;
// Scheduler unico: RT a 32 priorita' + CBS (Fase 11). Il file mantiene il nome
// `sched_rt.rs`; esposto come `crate::sched` per i chiamanti.
#[path = "sched_rt.rs"]
mod sched;
mod cbs;
mod serial;
mod syscall;
mod user_binary;
mod vga;
mod vmm;
mod vmm_user;

use boot_info::HVM_START_MAGIC;
use core::panic::PanicInfo;
use x86_64::instructions::hlt;

#[unsafe(no_mangle)]
pub extern "C" fn rust_main(boot_info_phys: u64) -> ! {
    // Guard mappa di boot (PRIMA di qualunque print): l'identity map iniziale
    // copre [0, BOOT_MAP_LIMIT) e i print con timestamp leggono `pit::TICKS`
    // (.bss). Se il kernel — ingrossato dai binari embedded — supera il tetto,
    // il primo print farebbe triple fault a ZERO output (Fase 17: .bss oltre
    // i 2 MiB). Fail loud qui con raw serial (porta diretta, niente TICKS,
    // niente heap, niente format): solo immediati e indirizzo linker.
    {
        unsafe extern "C" {
            static _kernel_end: u8;
        }
        let kend = unsafe { &_kernel_end as *const u8 as u64 };
        if kend >= boot_tables::BOOT_MAP_LIMIT {
            const MSG: &[u8] = b"BOOT MAP TOO SMALL: kernel exceeds boot identity map\r\n";
            let mut i = 0usize;
            while i < MSG.len() {
                unsafe {
                    core::arch::asm!("out dx, al", in("dx") 0x3F8u16, in("al") MSG[i]);
                }
                i += 1;
            }
            loop {
                unsafe { core::arch::asm!("hlt") };
            }
        }
    }

    gdt::init();
    interrupts::init();
    pic::init();
    pit::init();
    syscall::init();

    let info = unsafe { boot_info::at(boot_info_phys) };
    assert_eq!(
        info.magic, HVM_START_MAGIC,
        "hvm_start_info magic errato: EBX non punta alla struttura PVH"
    );
    boot_info::dump(info);

    // Calcola max_addr dalla memory map PRIMA di tutto.
    let memmap = boot_info::memmap(info);
    let max_addr = memmap
        .iter()
        .filter(|e| e.kind == boot_info::MEM_RAM)
        .map(|e| e.addr + e.size)
        .max()
        .unwrap_or(256 * 1024 * 1024);

    serial_println!("[boot] max_addr RAM: {:#x} ({} MiB)", max_addr, max_addr / (1024 * 1024));

    // Fase 4: identity map dinamica → frame allocator → heap
    vmm::init(max_addr);
    vmm_user::init();

    unsafe extern "C" {
        static _kernel_start: u8;
        static _kernel_end: u8;
    }
    let kernel_start = unsafe { &_kernel_start as *const u8 as u64 };
    let kernel_end = unsafe { &_kernel_end as *const u8 as u64 };

    phys_mem::init(memmap, kernel_start, kernel_end);

    // Riserva la regione del kernel heap NEL frame allocator: se non la si
    // marca "used", i frame che la compongono verrebbero dati ai processi e
    // sovrascriverebbero la free-list dell'heap (corruzione).
    {
        let hs = phys_mem::bitmap_end();
        phys_mem::reserve(hs, crate::heap::HEAP_SIZE as u64);
    }

    // Pagina fisica scratch per i test userspace di `map_physical`
    // (usertests, testland): riservata qui cosi' il frame allocator non la
    // assegna a nessun processo.
    {
        phys_mem::reserve(syscall_numbers::MAP_TEST_PHYS, syscall_numbers::MAP_TEST_FRAMES * 4096);
    }

    // Heap: subito dopo bitmap + kernel
    let heap_start = phys_mem::bitmap_end();
    heap::init(heap_start);

    // Fase 5 step 2: scheduler preemptive con context switch reale.
    sched::init();
    // Ordine spawn = ordine PID: idle=0, init=1 (Linux convention). Gli altri
    // processi user sono spaw da init. I processi kernel non hanno canale di
    // nascita (parent_chan=None, ADR-0008). (Fase 15: il processo `keyboard`
    // e' stato eliminato — il driver PS/2 vive in userspace come `userkbd`.)
    sched::spawn("idle", sched::Priority::Idle, idle::idle, None, None);

    // Fase 8.1: init, primo processo user (PID 1), antenato dei servizi
    // che poi creera' via syscall `spawn`.
    user_binary::spawn_init();

    serial_println!("BOOT_OK");

    x86_64::instructions::interrupts::enable();

    println!("Welcome to Velordor v0.5 — Fase 5");
    println!("Scheduler preemptive timer-driven attivo");
    println!();

    #[cfg(feature = "selftest")]
    selftests();

    loop {
        hlt();
    }
}

#[cfg(feature = "selftest")]
fn selftests() {
    use x86_64::instructions::interrupts::int3;

    serial_println!("[test] int3 -> atteso #BREAKPOINT e continuazione");
    int3();
    serial_println!("[test] int3 ok (siamo tornati)");

    let t0 = pit::ticks();
    for _ in 0..50_000 {
        core::hint::spin_loop();
    }
    let t1 = pit::ticks();
    serial_println!("[test] timer ticks: {} -> {} (differenza {})", t0, t1, t1 - t0);
    if t1 > t0 {
        serial_println!("[test] timer ok");
    } else {
        serial_println!("[test] ERRORE: ticks non avanzati");
    }

    serial_println!("[test] alloc -> Box + Vec");
    let b = alloc::boxed::Box::new(42u64);
    serial_println!("[test] Box value = {}", *b);
    let v = alloc::vec![1u32, 2, 3, 4, 5];
    serial_println!("[test] Vec = {:?}", v.as_slice());
    drop(b);
    drop(v);
    serial_println!("[test] alloc ok");

    serial_println!("[test] frame alloc: liberi prima = {}", phys_mem::free_frames());
    let f1 = phys_mem::alloc().expect("frame alloc fallito");
    let f2 = phys_mem::alloc().expect("frame alloc fallito");
    assert_ne!(f1, f2, "allocatore ha restituito due volte la stessa frame");
    assert!(f1 < vmm::mapped_max() && f2 < vmm::mapped_max());
    serial_println!(
        "[test] frame {} e {} allocati, liberi dopo = {}, usati = {}",
        f1, f2,
        phys_mem::free_frames(),
        phys_mem::used_frames()
    );
    phys_mem::free(f1);
    phys_mem::free(f2);
    serial_println!("[test] frame liberati, liberi di nuovo = {}", phys_mem::free_frames());
    serial_println!("[test] frame alloc ok");

    serial_println!(
        "[test] lettura oltre il tetto mappa ({:#x}) -> atteso #PAGE FAULT",
        vmm::mapped_max() + 0x200000
    );
    let bad = (vmm::mapped_max() + 0x200000) as *const u64;
    let _ = unsafe { bad.read_volatile() };
    serial_println!("[test] ERRORE: la lettura non doveva riuscire");
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    crate::serial_println!("[PANIC] {}", info);
    println!("[PANIC] {}", info);
    loop {
        hlt();
    }
}
