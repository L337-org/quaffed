//! Each multi-byte codec reads byte strings exactly as `CPython` does.
//!
//! `scripts/generate-codecs` writes `tests/data/multi_byte.txt` from the oracle: byte strings,
//! many broken on purpose, with each character `CPython` decodes them to and where it starts,
//! `U+FFFD` where `CPython` reports an error.  This holds quaff's reading of every one to it, so a
//! table or a reading rule that drifts from `CPython`'s fails here rather than miscounting a
//! column in someone's file.

use quaffed_encoding::{Encoding, MultiByteCodec};

#[test]
fn every_multi_byte_codec_reads_as_cpython_does() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/multi_byte.txt");
    let vectors =
        std::fs::read_to_string(path).unwrap_or_else(|err| panic!("reading {path}: {err}"));
    let mut checked = 0;
    for (number, line) in vectors.lines().enumerate() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let mut fields = line.split(' ');
        let (Some(name), Some(hex)) = (fields.next(), fields.next()) else {
            panic!(
                "{path}:{}: expected a codec and bytes in {line:?}",
                number + 1
            );
        };
        let codec = MultiByteCodec::named(name)
            .unwrap_or_else(|| panic!("{path}:{}: quaff reads no codec {name:?}", number + 1));
        let bytes: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"))
            .collect();
        let expected: Vec<(usize, char)> = fields
            .map(|field| {
                let (at, code) = field.split_once(':').expect("offset:code point");
                (
                    at.parse().expect("an offset"),
                    char::from_u32(u32::from_str_radix(code, 16).expect("hex")).expect("a char"),
                )
            })
            .collect();
        let mut read = Vec::new();
        Encoding::MultiByte(codec).each_char(&bytes, |at, c| {
            read.push((at, c));
            true
        });
        assert_eq!(read, expected, "{path}:{}: {name} {hex}", number + 1);
        checked += 1;
    }
    // A file that held no strings would pass having checked nothing.
    assert!(checked >= 300, "only {checked} strings in {path}");
}
