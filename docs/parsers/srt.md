# vmkv-parser-srt

SubRip subtitles (`S_TEXT/UTF8`). Without arguments, everything is detected automatically. Every argument passed is recorded in `header.params` (rule 8).

| Option | Values | Default | Effect |
| --- | --- | --- | --- |
| `--blank-lines-in-cue` | `keep`, `strict` | `keep` | `keep`: blank lines that are not followed by a new cue (an index line and a timing line) or the end of the file belong to the cue's text, as in cues that start with blank lines to raise the text on screen. A timing line without an index after them is still `INVALID_BITSTREAM`. `strict`: every blank line ends the cue |

See decision 67 for the reasoning, and decision 39 for encodings and line endings.
