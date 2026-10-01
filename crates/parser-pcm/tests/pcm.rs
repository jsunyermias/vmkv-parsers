//! Golden files for real encoder output, plus synthetic WAV files for the
//! structural cases. Set `UPDATE_GOLDEN=1` to rewrite the golden files
//! after an intended change.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use vmkv_parser_pcm::Pcm;
use vtj::cli;
use vtj::validate::{validate, Options, Outcome};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn run_args(args: &[&str], input: &Path) -> (i32, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut argv: Vec<OsString> = args.iter().map(OsString::from).collect();
    argv.push(input.as_os_str().to_os_string());
    let code = cli::run(&Pcm, &argv, &mut out, &mut err);
    (code, String::from_utf8(out).unwrap())
}

fn run(input: &Path) -> (i32, String) {
    run_args(&[], input)
}

fn temp(name: &str, bytes: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vmkv-pcm-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

fn outcome(out: &str) -> Outcome {
    let r = validate(out.as_bytes(), &Options { codec_aware: true, max_problems: 0 });
    assert_ne!(r.outcome, Outcome::Invalid, "{:?}", r.problems);
    r.outcome
}

fn units(out: &str) -> Vec<&str> {
    out.lines().filter(|l| l.starts_with(r#"{"type":"unit""#)).collect()
}

fn error(out: &str) -> &str {
    out.lines().last().unwrap()
}

fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut c = id.to_vec();
    c.extend((body.len() as u32).to_le_bytes());
    c.extend(body);
    if body.len() % 2 == 1 {
        c.push(0);
    }
    c
}

/// 16-bit mono PCM at 1000 Hz.
fn fmt16() -> Vec<u8> {
    let mut b = 1u16.to_le_bytes().to_vec();
    b.extend(1u16.to_le_bytes());
    b.extend(1000u32.to_le_bytes());
    b.extend(2000u32.to_le_bytes());
    b.extend(2u16.to_le_bytes());
    b.extend(16u16.to_le_bytes());
    b
}

fn wav(chunks: &[Vec<u8>]) -> Vec<u8> {
    let body: Vec<u8> = chunks.concat();
    let mut w = b"RIFF".to_vec();
    w.extend((4 + body.len() as u32).to_le_bytes());
    w.extend(b"WAVE");
    w.extend(body);
    w
}

#[test]
fn golden_outputs() {
    for name in ["pcm_s16_stereo", "pcm_s24_51", "pcm_f32_mono", "pcm_u8_rf64"] {
        let input = root().join(format!("testdata/media/{name}.wav"));
        let (code, out) = run(&input);
        assert_eq!(code, 0, "{out}");
        assert_eq!(outcome(&out), Outcome::Success);
        let golden = root().join(format!("testdata/golden/pcm/{name}.vtj"));
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
            std::fs::write(&golden, &out).unwrap();
        }
        assert_eq!(out, std::fs::read_to_string(&golden).unwrap(), "{name}");
        assert_eq!(run(&input).1, out, "rule 8");
    }
    let (_, out) = run(&root().join("testdata/media/pcm_u8_rf64.wav"));
    // 5513 samples at 22050 Hz in units of 882: the odd-sized data chunk of
    // an RF64 file, and a short last unit.
    let u = units(&out);
    assert_eq!(u.len(), 7);
    assert!(u[6].contains(r#""payload":[["src",0,5406,221]]"#), "{}", u[6]);
    assert!(out
        .contains(r#""codec_id":"A_PCM/INT/LIT","audio":{"sampling_frequency":[22050,1],"channels":1,"bit_depth":8}"#));
    let (_, out) = run(&root().join("testdata/media/pcm_f32_mono.wav"));
    assert!(out.contains(r#""codec_id":"A_PCM/FLOAT/IEEE""#));
}

#[test]
fn unit_samples_parameter() {
    let data = vec![0u8; 2 * 2500];
    let p = temp("units.wav", &wav(&[chunk(b"fmt ", &fmt16()), chunk(b"data", &data)]));
    let (code, out) = run(&p);
    assert_eq!(code, 0, "{out}");
    // Default: 1000 / 25 = 40 samples per unit.
    assert_eq!(units(&out).len(), 63);
    let (code, out) = run_args(&["--unit-samples", "1000"], &p);
    assert_eq!(code, 0, "{out}");
    assert!(out.lines().next().unwrap().ends_with(r#""params":{"unit_samples":1000}}"#), "{out}");
    let u = units(&out);
    assert_eq!(u.len(), 3);
    assert!(u[2].starts_with(r#"{"type":"unit","pts_ns":2000000000,"duration_ns":500000000,"#), "{}", u[2]);
    assert!(u[2].contains(r#""payload":[["src",0,4044,1000]]"#), "{}", u[2]);
}

#[test]
fn chunks_around_data() {
    // A LIST chunk with an odd size before fmt and data, and one after.
    let list = chunk(b"LIST", b"INFOx");
    let data = vec![0u8; 20];
    let p = temp("chunks.wav", &wav(&[list.clone(), chunk(b"fmt ", &fmt16()), chunk(b"data", &data), list]));
    let (code, out) = run(&p);
    assert_eq!(code, 0, "{out}");
    assert!(units(&out)[0].contains(r#""payload":[["src",0,58,20]]"#), "{out}");

    let (_, out) = run(&temp("nofmt.wav", &wav(&[chunk(b"data", &data), chunk(b"fmt ", &fmt16())])));
    assert!(
        error(&out).contains(r#""code":"MISSING_INITIALIZATION_DATA","message":"data chunk before fmt chunk""#),
        "{out}"
    );

    let (_, out) = run(&temp("odd.wav", &wav(&[chunk(b"fmt ", &fmt16()), chunk(b"data", &[0; 5])])));
    assert!(error(&out).contains("data chunk of 5 bytes is not a multiple of block align 2"), "{out}");

    let (_, out) = run(&temp("nodata.wav", &wav(&[chunk(b"fmt ", &fmt16())])));
    assert!(error(&out).contains(r#""code":"INVALID_BITSTREAM","message":"no data chunk""#), "{out}");

    let (_, out) = run(&temp("notwav.wav", b"RIFF\x04\0\0\0AVI "));
    assert!(error(&out).contains("RIFF file is not WAVE"), "{out}");
}

#[test]
fn truncated_data_policy() {
    let mut w = wav(&[chunk(b"fmt ", &fmt16()), chunk(b"data", &[0; 200])]);
    w.truncate(w.len() - 51);
    let p = temp("cut.wav", &w);
    let (code, out) = run(&p);
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert_eq!(outcome(&out), Outcome::Failure);
    assert!(
        error(&out).contains(
            r#""code":"TRUNCATED_BITSTREAM","message":"data chunk declares 200 bytes, the source holds 149""#
        ),
        "{out}"
    );
    let (code, out) = run_args(&["--truncated-data", "keep"], &p);
    assert_eq!(code, 0, "{out}");
    let u = units(&out);
    // 74 whole samples are left: 40 + 34; the odd byte is not a sample.
    assert_eq!(u.len(), 2);
    assert!(u[1].contains(r#""duration_ns":34000000,"#) && u[1].contains(r#"["src",0,124,68]"#), "{}", u[1]);

    let mut w = wav(&[chunk(b"fmt ", &fmt16())]);
    w.extend(b"dat");
    let (_, out) = run(&temp("hdr.wav", &w));
    assert!(
        error(&out).contains(r#""code":"TRUNCATED_BITSTREAM","message":"chunk header at byte 36 cut at byte 39""#),
        "{out}"
    );
}

#[test]
fn rf64_large_data_size_comes_from_ds64() {
    let data = vec![0u8; 100];
    let mut ds64 = 0u64.to_le_bytes().to_vec();
    ds64.extend((data.len() as u64).to_le_bytes());
    ds64.extend(50u64.to_le_bytes());
    ds64.extend(0u32.to_le_bytes());
    let mut w = b"RF64\xff\xff\xff\xffWAVE".to_vec();
    w.extend(chunk(b"ds64", &ds64));
    w.extend(chunk(b"fmt ", &fmt16()));
    w.extend(b"data\xff\xff\xff\xff");
    w.extend(&data);
    let (code, out) = run(&temp("rf64.wav", &w));
    assert_eq!(code, 0, "{out}");
    assert_eq!(units(&out).len(), 2);

    let (_, out) = run(&temp("rf64bad.wav", b"RF64\xff\xff\xff\xffWAVEfmt \x10\0\0\0"));
    assert!(error(&out).contains("RF64 file does not start with a ds64 chunk"), "{out}");
}
