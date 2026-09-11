# Interrupt Handling

## Panoramica

Gli interrupt sono il meccanismo con cui la CPU risponde a eventi hardware
o software. Velordor gestisce tre categorie:

| Tipo | Range | Esempi |
|------|-------|--------|
| CPU Exceptions | 0-31 | page fault, breakpoint, double fault |
| Hardware IRQ | 32-47 | PIT timer (IRQ 0), tastiera PS/2 (IRQ 1) |
| Software | futuri | syscall (SYSCALL instruction) |

## IDT (Interrupt Descriptor Table)

L'IDT è una tabella di 256 entry. Il kernel la costruisce con il crate
`x86_64` e la carica con `lidt`:

```rust
static IDT: Lazy<InterruptDescriptorTable> = Lazy::new(|| {
    let mut idt = InterruptDescriptorTable::new();
    idt.breakpoint.set_handler_fn(breakpoint_handler);
    idt.double_fault.set_handler_fn(double_fault_handler);
    idt[0x20].set_handler_fn(timer_handler);
    idt[0x21].set_handler_fn(keyboard_handler);
    // ...
    idt
});
```

Le eccezioni CPU usano la firma `extern "x86-interrupt" fn(ISF)`.
Gli interrupt hardware usano la stessa firma: `extern "x86-interrupt" fn(ISF)`.

## PIC 8259 — Remapping

Il PIC seriale va rimappato dopo il boot perché i suoi IRQ 0-7
si sovrappongono alle eccezioni CPU 0-7:

```
PRIMA:  IRQ 0-7  → INT 0-7   (conflitto con eccezioni CPU!)
DOPO:   IRQ 0-7  → INT 32-39  (master, offset 0x20)
        IRQ 8-15 → INT 40-47  (slave,  offset 0x28)
```

```rust
use pic8259::ChainedPics;
static PICS: Mutex<ChainedPics> =
    Mutex::new(unsafe { ChainedPics::new(0x20, 0x28) });

// Init: PICS.lock().initialize();
// Maschera: solo IRQ 0 (timer) e IRQ 1 (keyboard) abilitati.
// EOI dopo ogni handler: unsafe { PICS.lock().notify_end_of_interrupt(irq); }
```

**EOI (End of Interrupt)**: dopo ogni interrupt handler, il kernel deve
mandare EOI al PIC. Senza, il PIC non genera altri interrupt e il
sistema si blocca silenziosamente.

## PIT — Programmable Interval Timer

Il PIT genera IRQ 0 a frequenza costante. È il cuore dello scheduler:

```
Frequenza target: 100 Hz (ogni ~10 ms)
Divisore = 1_193_182 / 100 = 11_931
Porta 0x43: comando 0x36 (channel 0, lobyte/hibyte, rate generator)
Porta 0x40: low byte + high byte del divisore
```

```rust
extern "x86-interrupt" fn timer_handler(_stack_frame: InterruptStackFrame) {
    TICKS.fetch_add(1, Ordering::Relaxed);
    unsafe { PICS.lock().notify_end_of_interrupt(0); }
}
```

Il contatore `TICKS: AtomicU64` è globale: lo scheduler (Fase 5) lo
leggerà per decidere quando fare context switch.

## Tastiera PS/2

La tastiera invia uno scancode su IRQ 1 ogni volta che un tasto è
premuto o rilasciato:

```
Porta 0x60: scancode (byte)
IRQ 1 → INT 0x21 dopo remapping
```

Il crate `pc-keyboard` gestisce la decodifica:
scancode set 1 → evento tasto → decodifica layout → carattere Unicode.

```rust
extern "x86-interrupt" fn keyboard_handler(_stack_frame: InterruptStackFrame) {
    let scancode: u8 = unsafe { Port::new(0x60).read() };
    crate::kbd_events::push(scancode);
    // Risveglia il keyboard process, che decodifica e invia al console server.
    crate::kbd_process::notify();
    unsafe { crate::pic::end_of_interrupt(0x21) };
}
```

**Fase 8.2 completata**: il keyboard handler inserisce lo scancode in una coda
(`kbd_events`), poi il `keyboard process` (kernel thread) lo decodifica e lo
inoltra via IPC al *console server* userspace (`userconsole`). Se il server
non e' pronto, lo scancode viene stampato su seriale come fallback.

## Ordine di inizializzazione

```
GDT → IDT → PIC (remap + maschera) → PIT (~100 Hz) → keyboard → STI
```

`sti` (enable interrupts) viene dopo aver configurato tutto: se lo
chiamassimo prima, un IRQ non gestito causerebbe un triple fault.

## Interrupt Stack Frame

Quando un interrupt si verifica, la CPU pusha automaticamente:

```
┌─────────────────┐
│ SS (User Stack)  │  Solo per Ring 3 → Ring 0
│ RSP             │
│ RFLAGS          │
│ CS (Code Seg)   │
│ RIP (Return)    │
│ Error Code      │  Solo per alcune eccezioni
└─────────────────┘
```

## Riferimenti

- [Writing an OS in Rust - Interrupts](https://os.phil-opp.com/diving-in/)
- [OSDev Wiki - PIC](https://wiki.osdev.org/PIC)
- [OSDev Wiki - PIT](https://wiki.osdev.org/Programmable_Interval_Timer)
- [OSDev Wiki - PS/2 Keyboard](https://wiki.osdev.org/PS/2_Keyboard)
- [Intel SDM - Chapter 6: Interrupts](https://www.intel.com/content/www/us/en/developer/articles/technical/intel-sdm.html)
