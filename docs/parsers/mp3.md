# vmkv-parser-mp3

MPEG-1, MPEG-2 and MPEG-2.5 Layer III (`A_MPEG/L3`). Without arguments, everything is detected automatically. Every argument passed is recorded in `header.params`, so each `.vtj` says which manual settings produced it (rule 8). Any combination that contradicts itself is a usage error: the parser exits with code 2 and writes nothing.

`vmkv-parse mp3 --help` shows the same list with its ranges and default values.

## Gapless (start delay and end padding)

| Option | Values | Default | Effect |
| --- | --- | --- | --- |
| `--gapless` | `auto`, `off` | `auto` | `off`: no delay and no padding; the timeline starts at 0 |
| `--lame-crc` | `verify`, `ignore` | `verify` | `ignore`: uses the LAME tag even if its CRC does not match |
| `--encoder-delay` | 0–65535 samples | LAME tag's | Forces the encoder delay; without a tag, it enables gapless on its own |
| `--encoder-padding` | 0–65535 samples | LAME tag's | Forces the padding, with LAME semantics: includes the decoder delay |
| `--decoder-delay` | 0–4096 samples | 529 | Decoder delay; added to the start delay and subtracted from the final padding |
| `--xing-count-mismatch` | `keep-delay`, `use-padding`, `ignore-tag` | `keep-delay` | When the Xing frame count does not match the frames found |
| `--info-frame` | `skip`, `keep` | `skip` | `keep`: writes the Xing/Info/VBRI frame as a unit (it decodes to silence) and adds its duration to the delay, so the audible part does not move |

- `codec_delay_ns` = encoder delay + decoder delay (+ the info frame with `keep`).
- The discarded end is `padding − decoder delay`, spread backwards over the last frames.
- `--gapless off` conflicts with the other options in this table.

## Damaged or unusual streams

| Option | Values | Default | Effect |
| --- | --- | --- | --- |
| `--junk` | `error`, `resync` | `error` | `resync` skips bytes that are not a frame until the next frame consistent with the stream (same version, rate and channels, and followed by another valid frame). The skipped bytes are lost. A real parameter change is still `INCONSISTENT_TRACK_PARAMETERS` |
| `--zero-padding` | `skip`, `error` | `skip` | Zeros before the first frame or after the last one (taggers' padding) |
| `--incomplete-end` | `error`, `drop` | `error` | `drop`: a last frame cut off by the end of the audio is discarded instead of failing |
| `--byte-range` | `A:B` or `A:` | between the tags | Parses exactly those bytes and ignores tag detection. Conflicts with `--zero-padding` |

The policies that discard data (`resync`, `drop`) are never the default. They only apply when requested, and they are recorded in `params` (decision 38).
