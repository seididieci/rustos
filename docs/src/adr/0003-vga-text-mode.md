# ADR-0003: VGA Text Mode

## Status

Accepted

## Context

Per visualizzare output video all'avvio del kernel, è necessario un driver display. Le opzioni per x86_64 sono:

1. **VGA Text Mode**: buffer a 80×25 caratteri, accessibile a 0xB8000
2. **VGA Graphics Mode**: pixel-level, più complesso
3. **Framebuffer**: high-resolution, richiede font rendering
4. **Serial output**: solo debug, nessun output video

VGA Text Mode è la scelta con il miglior rapporto costo/beneficio per le fasi
di bring-up, perché:
- È semplice da implementare (buffer di memoria mappato)
- Non richiede driver grafici complessi (nessun font rendering, nessuna
  pipeline di presentazione)
- È sufficiente per l'output di testo e di debug della console
- Non richiede setup aggiuntivo: è il modo video di default della BIOS

## Decision

Usare VGA Text Mode come driver display principale per le prime fasi del progetto.

Il buffer VGA è mappato a 0xB8000 e ogni carattere occupa 2 byte:
```
Byte 0: ASCII character
Byte 1: Color attribute (foreground + background)
```

```rust
// Esempio di accesso al buffer VGA
const VGA_BUFFER: *mut Buffer = 0xB8000 as *mut Buffer;
```

## Consequences

### Positive

- Implementazione semplice e veloce
- Output immediato all'avvio del kernel
- Non richiede driver esterni
- Perfetto per debug e output di testo
- Funziona senza setup aggiuntivo (è il default della BIOS)

### Negative

- Limitato a 80×25 caratteri
- Solo 16 colori (4 bit per foreground, 4 per background)
- Nessun supporto grafico (immagini, font custom)
- Non funziona su sistemi UEFI senza CSM (Compatibility Support Module)

### Neutral

- Il buffer VGA è volatile (il compiler non deve ottimizzare gli accessi)
- Richiede un Mutex per l'accesso concorrente
- È il primo passo verso driver più complessi (framebuffer)

## Alternatives Considered

- **Serial output (COM1)**: Utile per debug ma nessun output video visibile. Usato come complemento, non come alternativa.

- **Framebuffer**: Permetterebbe high-resolution e font custom, ma richiederebbe:
  - Parsing delle informazioni framebuffer da UEFI/BIOS
  - Implementazione di un font renderer
  - Gestione di pixel-level rendering
  - Molto più complesso per un primo passo

- **VGA Graphics Mode**: Modo grafico a 320×200 pixel, più complesso di testo ma non significativamente migliore per output di testo.

## References

- [OSDev Wiki - VGA Hardware](https://wiki.osdev.org/VGA_Hardware)
- [Writing an OS in Rust - VGA Text Mode](https://os.phil-opp.com/vga-text-mode/)
- [VGA Text Mode - OSDev Wiki](https://wiki.osdev.org/VGA_Text_Mode)
