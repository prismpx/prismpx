//! Fixture-based tests. Fixtures and their expected pixels are produced by an
//! independent Python encoder (scripts/gen_fixtures.py).

use std::fs;
use std::path::PathBuf;

use prismpx::{optimize, png, Effort, MetadataPolicy, Options};

fn fixtures() -> Vec<(String, Vec<u8>, Vec<u16>)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut out = Vec::new();
    for entry in fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "png") {
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            let data = fs::read(&path).unwrap();
            let raw = fs::read(path.with_extension("rgba16")).unwrap();
            let expected = raw
                .chunks_exact(2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .collect();
            out.push((name, data, expected));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(
        out.len() > 40,
        "fixtures missing; run scripts/gen_fixtures.py"
    );
    out
}

/// Chunk types present in a PNG, in order.
fn chunk_types(data: &[u8]) -> Vec<String> {
    let mut pos = 8;
    let mut out = Vec::new();
    while pos + 8 <= data.len() {
        let len = u32::from_be_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        out.push(String::from_utf8_lossy(&data[pos + 4..pos + 8]).into_owned());
        pos += 12 + len;
    }
    out
}

#[test]
fn decoder_matches_reference() {
    for (name, data, expected) in fixtures() {
        let img = png::decode(&data).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(img.to_rgba16(), expected, "{name}");
    }
}

#[test]
fn optimized_output_is_pixel_identical_and_not_larger() {
    for (name, data, expected) in fixtures() {
        for effort in [Effort::Fast, Effort::Default, Effort::Max] {
            for metadata in [
                MetadataPolicy::KeepAll,
                MetadataPolicy::Safe,
                MetadataPolicy::ColorOnly,
            ] {
                // verify=false so this test checks pixels itself instead of
                // relying on the library's own verification.
                let opts = Options::default()
                    .with_effort(effort)
                    .with_metadata(metadata)
                    .with_verify(false);
                let r = optimize(&data, &opts).unwrap_or_else(|e| panic!("{name}: {e}"));
                assert!(r.size() <= data.len(), "{name}: grew");
                let back = png::decode(&r.data).unwrap_or_else(|e| panic!("{name} output: {e}"));
                assert_eq!(back.to_rgba16(), expected, "{name} {effort:?} {metadata:?}");
            }
        }
    }
}

#[test]
fn wasteful_encodings_are_reduced() {
    let all = fixtures();
    let get = |n: &str| all.iter().find(|f| f.0 == n).unwrap().1.clone();
    let check = |n: &str, want: &str| {
        let r = optimize(&get(n), &Options::default()).unwrap();
        assert!(r.optimized, "{n}: {}", r.strategy);
        assert!(r.strategy.contains(want), "{n}: {}", r.strategy);
    };
    check("rgba8_is_gray4", "-> gray 4-bit");
    check("rgba8_five_colors", "-> palette 4-bit (5 colors)");
    check("rgb16_is_8bit", "8-bit");
    check("rgba8_binary_alpha", "tRNS key");
}

#[test]
fn metadata_policies() {
    let data = fixtures()
        .into_iter()
        .find(|f| f.0 == "meta_rgb8")
        .unwrap()
        .1;
    let types = |policy| {
        let opts = Options::default().with_metadata(policy);
        chunk_types(&optimize(&data, &opts).unwrap().data)
    };
    let has = |v: &[String], t: &str| v.iter().any(|c| c == t);

    let safe = types(MetadataPolicy::Safe);
    for t in ["gAMA", "pHYs", "eXIf"] {
        assert!(has(&safe, t), "safe keeps {t}: {safe:?}");
    }
    for t in ["tEXt", "tIME", "prVt", "prVT", "bKGD"] {
        assert!(!has(&safe, t), "safe drops {t}: {safe:?}");
    }

    let color = types(MetadataPolicy::ColorOnly);
    assert!(has(&color, "gAMA") && !has(&color, "pHYs") && !has(&color, "eXIf"));

    let all = types(MetadataPolicy::KeepAll);
    for t in ["gAMA", "pHYs", "eXIf", "tEXt", "tIME", "prVt"] {
        assert!(has(&all, t), "keep-all keeps {t}: {all:?}");
    }
    // Unsafe-to-copy unknown chunks must not survive re-encoding.
    assert!(!has(&all, "prVT"));
}

#[test]
fn rejects_corruption_without_panicking() {
    let data = fixtures()
        .into_iter()
        .find(|f| f.0 == "rgba8_gradient")
        .unwrap()
        .1;
    for i in (8..data.len()).step_by(7) {
        let mut bad = data.clone();
        bad[i] ^= 0x55;
        let _ = optimize(&bad, &Options::default()); // error or success, never panic
    }
    for cut in (0..data.len()).step_by(13) {
        let _ = optimize(&data[..cut], &Options::default());
    }
}

#[test]
fn keeps_original_when_nothing_helps() {
    let data = fixtures().into_iter().find(|f| f.0 == "gray1").unwrap().1;
    let once = optimize(&data, &Options::default()).unwrap();
    let twice = optimize(&once.data, &Options::default()).unwrap();
    assert!(twice.size() <= once.size());
}

#[test]
fn visually_lossless_meets_target_and_beats_lossless() {
    use prismpx::{metrics::ssimulacra2, Mode};
    for (name, data, _) in fixtures() {
        let original = png::decode(&data).unwrap();
        if original.width < 8 || original.height < 8 {
            continue;
        }
        let lossless = optimize(&data, &Options::default()).unwrap();
        for quality in [90.0f32, 80.0] {
            let opts = Options::default()
                .with_mode(Mode::VisuallyLossless)
                .with_quality(quality);
            let r = optimize(&data, &opts).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(
                r.size() <= lossless.size(),
                "{name}: lossy larger than lossless"
            );
            let out = png::decode(&r.data).unwrap();
            let score = ssimulacra2(&original, &out).unwrap();
            assert!(
                score >= quality as f64 - 1e-9,
                "{name} q{quality}: score {score} ({})",
                r.strategy
            );
        }
    }
}

#[test]
fn jpeg_is_optimized_through_the_public_api() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/jpeg");
    let data = fs::read(dir.join("baseline_420_q75.jpg")).unwrap();
    let r = optimize(&data, &Options::default()).unwrap();
    assert_eq!(r.format, prismpx::Format::Jpeg);
    assert!(r.optimized && r.size() < data.len(), "{}", r.strategy);
    // Visually-lossless mode falls back to lossless JPEG optimization for now.
    let v = optimize(
        &data,
        &Options::default().with_mode(prismpx::Mode::VisuallyLossless),
    )
    .unwrap();
    assert_eq!(v.data, r.data);
}
