//! Golden files for real encoder output, and streams re-muxed from it to
//! cover packets split across pages, start offsets and damaged pages.
//! Set `UPDATE_GOLDEN=1` to rewrite the golden files after an intended change.

use std::path::{Path, PathBuf};

use vmkv_parser_opus::ogg::{crc32, OggReader};
use vmkv_parser_opus::{packet_samples, Opus, MAX_PACKET_BYTES};
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
    let code = cli::run(&Opus, &[input.as_os_str().to_os_string()], &mut out, &mut err);
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
    let (code, out) = run(&temp("trim.opus", &bytes));
    assert_eq!(code, 0, "trimming over several packets of the EOS page: {out}");
    assert_eq!(outcome(&out), Outcome::Success);
    let t = timing(&out);
    let trimmed: Vec<_> = t.iter().filter(|u| u.2.is_some()).collect();
    assert!(trimmed.len() >= 5, "5000 samples span at least 5 packets of 960");
    assert!(trimmed[1..].iter().all(|u| u.2 == Some(u.1)), "packets after the audible end are discarded whole");
    let audible_end = trimmed[0].0 + trimmed[0].1 - trimmed[0].2.unwrap();
    assert_eq!(audible_end, vtj::ticks_to_ns((total - 5000 - 312) as i128, vtj::Rational::new(48000, 1)).unwrap());

    let bytes = remux(&pk, 4000, 0, 100);
    let (_, out) = run(&temp("trim_all.opus", &bytes));
    assert!(out.contains(r#""code":"UNREPRESENTABLE_IN_VMKV","message":"end trimming of"#), "{out}");
    assert!(out.contains("reaches before the last page"), "{out}");
}

#[test]
fn tags_page_must_have_granule_zero() {
    let pk = packets(&media("opus_stereo.opus"));
    let total: i64 = pk[2..].iter().map(|p| packet_samples(&p[..2]).unwrap() as i64).sum();
    let mut b = remux(&pk, 4000, 0, total);
    let at = 27 + 1 + pk[0].len();
    assert_eq!(&b[at..at + 4], b"OggS");
    b[at + 6..at + 14].copy_from_slice(&5i64.to_le_bytes());
    let nsegs = b[at + 26] as usize;
    let body: usize = b[at + 27..at + 27 + nsegs].iter().map(|&l| l as usize).sum();
    let len = 27 + nsegs + body;
    b[at + 22..at + 26].fill(0);
    let crc = crc32(&b[at..at + len]);
    b[at + 22..at + 26].copy_from_slice(&crc.to_le_bytes());
    let (code, out) = run(&temp("tags_granule.opus", &b));
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert_eq!(outcome(&out), Outcome::Failure);
    assert!(
        out.contains(r#""message":"the page that ends the OpusTags header has granule position 5 instead of 0""#),
        "{out}"
    );
}

fn last_page_at(b: &[u8]) -> usize {
    b.windows(4).rposition(|w| w == b"OggS").expect("at least one Ogg page")
}

/// Strips the `eos` flag from the real last page of `b` (recomputing its
/// CRC) so a page appended after it can act as the end of stream instead.
/// Returns the sequence number that page used, so the caller can continue
/// from `seq + 1`.
fn drop_eos_from_last_page(b: &mut [u8]) -> u32 {
    let at = last_page_at(b);
    let seq = u32::from_le_bytes(b[at + 18..at + 22].try_into().unwrap());
    b[at + 5] &= !4;
    let nsegs = b[at + 26] as usize;
    let body: usize = b[at + 27..at + 27 + nsegs].iter().map(|&l| l as usize).sum();
    let len = 27 + nsegs + body;
    b[at + 22..at + 26].fill(0);
    let crc = crc32(&b[at..at + len]);
    b[at + 22..at + 26].copy_from_slice(&crc.to_le_bytes());
    seq
}

/// RFC 3533 §4 allows a "nil" end-of-stream page: no lacing entries, so it
/// completes nothing, but it may still carry the position the stream
/// already reached, closing it cleanly.
#[test]
fn nil_end_of_stream_page_with_consistent_granule_is_accepted() {
    let pk = packets(&media("opus_stereo.opus"));
    let total: i64 = pk[2..].iter().map(|p| packet_samples(&p[..2]).unwrap() as i64).sum();
    let mut b = remux(&pk, 4000, 0, total);
    let seq = drop_eos_from_last_page(&mut b);
    b.extend(page(4, total, seq + 1, &[], &[]));
    let (code, out) = run(&temp("nil_eos.opus", &b));
    assert_eq!(code, 0, "{out}");
    assert_eq!(outcome(&out), Outcome::Success);
    assert!(!out.contains("discard_padding_ns"), "the granule was exact, nothing to trim: {out}");
}

/// A nil end-of-stream page whose granule contradicts the position already
/// reached is not a clean end of stream and must still be rejected.
#[test]
fn nil_end_of_stream_page_with_inconsistent_granule_is_rejected() {
    let pk = packets(&media("opus_stereo.opus"));
    let total: i64 = pk[2..].iter().map(|p| packet_samples(&p[..2]).unwrap() as i64).sum();
    let mut b = remux(&pk, 4000, 0, total);
    let seq = drop_eos_from_last_page(&mut b);
    b.extend(page(4, total + 1, seq + 1, &[], &[]));
    let (code, out) = run(&temp("nil_eos_bad.opus", &b));
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert_eq!(outcome(&out), Outcome::Failure);
    assert!(
        out.contains("nil end-of-stream page") && out.contains("inconsistent with the last known position"),
        "{out}"
    );
}

/// Cutting a stream exactly on a page boundary leaves no partial packet
/// behind, so losing the end-of-stream page entirely must still fail
/// (RFC 3533 requires a logical stream to end in an `eos` page); it must not
/// look like a clean, short-but-valid end of stream.
#[test]
fn truncation_at_every_page_boundary_fails_without_an_eos_page() {
    for name in ["opus_stereo.opus", "opus_mono_44k.opus"] {
        let b = std::fs::read(media(name)).unwrap();
        let starts: Vec<usize> = (0..b.len().saturating_sub(3)).filter(|&i| &b[i..i + 4] == b"OggS").collect();
        assert!(starts.len() > 2, "{name}: expected several pages, found {}", starts.len());
        // Skip offset 0 (an empty file) and require at least the BOS page.
        // Every such cut must fail (never a spurious clean success); cutting
        // right at the last page start — the reported bug — drops the whole
        // end-of-stream page and must name it specifically.
        for (i, &at) in starts[1..].iter().enumerate() {
            let (code, out) = run(&temp(&format!("{name}-cut-{at}.opus"), &b[..at]));
            assert_eq!(code, cli::EXIT_PARSE_ERROR, "{name} cut at byte {at}: {out}");
            assert_eq!(outcome(&out), Outcome::Failure, "{name} cut at byte {at}");
            assert!(out.contains(r#""code":"TRUNCATED_BITSTREAM""#), "{name} cut at byte {at}: {out}");
            if i + 1 == starts.len() - 1 {
                assert!(
                    out.ends_with(&format!(
                        "\"code\":\"TRUNCATED_BITSTREAM\",\"message\":\"stream ends at byte {at} without an end-of-stream page\"}}\n"
                    )),
                    "{name} cut right at the EOS page (byte {at}): {out}"
                );
            }
        }
    }
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

// --- Multistream (RFC 7845 §5.1.1 / RFC 6716 Appendix B) ---

fn multistream_head(channels: u8, stream_count: u8, coupled_count: u8, mapping: &[u8]) -> Vec<u8> {
    let mut h = b"OpusHead".to_vec();
    h.push(1); // version
    h.push(channels);
    h.extend(0u16.to_le_bytes()); // pre_skip
    h.extend(48_000u32.to_le_bytes()); // input_sample_rate
    h.extend(0i16.to_le_bytes()); // output_gain
    h.push(1); // mapping family (non-zero: stream/mapping table follows)
    h.push(stream_count);
    h.push(coupled_count);
    h.extend_from_slice(mapping);
    h
}

fn empty_tags() -> Vec<u8> {
    let mut t = b"OpusTags".to_vec();
    t.extend(0u32.to_le_bytes()); // vendor string length
    t.extend(0u32.to_le_bytes()); // comment list length
    t
}

/// `stream_count` independent code-0 (single frame, config 0 = 480 samples)
/// packets, 3 bytes of frame data each, concatenated per RFC 6716 Appendix
/// B: every one but the last is self-delimited (an explicit length byte).
fn multistream_audio_packet(stream_count: u8) -> Vec<u8> {
    let mut out = Vec::new();
    for i in 0..stream_count {
        out.push(0x00);
        if i + 1 < stream_count {
            out.push(3);
        }
        out.extend([0xAA ^ i, 0xBB ^ i, 0xCC ^ i]);
    }
    out
}

/// A minimal multistream Ogg Opus file: OpusHead (`stream_count` streams),
/// OpusTags, and one audio page carrying one multistream packet (480
/// samples, matching `multistream_audio_packet`'s code-0 480-sample
/// frames) as the exact, untrimmed end of stream.
fn multistream_file(stream_count: u8, coupled_count: u8, channels: u8, mapping: &[u8], audio: &[u8]) -> Vec<u8> {
    let head = multistream_head(channels, stream_count, coupled_count, mapping);
    let tags = empty_tags();
    let mut out = page(2, 0, 0, &lacing(&head), &head);
    out.extend(page(0, 0, 1, &lacing(&tags), &tags));
    out.extend(page(4, 480, 2, &lacing(audio), audio));
    out
}

#[test]
fn multistream_audio_is_split_and_validated() {
    let audio = multistream_audio_packet(2);
    let f = multistream_file(2, 0, 2, &[0, 1], &audio);
    let path = temp("multistream.opus", &f);
    let (code, out) = run(&path);
    assert_eq!(code, 0, "{out}");
    assert_eq!(outcome(&out), Outcome::Success);
    assert_eq!(units(&out).len(), 1);
    assert_eq!(
        field(units(&out)[0], "duration_ns"),
        Some(vtj::ticks_to_ns(480, vtj::Rational::new(48_000, 1)).unwrap())
    );

    // The whole multistream packet stays the payload, unsplit (decision 47).
    let src = std::fs::read(&path).unwrap();
    let mut rebuilt: Vec<u8> = Vec::new();
    for c in units(&out)[0].split("[\"src\",0,").skip(1) {
        let mut it = c.split([',', ']']);
        let (o, l): (usize, usize) = (it.next().unwrap().parse().unwrap(), it.next().unwrap().parse().unwrap());
        rebuilt.extend(&src[o..o + l]);
    }
    assert_eq!(rebuilt, audio);
}

#[test]
fn multistream_missing_a_stream_is_rejected() {
    // Stream 0's complete, well-formed self-delimited bytes, but nothing
    // at all for stream 1, even though the header declares two streams.
    let audio = multistream_audio_packet(2)[..5].to_vec();
    let f = multistream_file(2, 0, 2, &[0, 1], &audio);
    let (code, out) = run(&temp("multistream_short.opus", &f));
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert_eq!(outcome(&out), Outcome::Failure);
    assert!(out.contains(r#""code":"INVALID_BITSTREAM""#) && out.contains("stream 1"), "{out}");
}

#[test]
fn multistream_truncated_self_delimited_length_is_rejected() {
    // Stream 0 (self-delimited) declares a 3-byte frame but only 2 bytes of
    // it (and nothing for stream 1) are actually present.
    let audio = vec![0x00, 3, 0xAA, 0xBB];
    let f = multistream_file(2, 0, 2, &[0, 1], &audio);
    let (code, out) = run(&temp("multistream_cut.opus", &f));
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert_eq!(outcome(&out), Outcome::Failure);
    assert!(
        out.contains(r#""code":"INVALID_BITSTREAM""#) && out.contains("stream 0") && out.contains("exceeds the packet"),
        "{out}"
    );
}

#[test]
fn multistream_trailing_bytes_are_absorbed_by_the_last_stream() {
    // The last of the `stream_count` streams uses normal (non-self-delimited)
    // framing by design (RFC 6716 Appendix B): its length is never encoded,
    // only implied by "whatever is left" of the Ogg-level packet. A stray
    // byte appended after it is therefore indistinguishable from one more
    // byte of real frame data — exactly as a real decoder would also see
    // it, since Ogg's own lacing, not Opus framing, delimits the packet.
    // This documents that property rather than asserting a rejection that
    // the format gives no way to make.
    let mut audio = multistream_audio_packet(2);
    audio.push(0xFF);
    let f = multistream_file(2, 0, 2, &[0, 1], &audio);
    let path = temp("multistream_trailing.opus", &f);
    let (code, out) = run(&path);
    assert_eq!(code, 0, "{out}");
    assert_eq!(outcome(&out), Outcome::Success);
    let src = std::fs::read(&path).unwrap();
    let mut rebuilt: Vec<u8> = Vec::new();
    for c in units(&out)[0].split("[\"src\",0,").skip(1) {
        let mut it = c.split([',', ']']);
        let (o, l): (usize, usize) = (it.next().unwrap().parse().unwrap(), it.next().unwrap().parse().unwrap());
        rebuilt.extend(&src[o..o + l]);
    }
    assert_eq!(rebuilt, audio, "the stray byte is absorbed into stream 1's payload, not rejected");
}

#[test]
fn multistream_mismatched_durations_are_rejected() {
    // Stream 0: code 0, config 0 (480 samples), self-delimited, 3 bytes.
    // Stream 1: code 0, config 1 (960 samples: a different duration).
    let mut audio = vec![0x00, 3, 0xAA, 0xBB, 0xCC];
    audio.extend([0x08, 0xDD, 0xEE, 0xFF]); // config 1 << 3 = 0x08
    let f = multistream_file(2, 0, 2, &[0, 1], &audio);
    let (code, out) = run(&temp("multistream_mismatch.opus", &f));
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert_eq!(outcome(&out), Outcome::Failure);
    assert!(out.contains("stream 1 lasts") && out.contains("stream 0 lasts"), "{out}");
}

// --- Single-stream (mapping family 0) packets go through the same framing
// parser as multistream ones (decision 52), not just a duration-only read.

fn mono_or_stereo_head(channels: u8) -> Vec<u8> {
    let mut h = b"OpusHead".to_vec();
    h.push(1); // version
    h.push(channels);
    h.extend(0u16.to_le_bytes()); // pre_skip
    h.extend(48_000u32.to_le_bytes()); // input_sample_rate
    h.extend(0i16.to_le_bytes()); // output_gain
    h.push(0); // mapping family 0: no stream/mapping table
    h
}

fn single_stream_file(channels: u8, audio: &[u8]) -> Vec<u8> {
    let head = mono_or_stereo_head(channels);
    let tags = empty_tags();
    let mut out = page(2, 0, 0, &lacing(&head), &head);
    out.extend(page(0, 0, 1, &lacing(&tags), &tags));
    out.extend(page(4, 480, 2, &lacing(audio), audio));
    out
}

fn assert_rejected(f: &[u8], name: &str, needle: &str) {
    let (code, out) = run(&temp(name, f));
    assert_eq!(code, cli::EXIT_PARSE_ERROR, "{out}");
    assert_eq!(outcome(&out), Outcome::Failure, "{out}");
    assert!(out.contains(needle), "expected {needle:?} in: {out}");
}

#[test]
fn single_stream_code1_odd_length_is_rejected() {
    // TOC 0x01: config 0, code 1 (two equal CBR frames); a single leftover
    // byte cannot split evenly in two. Before validating single-stream
    // packets with the same framing parser as multistream ones, only the
    // duration (480 samples from the TOC alone) was checked, and this
    // packet passed as a clean, if nonsensical, 20 ms unit.
    let f = single_stream_file(2, &[0x01, 0xAA]);
    assert_rejected(&f, "code1_odd.opus", "odd number of bytes");
}

#[test]
fn single_stream_code2_frame_length_exceeding_the_packet_is_rejected() {
    // TOC 0x02: code 2, frame 1 declared 200 bytes long, only 2 remain.
    let f = single_stream_file(2, &[0x02, 200, 0xAA, 0xBB]);
    assert_rejected(&f, "code2_bad.opus", "frame 1 is longer than the packet");
}

#[test]
fn single_stream_code3_zero_frames_is_rejected() {
    // TOC 0x03: code 3, frame count byte 0 (no frames, no padding, CBR).
    let f = single_stream_file(2, &[0x03, 0x00]);
    assert_rejected(&f, "code3_zero.opus", "out of range");
}

#[test]
fn single_stream_frame_over_1275_bytes_is_rejected() {
    // TOC 0x00: code 0, a single implicit-length frame of 1276 bytes —
    // one more than RFC 6716's per-frame cap.
    let mut audio = vec![0x00];
    audio.extend(vec![0xABu8; 1276]);
    let f = single_stream_file(2, &audio);
    assert_rejected(&f, "toolong.opus", "longer than 1275 bytes");
}

#[test]
fn a_packet_over_the_size_limit_is_rejected_before_reading_it() {
    // RFC 7845 §6: bigger than this, and it is refused outright rather than
    // read into memory at all, regardless of what framing it would decode
    // to (a single Ogg page's body can hold more than this on its own).
    let mut audio = vec![0x00];
    audio.extend(vec![0u8; MAX_PACKET_BYTES as usize]);
    let f = single_stream_file(2, &audio);
    assert_rejected(&f, "oversized.opus", "byte limit");
}
