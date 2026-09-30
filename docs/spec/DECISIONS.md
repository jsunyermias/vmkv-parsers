# Decisiones de interpretación del spec v1

Registro de los puntos que el borrador del spec dejaba abiertos o contradictorios. Todas las
decisiones están cerradas e incorporadas al texto de
[`VMKV_Parser_Output_Format_v1.md`](VMKV_Parser_Output_Format_v1.md), que es la fuente
normativa. Esta página solo explica el porqué de cada una.

| # | Decisión | Por qué |
| --- | --- | --- |
| 1 | `type` es siempre el primer campo | Todos los ejemplos lo hacían así; las tablas no lo listaban |
| 2 | `sources[]` en el orden `id`, `size`, `sha256`, `path`; se corrigió el ejemplo del header | La regla de orden por tablas es normativa; el ejemplo la contradecía |
| 3 | Claves de `params` en orden ascendente de bytes; valores entero, racional o cadena | Sin un orden fijo no se puede cumplir la regla 8 |
| 4 | Tabla de campos de `colour` y `mastering` en el orden de Matroska | El spec solo los describía en prosa |
| 5 | Nuevo tipo común Real: el decimal más corto que vuelve al mismo `f64`, sin exponente y con `-0` como `0` | `mastering` y los ángulos de `projection` no son enteros, y hace falta una forma canónica |
| 6 | `projection.private` y `extra_data` son cadenas de datos | Igual que `codec_private`: pueden apuntar a la fuente |
| 7 | Los valores por defecto y las listas vacías se omiten; el base64 es canónico | Una sola forma válida por valor |
| 8 | Un campo desconocido invalida la salida; las ampliaciones cambian `version` | Misma lógica que las marcas: los errores tipográficos no deben pasar inadvertidos |
| 9 | `sources` en orden de `id` estrictamente creciente | Lo exige la serialización canónica y además garantiza ids únicos |
| 10 | `video` y `audio` solo con su `track_type` | Evita datos de pista contradictorios |
| 11 | Restricciones de valor añadidas al checklist (retardos ≥ 0, tamaños > 0, recorte) | Valores fuera de rango que Matroska no admite |
| 12 | El `code` de `error` debe ser uno de los de la tabla | Mismo motivo que 8 |
| 13 | Un trozo `src` sin header es inválido | No existe la fuente a la que apunta |
| 14 | Nuevo código `SOURCE_UNREADABLE` para errores de E/S de la fuente; una lectura corta sigue siendo `TRUNCATED_BITSTREAM` | Ningún código existente describía un fallo de E/S; `UNREPRESENTABLE_IN_VMKV` era engañoso |
| 15 | Los parsers de referencia no escriben `sources[].path` | Dependería de cómo se invoque el parser y rompería la regla 8 |
| 16 | MP3: la trama Xing/Info se salta; con etiqueta LAME, retardo del encoder + 529 en `codec_delay_ns` y relleno final − 529 en `discard_padding_ns` de los últimos frames; sin etiqueta LAME no se inventa nada | Regla 4 (tiempos reales, como Opus) y regla 7 (no inventar) |
| 17 | Un binario por parser, `vmkv-parser-<códec>`, más el lanzador `vmkv-parse` al estilo de `git` | Aislamiento de fallos entre parsers, parsers en cualquier lenguaje (el contrato es CLI + `.vtj`) y versiones independientes |
| 18 | CRC de la etiqueta LAME: se acepta tanto la calculada sobre los bytes previos al campo como la de FFmpeg (190 bytes, con el campo a cero y relleno con ceros) | En MPEG-1 estéreo coinciden; en mono o MPEG-2, FFmpeg usa la segunda. Con cualquier otra CRC no se usa la etiqueta (regla 7) |
| 19 | Si la trama Xing declara un número de frames distinto del real, se ignora el relleno LAME, pero se mantiene el retardo | El archivo fue recortado o concatenado: el relleno ya no describe su final, mientras que el inicio sigue siendo válido |
| 20 | `vmkv-parser-mp3` solo acepta Layer III; Layer I/II fallan con `UNSUPPORTED_CODEC_VARIANT` | Son otro `codec_id` (`A_MPEG/L1`, `A_MPEG/L2`) |
| 21 | La trama VBRI se salta sin retardo | Su campo de retardo no es fiable y FFmpeg tampoco lo usa |
| 22 | ADTS: sin `codec_delay_ns` ni `output_sampling_frequency` | ADTS no indica el retardo del encoder ni si hay SBR implícito; no se inventan (regla 7) |
| 23 | ADTS: solo un raw data block por frame; la configuración de canales 0 (PCE) se rechaza con `UNSUPPORTED_FEATURE` | Varios bloques sin CRC no tienen límites conocidos sin decodificar, y un PCE tendría que extraerse del payload para ir en el AudioSpecificConfig |
| 24 | ADTS: la CRC no se verifica | Cubre bits del raw data block que solo se conocen decodificando; la sincronía y la longitud de cada frame ya detectan la corrupción de estructura |
| 25 | Ogg: un solo flujo lógico; los flujos multiplexados o encadenados fallan con `UNSUPPORTED_FEATURE` | Un `.vtj` describe una pista; elegir flujo o encadenar cambios de parámetros queda para cuando haga falta |
| 26 | Ogg: se verifican la CRC y la secuencia de cada página, y la coherencia del flag de continuación | Una página perdida o corrupta pierde datos: se falla antes que inventar (regla 7) |
| 27 | Opus: `sampling_frequency` es 48000 y `seek_preroll_ns` 80 ms; la frecuencia de entrada de OpusHead se ignora | Opus siempre decodifica a 48 kHz; son los valores del mapping de Matroska |
| 28 | Opus: la posición granular de cada página se contrasta con las duraciones de los TOC; solo la página EOS puede quedarse corta (recorte final); el recorte se reparte entre los paquetes de esa página y no puede ir más atrás de su inicio | Un desajuste en otra página indica un flujo corrupto; un recorte que llegara a páginas anteriores contradiría la posición granular de la página previa |
| 29 | Opus: si la primera página de audio es EOS y su posición granular es menor que lo que completa, el audio empieza en 0 y la diferencia es recorte final | Es el caso que RFC 7845 permite para flujos muy cortos |
| 30 | Cada parser es autónomo: no comparte lógica de códec ni de contenedor (salto de etiquetas, demuxer Ogg) con otros parsers, aunque la repita. `vtj` solo contiene formato y contrato | La salida de un parser depende de su `parser.version` (regla 8); una lógica compartida cambiaría la salida de varios parsers sin que ninguno cambie de versión |
| 31 | `discard_padding_ns` es `fin del unit − max(fin audible, pts_ns)`, con el fin audible redondeado una vez; un relleno más largo que un unit se reparte hacia atrás (`vtj::trim_end`) | Regla 3 aplicada al relleno: un unit que es relleno entero descarta exactamente su duración y `pts + duración − relleno` da el fin audible exacto. Los 11 MP3 LAME reales probados declaraban ~1684 muestras de relleno (1155 tras restar 529, más que un frame) |

## Convenciones de la implementación (fuera del formato)

- Códigos de salida de un parser: 0 éxito; 1 fallo de parseo (se escribió `error`);
  2 error de uso (no se escribe nada); 3 no se pudo escribir la salida.
- Códigos de salida de `vtj-validate`: 0 éxito válido; 1 inválido; 2 error de uso o E/S;
  3 salida de fallo bien formada.
- Una violación del formato por el propio parser la detecta el writer y se emite como
  `UNREPRESENTABLE_IN_VMKV` con un mensaje que empieza por `internal:`.
- Un parámetro racional se pasa como `N/D` o `N` (equivale a `N/1`) y se guarda sin
  reducir.
- `durations_from_pts` da duración 0 a las unidades que comparten `pts_ns`, salvo a la
  última. Se revisará con H.264 real en la fase 3.
