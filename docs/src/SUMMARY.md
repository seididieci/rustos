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
- [Performance](./13-performance.md) — Fasi 23/24/25 (baseline + throughput + cache)

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
- [ADR-0014: Diritti per-canale lato server](./adr/0014-channel-rights-serverside.md)
- [ADR-0015: POSIX come API di libr](./adr/0015-posix-api-libr-protocollo-interno.md)
- [ADR-0016: FAT32 scrivibile](./adr/0016-fat-writable.md)
- [ADR-0017: Servizi caricati da disco](./adr/0017-servizi-da-disco.md)
- [ADR-0018: Cache settoriale write-through in userdisk](./adr/0018-sector-cache-userdisk.md)
- [ADR-0019: async/await in libr (piano, 4 passi)](./adr/0019-async-await-libr.md)
- [ADR-0020: Higher-half kernel + direct map](./adr/0020-higher-half-direct-map.md)
- [ADR-0021: Loader ELF per-segmento (W^X)](./adr/0021-elf-loader.md)