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
3. [x] Ogg Opus (`vmkv-parser-opus`, con su propio demuxer Ogg: paquetes con extents, granule position, BOS/EOS). Cubre tiempos negativos, `codec_delay_ns`, paquetes partidos entre páginas y `discard_padding_ns`. Vorbis y FLAC-Ogg partirán de una copia de ese demuxer (decisión 30).
4. [x] FLAC nativo (`vmkv-parser-flac`): frames delimitados por CRC-16 y cabecera siguiente con el número esperado, bloque fijo o variable, ID3v2/ID3v1 (decisión 58). Verificado con `ffprobe` y remux a MKV en los fixtures y en música real (niveles 0, 8 y 12; 6 canales a 96 kHz).
5. [x] AC-3 y E-AC-3 (`vmkv-parser-ac3`): syncframes con CRC, códec según `bsid` (decisión 59). Verificado con remux a MKV en los fixtures y en pistas reales AC-3 2.0/5.1 y E-AC-3 5.1. Pendiente: substreams dependientes de E-AC-3 (7.1), sin muestra real.
6. [x] PCM en WAV/RF64/BW64 (`vmkv-parser-pcm`): `A_PCM/INT/LIT` y `A_PCM/FLOAT/IEEE`, unidades de 40 ms (decisión 60). Verificado contra el flujo PCM de un remux a MKV y contra los WAV de prueba de scipy.
7. [x] Vorbis en Ogg (`vmkv-parser-vorbis`): duraciones por modo de bloque, granule positions comprobadas en cada página (decisión 61). Verificado con remux a MKV en los fixtures y en música real con dos codificadores.

## Fase 2. Subtítulos

- [x] SRT (`vmkv-parser-srt`): `duration_required`, huecos entre unidades, transcodificación a UTF-8 como `inline` (decisión 39). Probado con `testdata/media/srt_sample.srt` (golden) y `vtj-stress` (1000+ variantes, 0 problems).

## Fase 3. Vídeo

1. [x] H.264 Annex B (`vmkv-parser-h264`): scanner Annex B, NAL units, ensamblador de Access Units (decisión 54), estado de parameter sets, POC/timing, unit. Un frame Matroska es un Access Unit y puede llevar varios NAL. Un frame no es un NAL.
2. [x] Frames B: las líneas salen en orden de archivo, pero `pts` y `duration` dependen del orden de presentación (POC, decisión 55). Se acumulan las units en memoria y se escribe en una segunda pasada. Probado con `testdata/media/h264_sample.h264` (real, generado con `ffmpeg`/libx264 esta sesión: I + 7 P + 12 B, `pic_order_cnt_type` 0) — el orden de presentación resultante coincide exactamente con el de `ffprobe`, y `codec_private` coincide byte a byte con el `avcC` que genera `ffmpeg -c:v copy -f mp4` del mismo archivo. `vtj-stress`: 1000+ variantes, 0 problems. Fuera de alcance por ahora (decisiones 56-57): `pic_order_cnt_type` 1, entrelazado, FMO, slices redundantes, croma 4:2:2/4:4:4, más de un SPS/PPS activo por pista. Esta abstracción (reordenar por POC en una segunda pasada) se comparte con HEVC.
3. HEVC: pendiente.

## Fase 4. Casos difíciles

- Multi-fuente (WavPack + `.wvc`) y varias pistas por archivo (TrueHD con núcleo AC-3: una ejecución por pista).
- `block_additions`, `colour`/HDR, `projection`.

## Fase 5. Verificación cruzada

- No comparar el `payload` con los bytes crudos de la fuente: en H.264 y ADTS cambian.
- Método: remuxar con `ffmpeg -c copy` a `.mkv`, hacer `ffprobe -show_packets -show_data` sobre ese MKV (ya en el mapping de Matroska) y comparar con el payload reconstruido desde el `.vtj`.
- Comparar tiempos relativos: FFmpeg puede desplazar tiempos de inicio (`-copyts`) y trata el pre-skip de Opus a su manera. Opus se revisa a mano.
- [x] H.264: variante del método anterior, con `ffmpeg`/`ffprobe` instalados sin `apt` (binario estático, sin red administrada por el sistema) esta sesión. En vez de un MKV, remux a `.mp4` con `-c:v copy` y comparación byte a byte de la caja `avcC` contra el `codec_private` del `.vtj` (coincide exactamente); orden de presentación (`pict_type` por `ffprobe -show_frames`) comparado contra el resultado de ordenar por POC. Ancho/alto/perfil/nivel/frame rate de `ffprobe -show_streams` contra los mismos campos de la SPS, también exactos.
- Corpus real con archivos truncados y corruptos: deben fallar con el código de error correcto.
- [x] `vtj-stress`: variantes truncadas y mutadas de cada entrada, con oráculo del contrato. Hay un test `robustness` por parser en CI. Se han probado 138 000 variantes de los fixtures y 46 800 de 52 MP3 reales, sin problemas. Ampliado a 65 MP3 reales más (`testdata/media`): 19 500 variantes adicionales (`--all-kinds --max-variants 300 --repeat`), 0 problems.

## Dificultad estimada

- Sencillos: MP3, SRT, AAC/ADTS.
- Medios: Ogg Opus (demuxer Ogg, pre-skip, `discard_padding_ns`).
- Difíciles, con revisión contra el estándar y `ffprobe`: H.264 (hecho), HEVC, multi-fuente, TrueHD, `block_additions`, HDR.
- Lo que decide si sale bien es la red de pruebas (validador, tests dorados, verificación cruzada), no escribir el parser.

## Fuera de alcance por ahora

- El planner y la construcción del MKV desde VMKV Track IR.
