# vmkv-parsers

Parsers de códec que describen una pista con el formato **VMKV Parser Output v1**
(`.vtj`, JSON Lines). Cada parser dice qué frames hay, cuándo se presentan y dónde están
sus bytes en el archivo original, sin copiar los datos.

- Spec: [`docs/spec/VMKV_Parser_Output_Format_v1.md`](docs/spec/VMKV_Parser_Output_Format_v1.md)
- Por qué el spec dice lo que dice: [`docs/spec/DECISIONS.md`](docs/spec/DECISIONS.md)
- Roadmap: [`docs/ROADMAP.md`](docs/ROADMAP.md)

## Crates

| Crate | Qué es |
| --- | --- |
| `crates/vtj` | Librería común: tipos, writer canónico, aritmética de tiempos exacta, checks, validador y contrato CLI de parsers |
| `crates/vtj-validate` | Binario `vtj-validate`: validador estructural y `--codec-aware` |
| `crates/vmkv-parse` | Lanzador `vmkv-parse <códec>`: ejecuta `vmkv-parser-<códec>` |
| `crates/vtj-stress` | `vtj-stress`: ejecuta un parser sobre variantes dañadas de sus entradas y comprueba el contrato |
| `crates/parser-mp3` | `vmkv-parser-mp3`: MPEG-1/2/2.5 Layer III, con retardo y relleno de la etiqueta LAME. Opciones: [`docs/parsers/mp3.md`](docs/parsers/mp3.md) |
| `crates/parser-aac` | `vmkv-parser-aac`: AAC en ADTS, sin cabecera ADTS en el payload y con AudioSpecificConfig en `codec_private` |
| `crates/parser-opus` | `vmkv-parser-opus`: Opus en Ogg, con su propio demuxer Ogg, pre-skip, timeline negativo y recorte final |
| `crates/parser-flac` | `vmkv-parser-flac`: FLAC nativo, con los límites de frame hallados por CRC-16 y cabecera siguiente, y todos los bloques de metadatos en `codec_private` |
| `crates/parser-ac3` | `vmkv-parser-ac3`: AC-3 y E-AC-3 en bruto, con comprobación del CRC de cada frame. Opciones: [`docs/parsers/ac3.md`](docs/parsers/ac3.md) |
| `crates/parser-pcm` | `vmkv-parser-pcm`: PCM entero o float en WAV, RF64 y BW64, en unidades de 40 ms. Opciones: [`docs/parsers/pcm.md`](docs/parsers/pcm.md) |
| `crates/parser-vorbis` | `vmkv-parser-vorbis`: Vorbis en Ogg, con su propia copia del demuxer Ogg, la cabecera setup recorrida entera para conocer los modos, recorte inicial y final |
| `crates/parser-dts` | `vmkv-parser-dts`: núcleo DTS en palabras de 16 bits big-endian; DTS-HD y las demás variantes se rechazan |
| `crates/parser-srt` | `vmkv-parser-srt`: subtítulos SubRip, con `duration_required`, huecos entre cues y transcodificación a UTF-8 cuando la fuente no lo es |
| `crates/parser-h264` | `vmkv-parser-h264`: H.264 en Annex B, con su propio ensamblador de Access Units, `codec_private` AVCC y reordenado de frames B por picture order count (POC) |

## Binarios

Cada parser es un binario independiente, `vmkv-parser-<códec>`, con su propio crate y su
propia versión, que es la que figura en `parser.version`. Así, un fallo en un parser solo
tumba su proceso, y un parser puede estar escrito en cualquier lenguaje: el contrato es la
CLI más el formato `.vtj`.

El lanzador `vmkv-parse` busca `vmkv-parser-<códec>` primero junto a sí mismo y después
en el `PATH`, y le cede el proceso (`exec`). Por eso la salida estándar, los errores y el
código de salida son los del parser. Si el códec no existe, termina con 2 y no escribe
nada en la salida estándar.

```bash
vmkv-parse --list
vmkv-parse mp3 cancion.mp3 -o cancion.vtj
```

Con `-o`, la salida se escribe en un temporal junto al destino y se renombra al terminar,
y se rechaza (código 2) un destino que sea una de las entradas, también a través de
enlaces.

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

Cada parser es autónomo: no comparte lógica de códec ni de contenedor con otros parsers,
aunque eso la repita (por ejemplo, MP3 y AAC tienen cada uno su copia del salto de
etiquetas ID3/APE, y el demuxer Ogg vive dentro del parser de Opus). Así, un cambio solo
puede alterar la salida del parser cuya versión cambia (regla 8). `vtj` contiene únicamente
el formato y el contrato: tipos, writer, timing, validador y CLI.

Un parser es un crate con una librería (`struct` que implementa `vtj::Parser`, para testearla
en proceso) y un binario `vmkv-parser-<códec>` que llama a `vtj::cli::main`. Su
`parser.name` es el nombre del binario. El runner se encarga de:

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

### Parámetros

Cada parser detecta todo automáticamente, pero además declara parámetros para adaptarse a cualquier versión o variante de su códec. Hay tres tipos:

- **overrides**: fuerzan un valor en lugar de detectarlo;
- **políticas**: deciden qué hacer ante una desviación del estándar;
- **selecciones**: eligen qué flujo o rango describir.

Se declaran con `ParamSpec::int(nombre, min, max, ayuda)`, `ParamSpec::choice(nombre, &[...], ayuda)`, `ParamSpec::rational` o `ParamSpec::string`, con `.default("…")` para la ayuda. `Parser::check_params` valida las combinaciones. Solo los parámetros que se pasan quedan registrados en `header.params`.

## Tests

- **`testdata/golden/`:** los ejemplos del spec, completados con `header` y `end`.
  `tests/golden.rs` los reconstruye con la API y los compara byte a byte.
- **`tests/checklist.rs`:** al menos un caso negativo por cada punto del checklist.
- **`tests/contract.rs`:** el contrato CLI, incluida la doble ejecución (regla 8), las
  rutas de error y `params`.
- **`testdata/media/`:** archivos reales pequeños generados con FFmpeg (seno de 1 s).
  Las salidas de referencia de cada parser están en `testdata/golden/<códec>/`, y se
  regeneran con `UPDATE_GOLDEN=1 cargo test` tras un cambio intencionado. Los offsets y
  tamaños de los frames se contrastaron con `ffprobe -show_packets`, y además los bytes de
  cada payload y el `codec_private` se compararon con un remux a MKV (`ffmpeg -c copy`,
  `ffprobe -show_data`).

## Robustez (`vtj-stress`)

`vtj-stress` ejecuta el binario de un parser como subproceso sobre copias mutadas de cada entrada. Cada variante debe:

- terminar antes del timeout;
- salir con 0 y una salida de éxito válida, o con 1 y una salida de fallo bien formada;
- no producir errores `internal:`, que indican un fallo del parser detectado por el writer;
- con `--repeat`, dar la misma salida dos veces.

Todo lo demás se reporta como problema (pánico, señal, cuelgue, salida inválida...), junto con el comando exacto para reproducirlo.

```bash
cargo build --release
# truncado en cada byte
./target/release/vtj-stress testdata/media/*.mp3
# todos los tipos, 1500 posiciones aleatorias, flips de un bit, ediciones de 4 bytes
./target/release/vtj-stress --all-kinds --random-positions 1500 --bit-flips --len 4 \
    --random-count 2000 --random-edits 5 --seed 1 testdata/media/*
# solo la cola del archivo (etiquetas y últimos frames), cada 3 bytes
./target/release/vtj-stress --kinds truncate,delete --from -2048 --step 3 cancion.mp3
# una variante concreta, p. ej. la de un fallo reportado
./target/release/vtj-stress --variant flip@1234:0x80 --keep fallos/ cancion.mp3
```

Las variaciones se eligen con precisión:

- **Tipos**: `--kinds` (`truncate`, `flip`, `set`, `zero`, `delete`, `insert`, `dup`, `random`).
- **Rango**: `--from`/`--to`, con offsets absolutos, negativos contados desde el final o `P%`.
- **Posiciones**: `--step`, `--positions` o `--random-positions` con `--seed`.
- **Parámetros de cada edición**: `--len`, `--masks`/`--bit-flips`, `--value` y `--random-count`/`--random-edits`.
- **Variantes exactas**: `--variant`, repetible.
- **Límite**: `--max-variants`.

`vtj-stress --help` lista todas las opciones, incluidas `--jobs`, `--timeout-ms`, `--fail-fast`, `--keep`, `--list` y `--json`.

Cada parser tiene un test `robustness` que ejecuta `vtj_stress::ci_suite()` sobre sus fixtures, unas 5 000–15 000 variantes, dentro de `cargo test`.

