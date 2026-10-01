//! Golden files for real encoder output and failure cases derived from it
//! by editing pages (with their CRC fixed). Set `UPDATE_GOLDEN=1` to rewrite
//! the golden files after an intended change.

use std::path::{Path, PathBuf};

use vmkv_parser_vorbis::{ogg::crc32, Vorbis};
use vtj::cli;
use vtj::validate::{validate, Options, Outcome};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn media(name: &str) -> Vec<u8> {
    std::fs::read(root().join("testdata/media").join(name)).unwrap()
}

fn run(input: &Path) -> (i32, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = cli::run(&Vorbis, &[input.as_os_str().to_os_string()], &mut out, &mut err);
    (code, String::from_utf8(out).unwrap())
}

fn temp(name: &str, bytes: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vmkv-vorbis-{}", std::process::id()));
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

/// `(offset, length)` of every page.
fn pages(b: &[u8]) -> Vec<(usize, usize)> {
    let mut v = Vec::new();
    let mut p = 0;
    while p < b.len() {
        let n = b[p + 26] as usize;
        let body: usize = b[p + 27..p + 27 + n].iter().map(|&x| x as usize).sum();
        v.push((p, 27 + n + body));
        p += 27 + n + body;
    }
    v
}

fn granule(b: &[u8], page: (usize, usize)) -> i64 {
    i64::from_le_bytes(b[page.0 + 6..page.0 + 14].try_into().unwrap())
}

/// Sets the granule position of a page and fixes its CRC.
fn set_granule(b: &mut [u8], page: (usize, usize), g: i64) {
    b[page.0 + 6..page.0 + 14].copy_from_slice(&g.to_le_bytes());
    fix_crc(b, page);
}

fn fix_crc(b: &mut [u8], (off, len): (usize, usize)) {
    b[off + 22..off + 26].fill(0);
    let crc = crc32(&b[off..off + len]);
    b[off + 22..off + 26].copy_from_slice(&crc.to_le_bytes());
}

#[test]
fn golden_outputs() {
    for name in ["vorbis_stereo", "vorbis_mono_22k"] {
        let input = root().join(format!("testdata/media/{name}.ogg"));
        let (code, out) = run(&input);
        assert_eq!(code, 0, "{out}");
        assert_eq!(outcome(&out), Outcome::Success);
        let golden = root().join(format!("testdata/golden/vorbis/{name}.vtj"));
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
            std::fs::write(&golden, &out).unwrap();
        }
        assert_eq!(out, std::fs::read_to_string(&golden).unwrap(), "{name}");
        assert_eq!(run(&input).1, out, "rule 8");
    }
    let (_, out) = run(&root().join("testdata/media/vorbis_stereo.ogg"));
    // Xiph lacing prefix (2, 30, 62) inline, the three headers in the source.
    assert!(
        out.contains(r#""codec_private":[["inline","Ah4+"],["src",0,28,30],["src",0,102,62],["src",0,164,3832]]"#),
        "{out}"
    );
    let u = units(&out);
    assert!(
        u[0].starts_with(r#"{"type":"unit","pts_ns":0,"duration_ns":0,"#),
        "the first packet only primes: {}",
        u[0]
    );
    // 256/4 + 2048/4 samples: a short block followed by a long one.
    assert!(u[1].starts_with(r#"{"type":"unit","pts_ns":0,"duration_ns":13061224,"#), "{}", u[1]);
    assert!(u.last().unwrap().contains(r#""discard_padding_ns":11519274"#), "end trimming from the EOS granule");
}

#[test]
fn start_trimming_gives_negative_times() {
    let mut b = media("vorbis_stereo.ogg");
    let p = pages(&b);
    // Lower every audio page's granule by 100 samples: the first one now
    // completes 100 samples fewer than its packets decode.
    for &pg in &p[2..] {
        let g = granule(&b, pg);
        set_granule(&mut b, pg, g - 100);
    }
    let (code, out) = run(&temp("start.ogg", &b));
    assert_eq!(code, 0, "{out}");
    let u = units(&out);
    // -100 / 44100 s.
    assert!(u[1].starts_with(r#"{"type":"unit","pts_ns":-2267574,"#), "{}", u[1]);
}

#[test]
fn granule_mismatch_and_headers() {
    let b = media("vorbis_stereo.ogg");
    let p = pages(&b);
    let mut g = b.clone();
    let v = granule(&g, p[3]);
    set_granule(&mut g, p[3], v + 1);
    let (code, out) = run(&temp("granule.ogg", &g));
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert_eq!(outcome(&out), Outcome::Failure);
    assert!(error(&out).contains(&format!("granule position {} of page 3 does not match", v + 1)), "{out}");

    // The setup header's page with a granule position.
    let mut h = b.clone();
    set_granule(&mut h, p[1], 5);
    let (_, out) = run(&temp("hdr.ogg", &h));
    assert!(error(&out).contains("the page that ends the setup header has granule position 5 instead of 0"), "{out}");

    // A corrupted setup header: its codebook sync pattern.
    let mut s = b.clone();
    let setup = 164 + 7 + 1;
    s[setup] ^= 0xff;
    fix_crc(&mut s, p[1]);
    let (_, out) = run(&temp("setup.ogg", &s));
    assert!(error(&out).contains(r#""message":"third packet: codebook 0 has no sync pattern""#), "{out}");

    // Not Vorbis: the identification header's signature.
    let mut n = b.clone();
    n[29] = b'X';
    fix_crc(&mut n, p[0]);
    let (_, out) = run(&temp("notvorbis.ogg", &n));
    assert!(
        error(&out).contains(
            r#""code":"MISSING_INITIALIZATION_DATA","message":"first packet: not a Vorbis header of type 1""#
        ),
        "{out}"
    );
}

#[test]
fn truncated_and_corrupt_pages() {
    let b = media("vorbis_stereo.ogg");
    let p = pages(&b);
    let (code, out) = run(&temp("cut.ogg", &b[..b.len() - 10]));
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert!(error(&out).contains(r#""code":"TRUNCATED_BITSTREAM""#), "{out}");

    // Cut exactly at a page boundary: no end-of-stream page.
    let last = *p.last().unwrap();
    let (_, out) = run(&temp("noeos.ogg", &b[..last.0]));
    assert!(error(&out).contains("without an end-of-stream page"), "{out}");

    let mut c = b.clone();
    c[p[3].0 + 60] ^= 1;
    let (_, out) = run(&temp("crc.ogg", &c));
    assert!(error(&out).contains(&format!("CRC mismatch in page 3 at byte {}", p[3].0)), "{out}");
}

/// The page CRC stops almost every mutation before the setup parser sees
/// it, so the parser is fed every single-bit flip and every truncation of
/// a real setup header directly: it must return, never panic.
#[test]
fn setup_parser_survives_every_bit_flip_and_cut() {
    use vmkv_parser_vorbis::headers::parse_setup;
    let b = media("vorbis_stereo.ogg");
    let setup = b[164..164 + 3832].to_vec();
    assert!(parse_setup(&setup, 2).is_ok());
    let mut ok = 0;
    for bit in 0..setup.len() * 8 {
        let mut s = setup.clone();
        s[bit / 8] ^= 1 << (bit % 8);
        ok += parse_setup(&s, 2).is_ok() as usize;
    }
    for n in 0..setup.len() {
        assert!(parse_setup(&setup[..n], 2).is_err(), "cut at {n}");
    }
    // Most flips land in codebook lengths or lookup values, which the
    // parser skips without judging; a few must break the structure.
    assert!(ok < setup.len() * 8, "{ok}");
}
