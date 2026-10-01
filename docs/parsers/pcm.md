# vmkv-parser-pcm

Integer and float PCM in WAV, RF64 and BW64 (`A_PCM/INT/LIT`, `A_PCM/FLOAT/IEEE`). Without arguments, everything is detected automatically. Every argument passed is recorded in `header.params` (rule 8).

| Option | Values | Default | Effect |
| --- | --- | --- | --- |
| `--unit-samples` | 1–65536 | sample rate / 25 (40 ms) | Samples per unit; the last unit holds what is left |
| `--truncated-data` | `error`, `keep` | `error` | A `data` chunk longer than the source: `keep` describes the whole sample frames present and drops a partial one at the end |

Supported formats: `WAVE_FORMAT_PCM`, `WAVE_FORMAT_IEEE_FLOAT` and `WAVE_FORMAT_EXTENSIBLE` with either subformat; integer samples of 8, 16, 24 or 32 bits (fewer valid bits left-justified in a container rounded up to bytes) and float samples of 32 or 64 bits. The `EXTENSIBLE` channel mask is not carried: neither the v1 format nor the Matroska PCM mapping has a field for it (decision 60).
