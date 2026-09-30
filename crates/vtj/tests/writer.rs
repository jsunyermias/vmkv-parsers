//! The writer rejects records the validator would reject, instead of writing
//! an invalid file. In particular every integer must fit in ±(2^53 − 1).

use std::collections::BTreeMap;

use vtj::validate::{validate, Options, Outcome};
use vtj::*;

const TOO_BIG: u64 = 1 << 53;

fn header(size: u64) -> Header {
    Header {
        parser: ParserInfo { name: "p".into(), version: "1".into() },
        sources: vec![Source { id: 0, size, sha256: None, path: None }],
        params: BTreeMap::new(),
    }
}

fn audio() -> Track {
    let mut t = Track::new(TrackType::Audio, "A_MPEG/L3");
    t.audio = Some(Audio::new(Rational::new(44100, 1), 2));
    t
}

fn contract_error<T>(r: Result<T, WriteError>, needle: &str) {
    match r {
        Err(WriteError::Contract(m)) => assert!(m.contains(needle), "{m}"),
        Err(e) => panic!("unexpected error {e}"),
        Ok(_) => panic!("accepted a record containing {needle}"),
    }
}

#[test]
fn header_integers_out_of_range() {
    contract_error(VtjWriter::new(Vec::new()).header(&header(TOO_BIG)), "sources[0].size: 9007199254740992 is outside");
    let mut h = header(10);
    h.params.insert("n".into(), ParamValue::Int(-(TOO_BIG as i64)));
    contract_error(VtjWriter::new(Vec::new()).header(&h), "params.n");
    let mut h = header(10);
    h.params.insert("r".into(), ParamValue::Rational(Rational::new(TOO_BIG as i64, 1)));
    contract_error(VtjWriter::new(Vec::new()).header(&h), "params.r[0]");
}

#[test]
fn unit_integers_out_of_range() {
    let mut w2 = VtjWriter::new(Vec::new());
    w2.header(&header(100)).unwrap();
    let flags = Flags::NONE;
    contract_error(w2.unit(&Unit::new(TOO_BIG as i64, 1, flags, vec![])), "pts_ns");
    contract_error(w2.unit(&Unit::new(0, TOO_BIG as i64, flags, vec![])), "duration_ns");
    let mut u = Unit::new(0, 1, flags, vec![]);
    u.discard_padding_ns = Some(-(TOO_BIG as i64));
    contract_error(w2.unit(&u), "discard_padding_ns");
    contract_error(w2.unit(&Unit::new(0, 1, flags, vec![Chunk::src(TOO_BIG, 0, 1)])), "payload[0][1]");
}

#[test]
fn track_integers_and_reals_out_of_range() {
    let mut w = VtjWriter::new(Vec::new());
    w.header(&header(100)).unwrap();
    let mut t = audio();
    t.codec_delay_ns = Some(TOO_BIG as i64);
    contract_error(w.finish(&t), "codec_delay_ns");
    let mut t = audio();
    t.audio.as_mut().unwrap().channels = TOO_BIG;
    contract_error(w.finish(&t), "audio.channels");
    let mut t = Track::new(TrackType::Video, "V_VP9");
    let mut v = Video::new(TOO_BIG, 1);
    v.colour = Some(Colour {
        max_cll: Some(TOO_BIG),
        mastering: Some(Mastering { luminance_max: Some(f64::NAN), ..Default::default() }),
        ..Default::default()
    });
    v.projection = Some(Projection {
        kind: ProjectionType::Rectangular,
        private: None,
        yaw: Some(f64::INFINITY),
        pitch: None,
        roll: None,
    });
    t.video = Some(v);
    match w.finish(&t) {
        Err(WriteError::Contract(m)) => {
            for needle in ["video.pixel_width", "video.colour.max_cll", "mastering.luminance_max", "projection.yaw"] {
                assert!(m.contains(needle), "{needle} missing in {m}");
            }
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn largest_safe_values_are_written_and_validate() {
    let max = (1u64 << 53) - 1;
    let mut w = VtjWriter::new(Vec::new());
    w.header(&header(max)).unwrap();
    w.unit(&Unit::new(-(max as i64), max as i64, Flags::NONE, vec![Chunk::src(0, max - 1, 1)])).unwrap();
    w.finish(&audio()).unwrap();
    let out = w.into_inner();
    let r = validate(&out, &Options::default());
    assert_eq!(r.outcome, Outcome::Success, "{:?}", r.problems);
}
