# Summary

[Introduzione](./00-introduzione.md)

---

# Architettura

- [Panoramica Architettura](./01-architettura.md)

# Componenti

- [Boot Process](./02-boot-process.md) — Fasi 1-2
- [Interrupt Handling](./03-interrupts.md) — Fase 3
- [Memory Management](./04-memory.md) — Fase 4
- [Process Scheduler](./05-scheduler.md) — Fase 5
- [System Calls](./06-syscalls.md) — Fase 6
- [IPC](./07-ipc.md) ⭐ — Fase 7
- [User Mode](./08-userspace.md) — Fase 8
- [File System](./09-filesystem.md) — Fase 9
- [RT Scheduler + CBS](./10-scheduler-rt-cbs.md) — Fase 11
- [Test Suite](./11-testing.md) — Fase 9.5
- [Utilities](./12-utilities.md) — Fase 18 (shell + utility utente)

---

# Decisioni Architetturali

- [ADR-0001: Use Rust Nightly](./adr/0001-use-rust-nightly.md)
- [ADR-0002: Use bootloader crate](./adr/0002-bootloader-crate.md)
- [ADR-0003: VGA Text Mode](./adr/0003-vga-text-mode.md)
- [ADR-0004: Boot via PVH con stub custom](./adr/0004-custom-multiboot-boot.md)
- [ADR-0005: Architettura Microkernel](./adr/0005-microkernel-architecture.md)
- [ADR-0006: TSS per-processo con I/O bitmap](./adr/0006-per-process-tss.md)
- [ADR-0007: Scheduler RT + CBS](./adr/0007-rt-scheduler-cbs.md)
- [ADR-0008: IPC per nome — registry + channel](./adr/0008-ipc-by-name-channels.md)
- [ADR-0009: IPC asincrono — request-id interno](./adr/0009-async-ipc.md)
- [ADR-0010: Process lifecycle — cleanup kernel-side](./adr/0010-process-lifecycle-cleanup.md)
- [ADR-0011: Tastiera e terminale in userspace](./adr/0011-userspace-keyboard-terminal.md)
- [ADR-0012: Disk driver ATA in userspace](./adr/0012-userspace-disk-driver.md)
- [ADR-0013: Mount espliciti in userspace](./adr/0013-mount-syscall.md)
