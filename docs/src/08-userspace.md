# User Mode

> **Aggiornamento Fase 12 (ADR-0008)**: init e i servizi user comunicano per
> **canale** (non per PID): `spawn` ritorna il canale di nascita verso il figlio,
> i servizi si registrano per nome (`service_register`). Le demo storiche
> `usersrv`/`usercli` (basate su IPC per PID dedotto) sono state rimosse dal
> catalogo binari. Vedi [`07-ipc.md`](./07-ipc.md).

## Panoramica

L'utente mode (Ring 3) è dove eseguono i programmi utente. Per entrare in Ring 3, il kernel deve:

1. Configurare GDT con segmenti utente
2. Configurare TSS con stack per Ring 0
3. Preparare lo stack utente
4. Eseguire SYRET o IRET per saltare a Ring 3

## Stato attuale (Fase 6)

User mode (ring 3) e entry syscall sono sviluppati nelle sotto-fasi della Fase 6
(dettagli in [`06-syscalls.md`](./06-syscalls.md)):

- **6.1 Infrastruttura**: GDT user segment (DPL 3), TSS `RSP0` dinamica, page table
  per-processo con separazione minima (kernel non accessibile dagli user), `CR3` nel
  context switch.
- **6.2 Entry Ring 3**: frame CPU user + trampoline `iretq`; primo processo in ring 3
  preemptato dal timer.
- **6.3 syscall/sysret**: MSR `STAR`/`LSTAR`/`SFMASK`, entry assembly (senza `swapgs`,
  accesso rip-relative a `PerCpu`), handler `getpid`/`write`/`exit`.
- **6.4 Embed binario utente**: crate freestanding incluso nel kernel, demo
  `getpid + write + busy-loop` con preemption in ring 3.
- **Fase 7 — IPC sincrona send/recv/reply**: syscall 16/17/18, messaggio registro-based
  nei registri (multi-parola via `ipc_override`), demo server/client in ring 3
  (dettagli in [`07-ipc.md`](./07-ipc.md)).
- **Fase 8.1 — init + syscall `spawn`**: il kernel spawna solo `init` (primo
  processo user, parent `None`); init e' l'**unico** che spawna i servizi user
  via la syscall `spawn` (20), registrandoli come figli (campo `parent` nel PCB).
  La process tree e' radicata in init. (Da Fase 12 spawn ritorna un canale di
  nascita e i servizi si registrano per nome.)
- **Fase 8.2 — Console server**: `userconsole` e' un processo user
  che mappa il frame buffer VGA (`0xB8000`) a `USER_VGA` (`0x4000_0010_0000`)
  tramite la syscall `map_physical` (21). Da Fase 15 e' solo rendering:
  pubblica `/dev/console` (DEV_WRITE disegna); tastiera in `userkbd`/`usertty`
  (sotto). `sys_write(fd=1)` stampa solo su seriale.

**Completate** le sotto-fasi **6.1** (infrastruttura: segmenti GDT user, TSS `RSP0`
dinamica aggiornata a ogni context switch, page table per-processo, `CR3` nel
context switch), **6.2** (entry ring 3: frame CPU user + trampoline `iretq`, primo
processo in ring 3 — stub `jmp $` — preemptato dal timer), **6.3** (meccanismo
syscall/sysret: MSR `STAR`/`LSTAR`/`SFMASK`, entry assembly senza `swapgs` — accesso
a `PerCpu` via `rip`-relative —, handler `getpid`/`write`/`exit`) e **6.4** (embed
binario utente: crate `userland/demo` freestanding PIC → binary raw incluso via
`include_bytes!`, caricato in `USER_CODE`; demo `getpid + write + busy-loop` che
mostra `pid=4` e `[demo] tick` — preemption in ring 3; ABI syscall con preservazione
di tutti i registri). **Completata anche la
Fase 7 — IPC sincrona send/recv/reply**: syscall 16/17/18 con messaggio registro-based,
demo server (`usersrv`) + client (`usercli`) in ring 3 che scambiano richieste/risposte
in loop (vedi [`07-ipc.md`](./07-ipc.md)). **Completata anche la Fase 8.1 — init +
syscall `spawn`** (numero 20): il kernel crea `init` come unico processo user; init
spawna tutti i servizi via `spawn(name)`, ogni figlio ha `parent = init`.
**Completata anche la Fase 8.2 — console server**: `userconsole`
mappa VGA via `map_physical` (syscall 21) a `USER_VGA` (da Fase 15 solo
rendering su `/dev/console`; tastiera in `userkbd`/`usertty`). **Completata
anche la Fase 8.3 — uptime
in userspace**: il processo `uptime` e' stato spostato dal kernel in userspace
come `useruptime` (syscall `get_ticks`, numero 22). Da Fase 15 il kernel non ha
piu' processi oltre `idle` (anche il driver tastiera e' in userspace, sotto).
Prossimo: **Fase 9 — File system server**.

## Ring Levels

```
Ring 0 (Kernel Mode)
  ├── Accesso completo a tutta la memoria
  ├── Tutte le istruzioni CPU disponibili
  └── Può eseguire IN/OUT per I/O

Ring 1-2 (Non usati)
  └── In x86_64, solo Ring 0 e Ring 3 sono usati

Ring 3 (User Mode)
  ├── Accesso limitato alla memoria (page tables)
  ├── Istruzioni privilegiate bloccate
  └── Deve usare system call per I/O
```

## GDT (Global Descriptor Table)

La GDT definisce i segmenti di memoria per Ring 0 e Ring 3:

```rust
use x86_64::structures::gdt::{GlobalDescriptorTable, Descriptor, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;

pub struct Gdt {
    gdt: GlobalDescriptorTable,
    kernel_code: SegmentSelector,
    kernel_data: SegmentSelector,
    user_code: SegmentSelector,
    user_data: SegmentSelector,
    tss: SegmentSelector,
}

impl Gdt {
    pub fn new(tss: &'static TaskStateSegment) -> Self {
        let mut gdt = GlobalDescriptorTable::new();
        
        // Kernel segments (Ring 0)
        let kernel_code = gdt.add_entry(Descriptor::kernel_code_segment());
        let kernel_data = gdt.add_entry(Descriptor::kernel_data_segment());
        
        // User segments (Ring 3)
        let user_code = gdt.add_entry(Descriptor::user_code_segment());
        let user_data = gdt.add_entry(Descriptor::user_data_segment());
        
        // TSS
        let tss = gdt.add_entry(Descriptor::tss_segment(tss));
        
        Self { gdt, kernel_code, kernel_data, user_code, user_data, tss }
    }
    
    pub fn load(&'static self) {
        self.gdt.load();
        unsafe {
            self.gdt.load();
            // Carica TSS
            x86_64::instructions::segmentation::CS::set_reg(self.kernel_code);
            x86_64::instructions::segmentation::DS::set_reg(self.kernel_data);
            x86_64::instructions::tables::load_tss(self.tss);
        }
    }
}
```

## TSS (Task State Segment)

Il TSS contiene lo stack pointer per Ring 0 (RSP0):

```rust
static TSS: TaskStateSegment = TaskStateSegment::new();

pub fn init_tss() {
    // Stack per Ring 0 (usato quando si entra da Ring 3)
    TSS.privilege_stack_table[0] = {
        const STACK_SIZE: usize = 4096 * 5;
        static mut STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];
        VirtAddr::from_ptr(unsafe { &STACK }) + STACK_SIZE
    };
    
    // IST per Double Fault
    TSS.interrupt_stack_table[0] = {
        const STACK_SIZE: usize = 4096 * 5;
        static mut STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];
        VirtAddr::from_ptr(unsafe { &STACK }) + STACK_SIZE
    };
}
```

## Entrare in User Mode

### Preparazione stack utente

```rust
pub fn enter_user_mode(entry_point: VirtAddr, stack: VirtAddr) -> ! {
    unsafe {
        let user_cs = SegmentSelector::new(3, Ring::Ring3);  // User code
        let user_ds = SegmentSelector::new(4, Ring::Ring3);  // User data
        
        asm!(
            // Imposta segment registers
            "mov ds, {user_ds}",
            "mov es, {user_ds}",
            "mov fs, {user_ds}",
            "mov gs, {user_ds}",
            
            // Push user stack
            "push {user_ds}",
            "push {stack}",
            
            // Push RFLAGS (con interrupts abilitati)
            "push 0x202",
            
            // Push user code
            "push {user_cs}",
            
            // Push entry point
            "push {entry}",
            
            // IRET (torna a Ring 3)
            "iretq",
            
            user_ds = in(reg) user_ds.as_u64(),
            stack = in(reg) stack.as_u64(),
            user_cs = in(reg) user_cs.as_u64(),
            entry = in(reg) entry_point.as_u64(),
            options(noreturn)
        );
    }
}
```

## ELF Loader

Per caricare programmi utente, dobbiamo parsare ELF64:

```rust
use xmas_elf::ElfFile;

pub fn load_elf(data: &[u8]) -> Result<LoadedProgram, LoadError> {
    let elf = ElfFile::new(data)?;
    
    let mut program = LoadedProgram {
        entry_point: elf.header.pt2.entry_point() as VirtAddr,
        page_table: create_user_page_table(),
    };
    
    // Carica tutti i segmenti PT_LOAD
    for ph in elf.program_iter() {
        if ph.get_type() == Ok(Type::Load) {
            let vaddr = VirtAddr::new(ph.virtual_addr());
            let mem_size = ph.mem_size() as usize;
            let file_size = ph.file_size() as usize;
            
            // Mappa pagine con permessi utente
            let flags = PageTableFlags::PRESENT
                | PageTableFlags::USER_ACCESSIBLE;
            
            if ph.flags().is_writable() {
                flags |= PageTableFlags::WRITABLE;
            }
            
            // Alloca frame e mappa
            for page in pages {
                let frame = frame_allocator.allocate_frame().unwrap();
                mapper.map_to(page, frame, flags, frame_allocator)?;
            }
            
            // Copia dati dal file
            if file_size > 0 {
                let dest = unsafe {
                    core::slice::from_raw_parts_mut(
                        vaddr.as_mut_ptr(),
                        file_size
                    )
                };
                dest[..file_size].copy_from_slice(&ph.raw_data(&elf)?[..file_size]);
            }
        }
    }
    
    Ok(program)
}
```

## Processo Init (Fase 8.1)

Il primo processo user è **`init`** (`userland/init`, binario `userinit.bin`),
creato dal kernel a boot tramite `user_binary::spawn_init()` con parent `None`
(è la radice della process tree):

```rust
// kernel/src/user_binary.rs (semplificato)
pub fn spawn_init() -> usize {
    spawn_user("userinit", sched::Priority::Normal, userinit_phys(), userinit_frames(), None)
}
```

Il binario di init è **embedded nel kernel** via `include_bytes!` come gli altri
processi user — il filesystem arriverà solo in Fase 9.

`init` è l'**unico** processo che spawna i servizi user. Usa la syscall
`spawn(name)` (numero 20) per creare i servizi come suoi figli. Dalla **Fase 12**
(ADR-0008) `spawn` crea il **canale di nascita** tra init e il figlio (il figlio
lo usa come canale 0 = parent) e ritorna il channel id. L'ordine di spawn resta
importante: `userconsole` per primo (+ attesa READY), poi `userdisk` (+ attesa
READY, Fase 16), poi `userfs` (+ attesa READY), poi uptime, `userdevfs`
(+ attesa READY), i test in sequenza e la shell
per ultima. I READY sono fire-and-forget via `send_async` (consumati senza
reply): una `send` sync resterebbe bloccata perché a boot init non aspetta
console/devfs (e console registra `/dev/input` solo dopo userfs: attendere
dopo sarebbe deadlock).

Dalla **Fase 14 (init-restart)** init è anche **supervisore**: console, disk,
fs, devfs, kbd e tty vengono riavviati alla morte (tabella bin/servizio/chan/pid + loop su
`EXIT_NOTIFY`, condiviso con l'attesa dei test così i restart funzionano anche
a suite in corso). Backoff anti spawn-storm (20 tick prima di ogni tentativo;
oltre 3 restart in 300 tick → hold + log). Shell/uptime/test: log-only.

## Differenze Ring 0 vs Ring 3

| Feature | Ring 0 (Kernel) | Ring 3 (User) |
|---------|-----------------|---------------|
| Istruzioni | Tutte | Solo non privilegiate |
| Memoria | Tutta | Solo pagine USER_ACCESSIBLE |
| I/O Port | IN/OUT liberi | IN/OUT bloccati |
| MSR | Tutti | Solo quelli concessi |
| Interrupt | Tutti | Gestiti dal kernel (IDT) |

## Prossime evoluzioni (Fasi 14-17)

Il modello "processi userspace + IPC per nome + TSS con I/O bitmap" si estende
in tre direzioni pianificate (vedi `AGENTS.md`):

- **Fase 14 — Cleanup processi** ([ADR-0010](./adr/0010-process-lifecycle-cleanup.md)):
  `exit`/`kill` kernel-side con cleanup differito (stack/TSS/CR3/page table/
  ring/heap), slot a generazioni, **notifica exit al parent** sul canale di
  nascita (consente a init di riavviare i servizi) e cascata sulla discendenza.
- **Fase 15 — Keyboard + Terminal server in userspace** (implementata,
  [ADR-0011](./adr/0011-userspace-keyboard-terminal.md)): `userkbd` (driver PS/2
  in ring 3, `io_ranges 0x60-0x64`, `/dev/kbd`, servizio `Kbd` svegliato da
  IRQ1) + `usertty` (decode, echo, `/dev/input/keyboard`, servizio `Tty`,
  client FS puramente async ed event-driven); console ridotto a rendering
  (`/dev/console`). Regole: mai IPC sincrone servendo, mai spinner, boot
  async senza attese di wake, handshake per canale.
- **Fase 16 — Disk/ATA server in userspace** (implementata,
  [ADR-0012](./adr/0012-userspace-disk-driver.md)): `userdisk` (driver ATA in
  ring 3, entrambi i canali via `io_ranges`, enumerazione IDENTIFY + MBR,
  `/dev/sdX`, servizio `Disk`) + `userfs` senza porte ne' codice ATA (parser
  FAT32 generico su `BlockSource`, client `DISK_*` con riconnessione lazy).
  Regole: mai sync incrociate tra server (registrazione async), mai throttle
  senza waker, consumer SPSC a `tail`, `ring_alloc` a coppie fresche.
  Fase 16c: mappa nome→handle di proprieta' del driver (`DISK_RESOLVE` 0x54,
  tag centralizzati in `syscall-numbers`) — userfs chiede, non indovina;
  userdisk unico owner di `Disk` (SATA futuro come backend interno).
- **Fase 17 — Shell + utility utente** (slittata in coda).

## Riferimenti

- [Writing an OS in Rust - Testing](https://os.phil-opp.com/testing/)
- [OSDev Wiki - User Mode](https://wiki.osdev.org/Ring_3)
- [Intel SDM - Privilege Levels](https://www.intel.com/content/www/us/en/developer/articles/technical/intel-sdm.html)
