# VMKV Parser Output Format v1

Sep 30, 2026 · @Jo

## Qué es

Todo parser VMKV lee un archivo de un códec y escribe un archivo de texto con este formato. Si todos los parsers escriben lo mismo, el resto del sistema (el que construye el MKV) puede venir después sin tocarlos.

La salida describe una sola pista y contiene tres cosas:

- Qué archivos de origen se leyeron.
- Una línea por frame: cuándo se muestra y dónde están sus bytes en el archivo original.
- Los datos de la pista: códec, resolución o frecuencia, canales, etc.

El parser no copia los datos del vídeo o audio. Solo dice dónde están.

Las palabras DEBE, NO DEBE y PUEDE son obligación, prohibición y opción, respectivamente.

## Formato del archivo

La salida es JSON Lines: texto UTF-8, un objeto JSON por línea, sin saltos de línea dentro de un objeto. Extensión recomendada: `.vtj`.

Una salida válida sigue una de estas dos formas:

- Éxito: `header`, cero o más `unit` (una por frame), `track`, `end`.
- Fallo: `header` (opcional), cero o más `unit`, `error`.

En un fallo, el `header` PUEDE faltar: un error al abrir o leer la fuente puede ocurrir antes de poder escribirlo. La línea `error` siempre es la última.

La línea `track` va al final porque algunos datos de la pista solo se conocen tras leer todo el archivo. Si falta la línea `end`, la salida está incompleta y DEBE descartarse.

### Serialización canónica

Para que la regla 8 (salida determinista) se pueda cumplir, la serialización queda fijada:

- UTF-8 sin BOM. Cada línea termina en LF (`\n`), incluida la última. Nunca CRLF.
- Sin espacios ni saltos fuera de las cadenas. Separadores `,` y `:` a secas.
- Escapes JSON mínimos: `\"`, `\\`, `\b`, `\f`, `\n`, `\r`, `\t`, y `\u00xx` (hexadecimal en minúsculas) para el resto de caracteres de control. No se escapan `/` ni los caracteres no ASCII.
- Enteros sin signo `+`, sin ceros a la izquierda y sin exponente ni parte decimal.
- `type` es siempre el primer campo. El resto se escribe en el orden de las tablas de este documento. Las listas conservan su orden semántico: `sources` por `id` estrictamente creciente y `flags` en el orden de la tabla de marcas. Las claves de `params` van en orden ascendente de bytes UTF-8.
- Los campos opcionales sin información se omiten, igual que los que tendrían su valor por defecto: `requires_lacing` solo se escribe como `true`, y `block_additions`, `block_addition_mappings` y `params` no se escriben vacíos.
- Base64 canónico: relleno obligatorio y bits sobrantes del último símbolo a cero.
- Los mensajes de `error` NO DEBEN incluir texto dependiente del entorno (rutas temporales, mensajes del sistema operativo, fechas).

Un campo desconocido, como una marca desconocida, invalida la salida: un error de escritura como `"pts":0` no pasa inadvertido. Las ampliaciones del formato cambian `version`.

### Tipos comunes

| Tipo | Forma en JSON | Ejemplo |
| --- | --- | --- |
| Entero | número sin decimales, entre -(2^53-1) y 2^53-1 | `41708333` |
| Tiempo | entero en nanosegundos | `1000000000` = 1 s |
| Racional | `[numerador, denominador]`, ambos > 0 | `[24000, 1001]` |
| Bytes | cadena base64 estándar, con relleno `=` | `"EhA="` |
| Real | número decimal, solo en `mastering` y en los ángulos de `projection`: el decimal más corto que vuelve al mismo `f64`, sin exponente y con `-0` escrito como `0` | `0.708`, `1000`, `0.0001` |
| Cadena de datos | lista de trozos (ver abajo); puede estar vacía | `[["src",0,0,418]]` |

El límite de 2^53 existe porque muchos lectores JSON pierden precisión por encima. Equivale a 9 PB en offsets y 104 días en tiempos.

### Cadena de datos

Una cadena de datos son bytes formados por trozos concatenados en orden. Cada trozo es una de estas formas:

- `["src", fuente, offset, longitud]`: `longitud` bytes del archivo de origen `fuente`, empezando en `offset`.
- `["inline", "<base64>"]`: bytes pequeños generados por el parser.

Una forma `["xform", …]` queda reservada para una versión futura. Los parsers v1 NO DEBEN usarla.

## Línea header

La primera línea identifica el formato, el parser y los archivos de origen.

```json
{"type":"header","format":"vmkv-parser-output","version":1,"parser":{"name":"mp3-parser","version":"0.1.0"},"sources":[{"id":0,"size":5234123,"sha256":"9f86d0…","path":"cancion.mp3"}]}
```

| Campo | Obligatorio | Significado |
| --- | --- | --- |
| `format` | sí | Siempre `"vmkv-parser-output"` |
| `version` | sí | Siempre `1` en esta versión |
| `parser.name`, `parser.version` | sí | Qué programa generó la salida |
| `sources` | sí | Lista de archivos leídos; al menos uno |
| `sources[].id` | sí | Número que usan los trozos `src`; único en la lista |
| `sources[].size` | sí | Tamaño exacto en bytes |
| `sources[].sha256` | recomendado | Hash del contenido, en hexadecimal minúsculas |
| `sources[].path` | no | Solo informativo; NO identifica el archivo. Los parsers de referencia no lo escriben, porque dependería de cómo se invoque el parser (regla 8) |
| `params` | no | Objeto con los parámetros externos que recibió el parser y que afectan a la salida (por ejemplo `{"frame_rate":[24000,1001]}`). Los valores usan los tipos comunes. Valores: entero, racional o cadena. Claves en orden ascendente de bytes. Se omite si no hubo ninguno |

Todo parámetro externo que cambie la salida DEBE aparecer en `params`, para que el `.vtj` diga de dónde salió su línea de tiempo. Cómo se entregan esos parámetros al parser (opciones de línea de comandos, API) no lo define este formato.

Un parser PUEDE leer varios archivos para una pista, por ejemplo WavPack con su archivo de corrección `.wvc`.

## Líneas unit

Cada línea `unit` es un frame tal como lo guardará Matroska.

```json
{"type":"unit","pts_ns":0,"duration_ns":26122449,"flags":["random_access"],"payload":[["src",0,0,418]]}
```

| Campo | Obligatorio | Significado |
| --- | --- | --- |
| `pts_ns` | sí | Momento en que se muestra o suena, en ns; puede ser negativo |
| `duration_ns` | sí | Duración en ns; `-1` si no se conoce |
| `flags` | sí | Lista de marcas (tabla siguiente); puede estar vacía |
| `payload` | sí | Cadena de datos con los bytes del frame; puede estar vacía |
| `codec_state` | no | Cadena de datos con estado nuevo del decodificador, si el códec lo usa |
| `discard_padding_ns` | no | Silencio a descartar en ns: positivo al final, negativo al principio. Se calcula como diferencia de instantes redondeados, igual que `duration_ns` (regla 3). Un relleno final más largo que un frame se reparte hacia atrás entre los últimos frames: los que son relleno entero descartan exactamente su `duration_ns` |
| `block_additions` | no | Lista de `{"id": n, "data": cadena}` con datos auxiliares; `id` ≥ 1 y único en el frame |

### Marcas

| Marca | Cuándo ponerla |
| --- | --- |
| `random_access` | Punto de entrada: se puede empezar a decodificar en este frame sin los anteriores. Si la pista declara `seek_preroll_ns`, la salida es correcta tras decodificar ese preroll desde el punto de entrada. Es lo que Matroska llama keyframe |
| `invisible` | El frame se decodifica pero no se muestra |
| `duration_required` | La duración debe guardarse aunque se pueda deducir. Exige `duration_ns` ≥ 0 |

`random_access` NO significa solo "frame intra". En vídeo, un frame intra que no reinicia el estado del decodificador no lleva la marca: tiene que poder decodificarse sin ningún frame anterior.

En audio se marcan todos los frames cuyo códec permite empezar a decodificar en cualquiera de ellos:

- Con preroll declarado (Opus): la dependencia de los paquetes anteriores queda acotada por `seek_preroll_ns`.
- Con dependencias cortas que el decodificador tolera por diseño al empezar: la reserva de bits de MP3 (`main_data_begin`, como mucho 511 bytes hacia atrás) solo puede afectar a los primeros frames decodificados tras un salto, cuyos datos que falten rellena el decodificador. Matroska y los reproductores tratan esos frames como keyframes, y un criterio estricto dejaría la pista sin ningún punto de acceso.

Una marca desconocida invalida la salida. Así, un error de escritura como `"randon_access"` no pasa inadvertido.

## Línea track

La línea `track` describe la pista. Va después del último frame.

```json
{"type":"track","track_type":"audio","codec_id":"A_AAC","codec_private":[["inline","EhA="]],"audio":{"sampling_frequency":[44100,1],"channels":2}}
```

| Campo | Obligatorio | Significado |
| --- | --- | --- |
| `track_type` | sí | `video`, `audio`, `subtitle`, `complex`, `logo`, `buttons`, `control` o `metadata` |
| `codec_id` | sí | CodecID de Matroska, p. ej. `V_MPEG4/ISO/AVC`, `A_OPUS`, `S_TEXT/UTF8` |
| `codec_private` | si el códec lo exige | Cadena de datos con la inicialización que pide el mapping de Matroska |
| `codec_delay_ns` | si el códec lo exige | Retardo del códec en ns (Opus: pre-skip) |
| `seek_preroll_ns` | si el códec lo exige | Datos a decodificar antes de un punto de búsqueda, en ns |
| `video` | si `track_type` = `video` | Ver tabla de vídeo |
| `audio` | si `track_type` = `audio` | Ver tabla de audio |
| `block_addition_mappings` | no | Lista de `{"id_value"?, "name"?, "type", "extra_data"?}`; `id_value` ≥ 2 y único, `type` ≠ 0, `extra_data` es una cadena de datos |
| `requires_lacing` | no | `true` solo si la pista es imposible de representar sin lacing; por defecto `false` |

### Audio

| Campo | Obligatorio | Significado |
| --- | --- | --- |
| `sampling_frequency` | sí | Racional, p. ej. `[48000, 1]` |
| `channels` | sí | Entero > 0 |
| `output_sampling_frequency` | no | Racional; frecuencia real de salida si difiere (AAC con SBR) |
| `bit_depth` | no | Bits por muestra |

### Vídeo

| Campo | Obligatorio | Significado |
| --- | --- | --- |
| `pixel_width`, `pixel_height` | sí | Tamaño codificado, > 0 |
| `pixel_crop_left`, `_top`, `_right`, `_bottom` | no | Píxeles a recortar |
| `display_width`, `display_height` | no | Tamaño de presentación (anamórfico) |
| `display_unit` | no | `pixels`, `centimeters`, `inches`, `display_aspect_ratio` o `unknown` |
| `interlace` | no | `undetermined`, `interlaced` o `progressive` |
| `field_order` | no | Valor numérico de FieldOrder de Matroska |
| `stereo_mode`, `alpha_mode` | no | Valor numérico de Matroska |
| `nominal_frame_rate` | no | Racional, p. ej. `[24000, 1001]`; solo informativo |
| `default_decoded_field_duration_ns` | no | Periodo entre campos, en ns |
| `uncompressed_fourcc` | con `V_UNCOMPRESSED` | 4 bytes en base64 |
| `colour` | no | Objeto con los campos de color de Matroska (matriz, rango, transferencia, primarios, MaxCLL, MaxFALL, `mastering`) |
| `projection` | no | `{"type", "private"?, "yaw"?, "pitch"?, "roll"?}`; `type`: `rectangular`, `equirectangular`, `cubemap` o `mesh`; `private` es una cadena de datos; los ángulos son reales en grados |

Los campos de `colour` usan los nombres de Matroska en minúsculas con guiones bajos y van en el orden de los elementos de Matroska. Todos son opcionales:

| Campo | Tipo |
| --- | --- |
| `matrix_coefficients`, `bits_per_channel`, `chroma_subsampling_horz`, `chroma_subsampling_vert`, `cb_subsampling_horz`, `cb_subsampling_vert`, `chroma_siting_horz`, `chroma_siting_vert`, `range`, `transfer_characteristics`, `primaries`, `max_cll`, `max_fall` | Entero ≥ 0 |
| `mastering` | Objeto con `primary_r_chromaticity_x`, `primary_r_chromaticity_y`, `primary_g_chromaticity_x`, `primary_g_chromaticity_y`, `primary_b_chromaticity_x`, `primary_b_chromaticity_y`, `white_point_chromaticity_x`, `white_point_chromaticity_y`, `luminance_max`, `luminance_min`, en ese orden; reales con el valor real, no codificado |

`video` solo aparece con `track_type` = `video`, y `audio` solo con `track_type` = `audio`.

En `projection`, `private` NO DEBE aparecer con `rectangular` y DEBE aparecer con los otros tres tipos.

Un campo sin información se omite. Nunca se escribe `null` ni un valor por defecto inventado.

## Línea end y errores

La última línea confirma que la salida está completa.

```json
{"type":"end","unit_count":11482}
```

`unit_count` DEBE coincidir con el número de líneas `unit`.

Si el parser falla, NO escribe `end`. En su lugar escribe una línea de error como última línea, termina con código de salida distinto de 0 y la salida se descarta entera. El `header` y las líneas `unit` ya escritas PUEDEN estar presentes o no:

```json
{"type":"error","code":"TRUNCATED_BITSTREAM","message":"frame 1201 cortado en el byte 5230001"}
```

| Código | Cuándo |
| --- | --- |
| `INVALID_BITSTREAM` | Los datos no son válidos para el códec |
| `TRUNCATED_BITSTREAM` | El archivo termina a mitad de un frame |
| `UNSUPPORTED_CODEC_VARIANT` | Variante del códec que el parser no conoce |
| `UNSUPPORTED_PROFILE` | Perfil o nivel no soportado |
| `UNSUPPORTED_FEATURE` | Característica del stream no soportada |
| `TIMING_REQUIRED` | No hay forma de calcular los tiempos sin información externa |
| `MISSING_INITIALIZATION_DATA` | Falta la inicialización del códec (p. ej. sin SPS en H.264) |
| `INCONSISTENT_TRACK_PARAMETERS` | Los parámetros cambian a mitad de pista de forma no representable |
| `SOURCE_UNREADABLE` | Un archivo de origen no se puede abrir o leer (error de E/S, no de contenido). El detalle del sistema va a stderr, no al mensaje |
| `UNREPRESENTABLE_IN_VMKV` | Cualquier otra cosa que este formato no puede expresar |

`message` es libre y PUEDE incluir detalles del códec, pero debe ser determinista (ver Serialización canónica).

## Reglas obligatorias

Todo parser DEBE cumplir estas ocho reglas. La mayoría de errores sutiles vienen de romper la 2, la 3 o la 5.

1. **Frames en orden de archivo.** Las líneas `unit` van en el orden en que el códec las decodifica, que es el del archivo. NO se ordenan por `pts_ns`. En vídeo con frames B, los tiempos saltan: 0, 120, 40, 80 ms es correcto.
2. **Tiempos calculados desde el origen.** Cada `pts_ns` se calcula con aritmética exacta a partir de su posición y se redondea una sola vez al nanosegundo más cercano; los empates se redondean hacia +∞ (es lo que dice la fórmula de abajo, también para tiempos negativos). NUNCA se suman duraciones ya redondeadas, porque el error se acumula.
3. **Duraciones coherentes con los tiempos.** Si la duración sale del reloj del códec, `duration_ns` = `pts_ns` del siguiente frame en orden de presentación − `pts_ns` de este. El último frame usa el instante final exacto redondeado igual. Si no se conoce, `-1`.
4. **Tiempos reales, sin desplazar.** `pts_ns` es el instante en que se oye o se ve. El retardo del códec va en `codec_delay_ns` y NO se suma a `pts_ns`. El parser NO desplaza la pista para que empiece en 0; los tiempos negativos son válidos.
5. **El payload es lo que Matroska espera.** Los bytes de `payload` son exactamente el frame que el mapping de Matroska define para ese `codec_id`. El parser hace toda la conversión: quitar cabeceras ADTS, cambiar start codes por longitudes, etc. Nada posterior tocará esos bytes.
6. **Apuntar al original.** Los datos grandes se referencian con trozos `src`. `inline` es para trozos pequeños generados: longitudes, cabeceras reescritas, datos de inicialización. Todo trozo `src` cumple `offset + longitud ≤ size` de su fuente.
7. **Fallar antes que inventar.** Si algo no se puede representar, el parser termina con error. Nunca descarta datos necesarios en silencio ni rellena valores que no conoce.
8. **Salida determinista.** Los mismos archivos de origen, los mismos parámetros externos (`params`) y la misma versión del parser producen la misma salida, byte a byte, siguiendo la serialización canónica.

La regla 2 escrita como fórmula, con t(n) el instante exacto del frame n como racional:

```latex
\mathrm{pts}_n = \left\lfloor t(n) \cdot 10^{9} + \tfrac{1}{2} \right\rfloor
```

Con t(n) = p/q segundos (p entero, q > 0), la fórmula equivale a `pts = floor((2·p·10^9 + q) / (2·q))`, con división entera con suelo (no truncada hacia cero) y enteros de al menos 128 bits en los productos intermedios. Un empate en −1,5 ns da −1, no −2: una función de biblioteca llamada "half up" o "round half away from zero" puede no coincidir con esta fórmula para negativos, así que no se usa sin comprobarlo.

Ejemplo a 24000/1001 fps: el frame 3 empieza en 3 × 1001/24000 s = 125 125 000 ns exactos. Sumar tres veces 41 708 333 da 125 124 999: un nanosegundo menos, que tras horas de vídeo se convierte en milisegundos.

## Lo que el parser no hace

El parser describe una pista suelta. Todo lo que tenga que ver con el archivo MKV final lo hace otra pieza más adelante.

El parser NO DEBE decidir ni escribir:

- Número de pista, UID de pista, nombre, idioma, pista por defecto o forzada.
- Clusters, Cues, SeekHead, TimestampScale ni offsets del MKV.
- Mezcla con otras pistas ni relación entre pistas.
- DefaultDuration, lacing ni compresión o cifrado de Matroska.
- Nada relacionado con FUSE o con la caché.

Si un archivo contiene varias pistas (por ejemplo TrueHD con núcleo AC-3), se ejecuta el parser una vez por pista. Cada ejecución produce su propia salida, y ambas pueden apuntar a la misma fuente.

## Ejemplos

Los tiempos y los base64 de estos ejemplos están calculados. Los offsets y tamaños son inventados, pero coherentes entre sí. Las líneas `header` se omiten salvo en el primero.

### MP3 (el caso más simple)

Cada frame MP3 se copia tal cual. El archivo empieza con una etiqueta ID3 de 2048 bytes, que el parser salta. Una trama Xing/Info tampoco es audio y también se salta. Si lleva etiqueta LAME, su retardo del encoder más el del decoder (529 muestras) va en `codec_delay_ns` y adelanta los `pts_ns` como en Opus, y su relleno final menos esas 529 muestras va en `discard_padding_ns` de los últimos frames (a menudo supera un frame). Sin etiqueta LAME no se inventa ningún retardo. 1152 muestras a 44,1 kHz son 26 122 448,98 ns por frame.

```json
{"type":"header","format":"vmkv-parser-output","version":1,"parser":{"name":"mp3-parser","version":"0.1.0"},"sources":[{"id":0,"size":5234123,"sha256":"9f86d0…"}]}
{"type":"unit","pts_ns":0,"duration_ns":26122449,"flags":["random_access"],"payload":[["src",0,2048,418]]}
{"type":"unit","pts_ns":26122449,"duration_ns":26122449,"flags":["random_access"],"payload":[["src",0,2466,417]]}
{"type":"unit","pts_ns":52244898,"duration_ns":26122449,"flags":["random_access"],"payload":[["src",0,2883,418]]}
{"type":"track","track_type":"audio","codec_id":"A_MPEG/L3","audio":{"sampling_frequency":[44100,1],"channels":2}}
{"type":"end","unit_count":3}
```

### AAC en ADTS

Matroska quiere el frame AAC sin la cabecera ADTS, así que el trozo `src` empieza 7 bytes después. La configuración va en `codec_private`: `EhA=` son los bytes 0x12 0x10, que corresponden a AAC-LC, 44,1 kHz y estéreo. Las duraciones alternan 23 219 955 y 23 219 954 porque salen de restar tiempos redondeados (regla 3).

```json
{"type":"unit","pts_ns":0,"duration_ns":23219955,"flags":["random_access"],"payload":[["src",0,7,364]]}
{"type":"unit","pts_ns":23219955,"duration_ns":23219954,"flags":["random_access"],"payload":[["src",0,378,373]]}
{"type":"track","track_type":"audio","codec_id":"A_AAC","codec_private":[["inline","EhA="]],"audio":{"sampling_frequency":[44100,1],"channels":2}}
```

### H.264 en Annex B

El archivo es: start code, SPS de 25 bytes (offset 4), start code, PPS de 4 bytes (offset 33), start code, frame IDR de 4340 bytes (offset 41), start code, frame P de 1200 bytes (offset 4385).

El parser construye `codec_private` intercalando cabeceras generadas con el SPS y el PPS originales. Cada frame pasa a tener delante su longitud en 4 bytes (`AAAQ9A==` es 4340; `AAAEsA==` es 1200) en lugar del start code. En este ejemplo el SPS y el PPS quedan solo en `codec_private`.

```json
{"type":"unit","pts_ns":0,"duration_ns":41708333,"flags":["random_access"],"payload":[["inline","AAAQ9A=="],["src",0,41,4340]]}
{"type":"unit","pts_ns":41708333,"duration_ns":41708334,"flags":[],"payload":[["inline","AAAEsA=="],["src",0,4385,1200]]}
{"type":"track","track_type":"video","codec_id":"V_MPEG4/ISO/AVC","codec_private":[["inline","AU0AKP/hABk="],["src",0,4,25],["inline","AQAE"],["src",0,33,4]],"video":{"pixel_width":1920,"pixel_height":1080,"interlace":"progressive","nominal_frame_rate":[24000,1001]}}
```

Un `.h264` crudo no trae tiempos. Si el SPS no indica la frecuencia de frames y no se proporciona desde fuera, el parser DEBE fallar con `TIMING_REQUIRED`. Con frames B, los tiempos salen del orden de presentación (POC), no del orden del archivo.

### Opus en Ogg

Esta es la regla 4 en acción. El pre-skip de 312 muestras a 48 kHz va en `codec_delay_ns` (6 500 000 ns). Por eso el primer paquete se oye en −6,5 ms: su tiempo real es negativo, y el parser NO lo desplaza a cero. `codec_private` es la cabecera OpusHead, referenciada directamente en la primera página Ogg.

```json
{"type":"unit","pts_ns":-6500000,"duration_ns":20000000,"flags":["random_access"],"payload":[["src",0,3895,120]]}
{"type":"unit","pts_ns":13500000,"duration_ns":20000000,"flags":["random_access"],"payload":[["src",0,4015,118]]}
{"type":"track","track_type":"audio","codec_id":"A_OPUS","codec_private":[["src",0,28,19]],"codec_delay_ns":6500000,"seek_preroll_ns":80000000,"audio":{"sampling_frequency":[48000,1],"channels":2}}
```

Un paquete Ogg partido entre dos páginas se expresa con dos trozos `src` seguidos. Los últimos paquetes de la página final PUEDEN llevar `discard_padding_ns` si su posición granular indica muestras sobrantes.

### Subtítulos SRT

El payload es solo el texto, sin número ni tiempos. Los subtítulos llevan `duration_required` porque entre uno y otro hay huecos sin texto. Archivo UTF-8 de 83 bytes con saltos de línea LF:

```json
{"type":"unit","pts_ns":1000000000,"duration_ns":2500000000,"flags":["random_access","duration_required"],"payload":[["src",0,32,5]]}
{"type":"unit","pts_ns":5000000000,"duration_ns":2250000000,"flags":["random_access","duration_required"],"payload":[["src",0,71,11]]}
{"type":"track","track_type":"subtitle","codec_id":"S_TEXT/UTF8"}
```

Si el archivo no está en UTF-8, el parser convierte el texto y lo escribe como `inline`.

## Checklist de validación

Una salida es válida solo si cumple todo lo siguiente. Un validador estructural puede comprobar todo salvo la regla 5 y los campos que dependen del mapping del códec; esos los cubre el nivel `--codec-aware`.

- [ ] Cada línea es JSON válido y tiene un `type` conocido, sin campos desconocidos.
- [ ] El orden es `header`, `unit`\*, `track`, `end`, sin líneas extra; o, en un fallo, `header`? `unit`\* `error`, con `error` como última línea.
- [ ] La serialización es canónica (LF, sin espacios, escapes y orden de campos como en este documento).
- [ ] Ningún campo vale `null`: lo desconocido se omite.
- [ ] `unit_count` coincide con el número de líneas `unit`.
- [ ] Todos los enteros están dentro de ±(2^53−1).
- [ ] Los `id` de `sources` son únicos y todo trozo `src` usa uno existente.
- [ ] Todo trozo `src` cumple `offset + longitud ≤ size`.
- [ ] Todo trozo `inline` es base64 válido.
- [ ] `duration_ns` ≥ −1 en todos los frames.
- [ ] Los frames con `duration_required` tienen `duration_ns` ≥ 0.
- [ ] Todas las marcas son conocidas y no se repiten dentro de un frame.
- [ ] Los `id` de `block_additions` son ≥ 1 y únicos dentro de su frame. Los ≥ 2 tienen un mapping con ese `id_value`.
- [ ] `codec_id` no está vacío y `track_type` es un valor conocido. `video` y `audio` solo aparecen con su `track_type`.
- [ ] `code` de `error` es uno de los códigos de la tabla.
- [ ] `codec_delay_ns` y `seek_preroll_ns` ≥ 0; `bit_depth`, `display_width`, `display_height` y `default_decoded_field_duration_ns` > 0; el recorte deja algún píxel.
- [ ] Vídeo: `pixel_width` y `pixel_height` > 0. Audio: `sampling_frequency` con ambos términos > 0 y `channels` > 0.
- [ ] Los racionales tienen numerador y denominador > 0.
- [ ] `projection.private` falta con `rectangular` y existe con los otros tipos.
- [ ] Si los archivos de origen están disponibles, `size` y `sha256` coinciden.
- [ ] Ejecutar el parser dos veces con los mismos parámetros da una salida idéntica (regla 8), y todo parámetro externo aparece en `params`.
- [ ] Nivel `--codec-aware` (opcional): `codec_private`, `codec_delay_ns`, `seek_preroll_ns` y `uncompressed_fourcc` presentes cuando el mapping del códec los exige.

## Relación con VMKV Track IR v0.4

Este formato es la versión práctica y legible del TrackIndex de VMKV Track IR v0.4. Una salida válida se convierte en un TrackIndex sin perder información.

| En este formato | En Track IR v0.4 |
| --- | --- |
| `sources` de `header` | `Source` |
| Trozo `src` | `SourceExtent` |
| Trozo `inline` (base64 directo) | `InlineExtent` + `inline_data` |
| Trozo `xform` (reservado) | `TransformExtent` |
| Línea `unit` | `Unit` |
| `flags` como texto | `UnitFlags` como bits |
| Línea `track` | `TrackDescriptor` |
| Línea `error` | Modelo de errores (sección 59) |

Para escribir un parser basta con este documento. El Track IR solo importa a quien construya el planner.
