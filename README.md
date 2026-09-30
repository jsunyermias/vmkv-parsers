# vmkv-parsers

Parsers de códec que describen una pista con el formato **VMKV Parser Output v1**
(`.vtj`, JSON Lines). Cada parser dice qué frames hay, cuándo se presentan y dónde están
sus bytes en el archivo original, sin copiar los datos.

- Spec: [`docs/spec/VMKV_Parser_Output_Format_v1.md`](docs/spec/VMKV_Parser_Output_Format_v1.md)
- Por qué el spec dice lo que dice: [`docs/spec/DECISIONS.md`](docs/spec/DECISIONS.md)
- Roadmap: [`docs/ROADMAP.md`](docs/ROADMAP.md) (fase 0 completa; aún no hay parsers de códec)

## Crates

| Crate | Qué es |
| --- | --- |
| `crates/vtj` | Librería común: tipos, writer canónico, aritmética de tiempos exacta, checks, validador y contrato CLI de parsers |
| `crates/vtj-validate` | Binario `vtj-validate`: validador estructural y `--codec-aware` |

## Comandos

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

cargo run -p vtj-validate -- --codec-aware testdata/golden/ogg_opus.vtj
cargo run -p vtj-validate -- --source 0=testdata/golden/srt_source.srt testdata/golden/srt.vtj
```

Códigos de salida de `vtj-validate`: 0 éxito válido, 1 inválido, 2 error de uso o E/S,
3 salida de fallo bien formada.

## Escribir un parser

Un parser implementa `vtj::Parser` y llama a `vtj::cli::main`. El runner se encarga de:

- la CLI común (`-o`, `--<param>`, `--help`, `--version`);
- abrir y hashear las fuentes;
- escribir el `header` con los `params`;
- escribir `track` y `end`, o la línea `error`;
- los códigos de salida.

El parser nunca escribe JSON a mano.

```rust
use vtj::*;

struct Mp3;

impl Parser for Mp3 {
    fn name(&self) -> &'static str { "mp3-parser" }
    fn version(&self) -> &'static str { env!("CARGO_PKG_VERSION") }

    fn parse(&self, ctx: &mut Context<'_>) -> Result<Track, ParseError> {
        let rate = Rational::new(44100, 1);
        let mut timeline = Timeline::new(rate, 0)?;      // posición exacta en muestras
        for (offset, len) in [(2048, 418), (2466, 417)] { // en un parser real: el escáner de frames
            let (pts, dur) = timeline.advance(1152)?;     // regla 2 y regla 3
            let flags = Flags::NONE.with(Flag::RandomAccess);
            ctx.emit(&Unit::new(pts, dur, flags, vec![Chunk::src(0, offset, len)]))?;
        }
        let mut track = Track::new(TrackType::Audio, "A_MPEG/L3");
        track.audio = Some(Audio::new(rate, 2));
        Ok(track)
    }
}

fn main() { vtj::cli::main(&Mp3) }
```

Utilidades de tiempo:

- `ticks_to_ns(ticks, rate)` y `round_ns(p, q)` implementan la fórmula normativa con
  aritmética de 128 bits.
- `Timeline` sirve para unidades en orden de presentación.
- `durations_from_pts` sirve para unidades en orden de decodificación distinto del de
  presentación (frames B).

Los streams sin tiempos declaran `vtj::cli::FRAME_RATE` en `params()` y fallan con
`ErrorCode::TimingRequired` si no se pasa.

## Tests

- **`testdata/golden/`:** los ejemplos del spec, completados con `header` y `end`.
  `tests/golden.rs` los reconstruye con la API y los compara byte a byte.
- **`tests/checklist.rs`:** al menos un caso negativo por cada punto del checklist.
- **`tests/contract.rs`:** el contrato CLI, incluida la doble ejecución (regla 8), las
  rutas de error y `params`.
