# Roadmap: parsers VMKV (salida `.vtj` v1)

Sep 30, 2026 · @Jo

Objetivo: escribir los parsers de cada códec con una salida estandarizada ([`spec/VMKV_Parser_Output_Format_v1.md`](spec/VMKV_Parser_Output_Format_v1.md)). El planner que construye el MKV queda fuera de alcance por ahora.

Lenguaje de implementación: Rust. Decisiones de interpretación del spec: [`spec/DECISIONS.md`](spec/DECISIONS.md).

## Fase 0a. Cerrar el spec (antes de escribir código)

- [x] Gramática de error: éxito `header unit* track end`; fallo `header? unit* error`. `error` siempre es la última línea. (`validate.rs`, `writer.rs`)
- [x] Timing externo (`TIMING_REQUIRED`): el spec solo define el campo `params` del header. La forma de pasarlo (`--frame-rate 24000/1001`) está en el contrato común (`cli.rs`).
- [x] Regla 8: mismos archivos + mismos parámetros externos + misma versión = misma salida byte a byte. (`tests/contract.rs`)
- [x] Redondeo: fórmula normativa `floor((2·p·10^9 + q) / (2·q))`, con división con suelo y 128 bits. Tests con positivos, negativos y empates. (`time.rs`)
- [x] Serialización canónica: UTF-8 sin BOM, LF en todas las líneas, sin espacios, escapes mínimos, orden de campos normativo, mensajes de error deterministas. (`json.rs`, `types.rs`)
- [x] Checklist: línea `error`, sin `null`, `params`. (`check.rs`, `validate.rs`, `tests/checklist.rs`)
- [x] Resolver las cuestiones abiertas e incorporarlas al texto del spec (motivos en [`spec/DECISIONS.md`](spec/DECISIONS.md)).

## Fase 0b. Librería común

- [x] Tipos del IR (unit, track, source, extents, racionales) y writer canónico. Los parsers no escriben JSON a mano. (`types.rs`, `writer.rs`)
- [x] Aritmética exacta: racionales, redondeo normativo, duración por diferencia de tiempos redondeados (regla 3). (`time.rs`: `round_ns`, `ticks_to_ns`, `Timeline`, `durations_from_pts`)
- [x] Códigos de error y código de salida distinto de 0. (`ErrorCode`, `cli.rs`)
- [x] `vtj-validate` estructural (independiente de los parsers) y `vtj-validate --codec-aware` (mappings de Matroska por códec).
- [x] Contrato de parsers: CLI común, `params`, salida a fichero o stdout. (`cli.rs`)
- [x] Tests dorados con los ejemplos del spec y test de doble ejecución (regla 8). (`testdata/golden`, `tests/golden.rs`, `tests/contract.rs`)

## Fase 1. Audio simple

1. [x] MP3 (`vmkv-parser-mp3`) y el lanzador `vmkv-parse`. Trama Xing/Info y etiqueta LAME según la decisión 16.
2. [x] AAC en ADTS (`vmkv-parser-aac`). La longitud de la cabecera (7 o 9 bytes con CRC) se lee de la cabecera; nunca se asume 7.
3. Ogg Opus. Demuxer Ogg independiente del códec (entrega paquetes con extents, granule position, BOS/EOS). Cubre tiempos negativos, `codec_delay_ns`, paquetes partidos entre páginas y `discard_padding_ns`. Vorbis y FLAC-Ogg salen después casi gratis.

## Fase 2. Subtítulos

- SRT: `duration_required`, huecos entre unidades, transcodificación a UTF-8 como `inline`.

## Fase 3. Vídeo

1. H.264 Annex B con esta cadena: scanner Annex B, NAL units, ensamblador de Access Units, estado de parameter sets, POC/timing, unit. Un frame Matroska es un Access Unit y puede llevar varios NAL. Un frame no es un NAL.
2. Frames B: las líneas salen en orden de archivo, pero `pts` y `duration` dependen del orden de presentación (POC). Se acumulan las units en memoria y se escribe en una segunda pasada. Esta abstracción se comparte con HEVC.
3. HEVC.

## Fase 4. Casos difíciles

- Multi-fuente (WavPack + `.wvc`) y varias pistas por archivo (TrueHD con núcleo AC-3: una ejecución por pista).
- `block_additions`, `colour`/HDR, `projection`.

## Fase 5. Verificación cruzada

- No comparar el `payload` con los bytes crudos de la fuente: en H.264 y ADTS cambian.
- Método: remuxar con `ffmpeg -c copy` a `.mkv`, hacer `ffprobe -show_packets -show_data` sobre ese MKV (ya en el mapping de Matroska) y comparar con el payload reconstruido desde el `.vtj`.
- Comparar tiempos relativos: FFmpeg puede desplazar tiempos de inicio (`-copyts`) y trata el pre-skip de Opus a su manera. Opus se revisa a mano.
- Corpus real con archivos truncados y corruptos: deben fallar con el código de error correcto.

## Dificultad estimada

- Sencillos: MP3, SRT, AAC/ADTS.
- Medios: Ogg Opus (demuxer Ogg, pre-skip, `discard_padding_ns`).
- Difíciles, con revisión contra el estándar y `ffprobe`: H.264, HEVC, multi-fuente, TrueHD, `block_additions`, HDR.
- Lo que decide si sale bien es la red de pruebas (validador, tests dorados, verificación cruzada), no escribir el parser.

## Fuera de alcance por ahora

- El planner y la construcción del MKV desde VMKV Track IR.
