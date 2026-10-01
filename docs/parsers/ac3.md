# vmkv-parser-ac3

AC-3 (`A_AC3`) and E-AC-3 (`A_EAC3`) elementary streams. The codec is chosen from the `bsid` of the first frame. Without arguments, everything is detected automatically. Every argument passed is recorded in `header.params` (rule 8).

| Option | Values | Default | Effect |
| --- | --- | --- | --- |
| `--crc` | `verify`, `ignore` | `verify` | `verify`: a frame whose CRC-16 does not match is `INVALID_BITSTREAM`. `ignore`: frames are described as they are, for captures with isolated bit errors |

Not supported (decision 59): E-AC-3 dependent substreams (7.1 over a 5.1 core), independent substreams other than 0, and the reduced sample rate AC-3 variants (`bsid` 9 and 10).
