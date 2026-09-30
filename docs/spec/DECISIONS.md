# Decisiones de interpretación del spec v1

El spec [`VMKV_Parser_Output_Format_v1.md`](VMKV_Parser_Output_Format_v1.md) deja algunos
puntos abiertos o contradictorios. La librería `vtj` implementa las decisiones de esta
lista. Las marcadas **(abierta)** conviene confirmarlas y pasarlas al texto del spec. Si
alguna cambia, hay que cambiar el código, los ficheros dorados y esta página a la vez.

## Serialización

1. **`type` es siempre el primer campo** de cada línea. Las tablas no lo listan, pero
   todos los ejemplos lo ponen primero.
2. **Orden de `sources[]`: `id`, `size`, `sha256`, `path`** (orden de la tabla). El ejemplo
   de la sección "Línea header" pone `path` antes que `size`, lo que contradice la regla
   "los campos se escriben en el orden de las tablas". Prevalece la regla. **(abierta:
   corregir el ejemplo)**
3. **Las claves de `params` van en orden ascendente de bytes UTF-8.** El spec no fija
   ningún orden y sin uno la regla 8 no se puede cumplir. `params` vacío se omite.
   Valores admitidos: entero, racional o cadena.
4. **`colour` sigue el orden de elementos de Matroska**: `matrix_coefficients`,
   `bits_per_channel`, `chroma_subsampling_horz`, `chroma_subsampling_vert`,
   `cb_subsampling_horz`, `cb_subsampling_vert`, `chroma_siting_horz`,
   `chroma_siting_vert`, `range`, `transfer_characteristics`, `primaries`, `max_cll`,
   `max_fall`, `mastering`. `mastering` usa `primary_{r,g,b}_chromaticity_{x,y}`,
   `white_point_chromaticity_{x,y}`, `luminance_max` y `luminance_min`. **(abierta:
   añadir la tabla al spec)**
5. **Números reales** (valores de `mastering` y `yaw`, `pitch`, `roll` de `projection`):
   el spec pide "valores reales" pero solo define enteros. Se escriben como el decimal más
   corto que vuelve al mismo `f64`, sin exponente y con `-0` escrito como `0` (por
   ejemplo `1000`, `0.708`, `0.0001`). **(abierta)**
6. **Tipos sin especificar**: `projection.private` y `block_addition_mappings[].extra_data`
   son cadenas de datos, como `codec_private`. `uncompressed_fourcc` es base64 de
   exactamente 4 bytes. **(abierta)**
7. **Los valores por defecto se omiten**: `requires_lacing` solo se escribe como `true`, y
   `block_additions` y `block_addition_mappings` vacíos se omiten. Escribir
   `"requires_lacing":false` o `[]` no es canónico.
8. **Base64 canónico**: relleno obligatorio y bits sobrantes a cero. `EhB=` es inválido
   aunque un decodificador laxo lo lea como `EhA=`.

## Validación

9. **Un campo desconocido invalida la línea**, igual que una marca desconocida. Así, un
   error como `"pts":0` no pasa inadvertido. **(abierta: el spec solo lo dice para
   marcas)**
10. **Las `sources` deben ir en orden de `id` estrictamente creciente.** Lo exige la
    serialización canónica y también garantiza que los ids sean únicos.
11. **`video` solo con `track_type` `video`, y `audio` solo con `audio`.** El spec dice
    cuándo son obligatorios, no cuándo están prohibidos.
12. **Otras restricciones que el checklist no enumera**: `codec_delay_ns` ≥ 0,
    `seek_preroll_ns` ≥ 0, `bit_depth` > 0, `display_width` y `display_height` > 0,
    `default_decoded_field_duration_ns` > 0, el recorte debe dejar algún píxel, y los
    `id_value` de los mappings son únicos.
13. **`code` de `error` debe ser uno de los nueve códigos.** El checklist no lo dice
    explícitamente.
14. **Un trozo `src` sin header** (salida de fallo sin `header`) es inválido, porque no
    existe la fuente a la que apunta.
15. **Salida del validador**: 0 éxito válido, 1 inválido, 2 error de uso o E/S,
    3 salida de fallo bien formada (hay que descartarla igualmente).

## Contrato de parsers

16. **No se escribe `sources[].path`.** Dependería de cómo se invoque el parser (ruta
    relativa o absoluta, nombre del archivo) y rompería la regla 8.
17. **Error al abrir o leer una fuente**: ningún código de error encaja. Se usa
    `UNREPRESENTABLE_IN_VMKV` con el mensaje determinista `source N cannot be read`, y el
    detalle del sistema operativo va solo a stderr. Una lectura que se queda corta usa
    `TRUNCATED_BITSTREAM`. **(abierta: propuesta de añadir `SOURCE_UNREADABLE`)**
18. **Una violación del formato por el propio parser** (por ejemplo un `src` fuera de
    rango) la detecta el writer y se emite como `UNREPRESENTABLE_IN_VMKV` con un mensaje
    que empieza por `internal:`.
19. **Códigos de salida del parser**: 0 éxito; 1 fallo de parseo (línea `error` escrita);
    2 error de uso (no se escribe nada); 3 no se pudo escribir la salida.
20. **Un parámetro racional** se pasa como `N/D` o como `N` (equivale a `N/1`) y se guarda
    tal cual, sin reducir.

## Pendiente para fases posteriores

- **(abierta)** MP3: qué hacer con la trama Xing/Info/LAME, que no es audio pero indica
  el retardo y el relleno del encoder: saltarla, o convertirla en `codec_delay_ns` y
  `discard_padding_ns`.
- `durations_from_pts` da duración 0 a las unidades que comparten `pts_ns`, salvo a la
  última. Hay que revisarlo con H.264 real.
