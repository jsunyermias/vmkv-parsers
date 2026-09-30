//! Golden files for real encoder output, and streams re-muxed from it to
//! cover packets split across pages, start offsets and damaged pages.
//! Set `UPDATE_GOLDEN=1` to rewrite the golden files after an intended change.

use std::path::{Path, PathBuf};

use vmkv_ogg::{crc32, OggReader};
use vmkv_parser_opus::{packet_samples, Opus};
use vtj::cli;
use vtj::source::SourceFile;
use vtj::validate::{validate, Options, Outcome};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn media(name: &str) -> PathBuf {
    root().join("testdata/media").join(name)
}

fn run(input: &Path) -> (i32, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = cli::run(&Opus, &[input.to_string_lossy().into_owned()], &mut out, &mut err);
    (code, String::from_utf8(out).unwrap())
}

fn temp(name: &str, bytes: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vmkv-opus-{}", std::process::id()));
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

fn field(line: &str, key: &str) -> Option<i64> {
    let i = line.find(&format!("\"{key}\":"))? + key.len() + 3;
    let end = line[i..].find(|c: char| c != '-' && !c.is_ascii_digit()).map_or(line.len(), |e| i + e);
    line[i..end].parse().ok()
}

fn timing(out: &str) -> Vec<(i64, i64, Option<i64>)> {
    units(out)
        .iter()
        .map(|u| (field(u, "pts_ns").unwrap(), field(u, "duration_ns").unwrap(), field(u, "discard_padding_ns")))
        .collect()
}

fn packets(path: &Path) -> Vec<Vec<u8>> {
    let mut src = SourceFile::open(0, path).unwrap();
    let mut r = OggReader::new(&src);
    let mut v = Vec::new();
    while let Some(p) = r.next_packet(&mut src).unwrap() {
        v.push(p.read(&mut src).unwrap());
    }
    v
}

fn page(flags: u8, granule: i64, seq: u32, lacing: &[u8], body: &[u8]) -> Vec<u8> {
    let mut p = b"OggS".to_vec();
    p.extend([0, flags]);
    p.extend(granule.to_le_bytes());
    p.extend(0x1234u32.to_le_bytes());
    p.extend(seq.to_le_bytes());
    p.extend([0; 4]);
    p.push(lacing.len() as u8);
    p.extend(lacing);
    p.extend(body);
    let c = crc32(&p);
    p[22..26].copy_from_slice(&c.to_le_bytes());
    p
}

fn lacing(p: &[u8]) -> Vec<u8> {
    let mut l = vec![255u8; p.len() / 255];
    l.push((p.len() % 255) as u8);
    l
}

/// Re-muxes head, tags and audio packets into pages whose body holds at most
/// `max_body` bytes, starting the audio at granule `start` and ending with
/// `final_granule` on the EOS page.
fn remux(pk: &[Vec<u8>], max_body: usize, start: i64, final_granule: i64) -> Vec<u8> {
    let mut out = page(2, 0, 0, &lacing(&pk[0]), &pk[0]);
    out.extend(page(0, 0, 1, &lacing(&pk[1]), &pk[1]));
    let mut seq = 2;
    let mut granule = start;
    let (mut lace, mut body) = (Vec::new(), Vec::new());
    let mut page_continued = false;
    let mut page_granule = -1;
    for p in &pk[2..] {
        let mut data = &p[..];
        let mut in_packet = false;
        for s in lacing(p) {
            if body.len() + s as usize > max_body || lace.len() == 255 {
                out.extend(page(page_continued as u8, page_granule, seq, &lace, &body));
                seq += 1;
                lace.clear();
                body.clear();
                page_continued = in_packet;
                page_granule = -1;
            }
            lace.push(s);
            body.extend(&data[..s as usize]);
            data = &data[s as usize..];
            in_packet = true;
        }
        granule += packet_samples(&p[..2.min(p.len())]).unwrap() as i64;
        page_granule = granule;
    }
    out.extend(page(page_continued as u8 | 4, final_granule, seq, &lace, &body));
    out
}

#[test]
fn golden_outputs() {
    for name in ["opus_stereo", "opus_mono_44k"] {
        let (code, out) = run(&media(&format!("{name}.opus")));
        assert_eq!(code, 0, "{out}");
        assert_eq!(outcome(&out), Outcome::Success);
        let golden = root().join(format!("testdata/golden/opus/{name}.vtj"));
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            std::fs::write(&golden, &out).unwrap();
        }
        assert_eq!(out, std::fs::read_to_string(&golden).unwrap(), "{name}");
        assert_eq!(run(&media(&format!("{name}.opus"))).1, out, "rule 8: {name}");
        let t = timing(&out);
        assert_eq!(t[0].0, -6_500_000, "pre-skip 312 puts the first packet at -6.5 ms (rule 4)");
        let (pts, dur, discard) = t[t.len() - 1];
        assert_eq!(pts + dur - discard.unwrap(), 1_000_000_000, "{name}: end trimming ends exactly at 1 s");
        assert!(
            out.contains(r#""codec_private":[["src",0,28,19]],"codec_delay_ns":6500000,"seek_preroll_ns":80000000"#)
        );
    }
}

#[test]
fn packets_split_across_pages() {
    let pk = packets(&media("opus_stereo.opus"));
    let total: i64 = pk[2..].iter().map(|p| packet_samples(&p[..2]).unwrap() as i64).sum();
    let original = run(&media("opus_stereo.opus")).1;
    let trim = timing(&original).last().unwrap().2.unwrap() * 48 / 1_000_000;
    let bytes = remux(&pk, 100, 0, total - trim);
    let (code, out) = run(&temp("split.opus", &bytes));
    assert_eq!(code, 0, "{out}");
    assert_eq!(outcome(&out), Outcome::Success);
    assert_eq!(timing(&out), timing(&original), "same timeline whatever the paging");
    let splittable = pk[2..].iter().filter(|p| p.len() > 255).count();
    assert!(splittable > 0);
    let split = units(&out).iter().filter(|u| u.matches("[\"src\"").count() > 1).count();
    assert_eq!(split, splittable, "every packet longer than one lacing segment spans two pages");
    let src = std::fs::read(temp("split.opus", &bytes)).unwrap();
    for (u, p) in units(&out).iter().zip(&pk[2..]) {
        let mut rebuilt: Vec<u8> = Vec::new();
        for c in u.split("[\"src\",0,").skip(1) {
            let mut it = c.split([',', ']']);
            let (o, l): (usize, usize) = (it.next().unwrap().parse().unwrap(), it.next().unwrap().parse().unwrap());
            rebuilt.extend(&src[o..o + l]);
        }
        assert_eq!(&rebuilt, p);
    }
}

#[test]
fn nonzero_start_granule_shifts_the_timeline() {
    let pk = packets(&media("opus_stereo.opus"));
    let total: i64 = pk[2..].iter().map(|p| packet_samples(&p[..2]).unwrap() as i64).sum();
    let bytes = remux(&pk, 4000, 48_000, 48_000 + total);
    let (code, out) = run(&temp("offset.opus", &bytes));
    assert_eq!(code, 0, "{out}");
    let t = timing(&out);
    assert_eq!(t[0].0, 1_000_000_000 - 6_500_000);
    assert_eq!(t.last().unwrap().2, None, "no end trimming");
}

#[test]
fn granule_mismatch_and_bad_trimming() {
    let pk = packets(&media("opus_stereo.opus"));
    let total: i64 = pk[2..].iter().map(|p| packet_samples(&p[..2]).unwrap() as i64).sum();
    let bytes = remux(&pk, 4000, 0, total + 1);
    let (code, out) = run(&temp("over.opus", &bytes));
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert_eq!(outcome(&out), Outcome::Failure);
    assert!(out.contains(r#""code":"INVALID_BITSTREAM","message":"granule position"#), "{out}");

    let bytes = remux(&pk, 4000, 0, total - 5000);
    let (_, out) = run(&temp("trim.opus", &bytes));
    assert!(
        out.contains(
            r#""code":"UNREPRESENTABLE_IN_VMKV","message":"end trimming of 5000 samples exceeds the last packet""#
        ),
        "{out}"
    );
}

#[test]
fn damaged_files() {
    let b = std::fs::read(media("opus_stereo.opus")).unwrap();
    let (code, out) = run(&temp("cut.opus", &b[..b.len() - 50]));
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert_eq!(outcome(&out), Outcome::Failure);
    assert!(out.contains(r#""code":"TRUNCATED_BITSTREAM""#), "{out}");

    let mut c = b.clone();
    c[1000] ^= 0xff;
    let (_, out) = run(&temp("crc.opus", &c));
    assert!(out.contains(r#""code":"INVALID_BITSTREAM","message":"CRC mismatch in page"#), "{out}");

    let pk = packets(&media("opus_stereo.opus"));
    let mut no_head = pk.clone();
    no_head[0] = b"NotOpusHeadXXXXXXXXX".to_vec();
    let (_, out) = run(&temp("nohead.opus", &remux(&no_head, 4000, 0, 0)));
    assert!(out.contains(r#""code":"MISSING_INITIALIZATION_DATA""#), "{out}");
}
