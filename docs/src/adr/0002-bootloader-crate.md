# ADR-0002: Use bootloader crate

## Status

Superseded by [ADR-0004](./0004-custom-multiboot-boot.md) — il boot avviene via
protocollo PVH con stub custom, senza il crate `bootloader`.

## Context

Per avviare un kernel bare-metal, è necessario un bootloader che:
1. Gestisca l'avvio da BIOS o UEFI
2. Imposti la CPU in Long Mode (64-bit)
3. Carichi il kernel dalla disk image
4. Fornisca informazioni sulla memoria (memory map)

Le opzioni disponibili sono:
- **GRUB**: richiede installazione separata, configurazione, e non è ideale per development
- **Custom bootloader**: complesso da implementare, molti pitfall
- **`bootloader` crate**: soluzione Rust completa, gestisce tutto

## Decision

Usare il crate `bootloader` (versione 0.11) come bootloader per Velordor.

```toml
[dependencies]
bootloader = "0.11"
```

Il crate fornisce:
- Boot stub in assembly per BIOS e UEFI
- Creazione automatica di disk image avviabili
- Memory map e informazioni di boot
- Stack setup e inizializzazione iniziale

## Consequences

### Positive

- Nessuna dipendenza esterna (GRUB non installato sul sistema)
- Tutto è embeddato nel progetto Rust
- Supporto BIOS e UEFI con lo stesso codice
- Il crate gestisce la transizione Real Mode → Protected Mode → Long Mode
- Fornisce `BootInfo` con memory map, framebuffer, e altre info utili
- Testabile con QEMU senza configurazione complessa

### Negative

- Dipendenza da un crate esterno (ma è il più usato nella community Rust)
- La versione 0.11 potrebbe avere bug (ma è stabile e testata)
- Meno controllo sul processo di boot rispetto a un custom bootloader

### Neutral

- Il crate è stato creato da Philipp Oppermann, nell'ambito del progetto
  "Writing an OS in Rust"
- È lo stesso approccio usato da molti progetti OS in Rust
- Il kernel Linux non lo usa (usa GRUB o altri bootloader), ma il crate e'
  adatto a un kernel Rust che vuole un boot standard senza gestire il
  bootstrap manualmente

## Alternatives Considered

- **GRUB**: Richiederebbe `sudo dnf install grub2-common`, configurazione di `grub.cfg`, e un processo di build più complesso. Incompatibile con il flusso `cargo run`.

- **Custom bootloader**: Implementare da zero la transizione di modalità CPU, A20 line, loading del kernel. Istruttivo ma complesso e soggetto a errori: il focus di design resta sul kernel, non sul bootloader.

- **Limine**: Bootloader moderno ma richiede configurazione esterna e non è Rust-native.

## References

- [bootloader crate](https://crates.io/crates/bootloader)
- [Writing an OS in Rust - Booting](https://os.phil-opp.com/booting/)
- [OSDev Wiki - Bootloaders](https://wiki.osdev.org/Bootloaders)
