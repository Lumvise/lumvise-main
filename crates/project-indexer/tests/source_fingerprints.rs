use lumvise_project_indexer::SourceFingerprint;

#[test]
fn existing_text_format_is_bit_exact_for_empty_short_tokenized_and_unicode_content() {
    // Golden outputs captured from the existing sibling indexer's unchanged source,
    // not computed by a duplicate of the new implementation in this test.
    for (text, expected) in [
        ("", "fp1:0000000000000000:cbf29ce484222325"),
        ("fn example() {}", "fp1:a2f7e5d99d32d014:32ff6bed14e70cf0"),
        (
            "pub fn alpha(items: &[Item]) -> usize { items.iter().map(|item| item.count).sum() }",
            "fp1:2841a09d1745a427:c84e1c19e8a8ea30",
        ),
        (
            "fn café(Über: usize) -> usize { Über + lower_CASE + café + one + two + three }",
            "fp1:4a68b212e7e015c9:e0c2055395f0c0c6",
        ),
    ] {
        let fingerprint = SourceFingerprint::text(text);
        assert_eq!(fingerprint.encoded, expected);
        assert_eq!(fingerprint.algorithm, "text-token-simhash-fp1-v1");
    }
}

#[test]
fn image_and_streamed_waveform_match_existing_golden_outputs() {
    let png = SourceFingerprint::file("gradient.PNG", include_bytes!("fixtures/gradient.png"));
    assert_eq!(png.algorithm, "image-dhash-fp1-v1");
    assert_eq!(png.encoded, "fp1:0c08181818103030:e0012112efdcbc11");
    let wave = SourceFingerprint::file("gradient.wav", include_bytes!("fixtures/gradient.wav"));
    assert_eq!(wave.algorithm, "audio-waveform-fp1-v1");
    assert_eq!(wave.encoded, "fp1:ffffffff00000000:108c108fcb0f2c0a");
}

#[test]
fn truncated_wave_format_chunk_falls_back_without_aborting_the_scan() {
    let mut bytes = vec![0; 44];
    bytes[..4].copy_from_slice(b"RIFF");
    bytes[8..12].copy_from_slice(b"WAVE");
    bytes[12..16].copy_from_slice(b"JUNK");
    bytes[16..20].copy_from_slice(&16u32.to_le_bytes());
    bytes[36..40].copy_from_slice(b"fmt ");
    bytes[40..44].copy_from_slice(&16u32.to_le_bytes());
    let result = SourceFingerprint::file("truncated.wav", &bytes);
    assert_eq!(result.algorithm, "audio-byte-window-fp1-v1");
}

#[test]
fn source_kind_and_established_text_size_boundary_select_the_existing_algorithms() {
    for (path, algorithm) in [
        ("image.avif", "image-byte-window-fp1-v1"),
        ("audio.mp3", "audio-byte-window-fp1-v1"),
        ("video.mov", "video-byte-window-fp1-v1"),
        ("binary.bin", "file-byte-window-fp1-v1"),
    ] {
        assert_eq!(SourceFingerprint::file(path, &[0xff]).algorithm, algorithm);
    }
    assert_eq!(
        SourceFingerprint::file("text.txt", &vec![b'a'; 1_000_000]).algorithm,
        "text-token-simhash-fp1-v1"
    );
    assert_eq!(
        SourceFingerprint::file("text.txt", &vec![b'a'; 1_000_001]).algorithm,
        "file-byte-window-fp1-v1"
    );
}

fn fingerprint_probe_paths() -> (std::path::PathBuf, String) {
    let root = std::env::var("LUMVISE_FINGERPRINT_PROBE_ROOT")
        .expect("LUMVISE_FINGERPRINT_PROBE_ROOT missing; expected absolute project directory");
    let image = std::env::var("LUMVISE_FINGERPRINT_PROBE_IMAGE")
        .expect("LUMVISE_FINGERPRINT_PROBE_IMAGE missing; expected relative PNG path");
    let root = std::path::PathBuf::from(root);
    let path = std::path::Path::new(&image);
    assert!(
        root.is_absolute() && root.is_dir(),
        "root {root:?}; expected absolute existing project directory"
    );
    assert!(
        !path.is_absolute()
            && path
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_)))
            && path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("png")),
        "image {image:?}; expected relative PNG path without parent/current components"
    );
    (root, image)
}

#[test]
#[ignore = "requires LUMVISE_FINGERPRINT_PROBE_ROOT/IMAGE; real-file diagnostic, no timing guarantee"]
fn provisioned_png_read_sha_fingerprint_cpu_split() {
    use lumvise_project_indexer::{FilesystemProjectSource, ProjectSource};
    use sha2::{Digest, Sha256};
    use std::{hint::black_box, time::Instant};
    let (root, path) = fingerprint_probe_paths();
    let source = FilesystemProjectSource::open(&root).unwrap();
    let mut expected = None;
    for iteration in 0..3 {
        let started = Instant::now();
        let read = source.read_file(&path).unwrap();
        let read_us = started.elapsed().as_micros();
        let started = Instant::now();
        let digest = Sha256::digest(black_box(&read.bytes));
        let sha_us = started.elapsed().as_micros();
        let started = Instant::now();
        let fingerprint = SourceFingerprint::file(&path, black_box(&read.bytes));
        let fingerprint_us = started.elapsed().as_micros();
        let observed = (digest.to_vec(), fingerprint.clone());
        if let Some(expected) = &expected {
            assert_eq!(
                &observed, expected,
                "iteration {iteration}; expected identical immutable input digest and fp1"
            );
        }
        expected = Some(observed);
        let started = Instant::now();
        let decoded = image::load_from_memory(black_box(&read.bytes)).unwrap();
        let decode_us = started.elapsed().as_micros();
        let (width, height) = (decoded.width(), decoded.height());
        let started = Instant::now();
        let resized = decoded
            .resize_exact(9, 8, image::imageops::FilterType::Triangle)
            .to_luma8();
        black_box(resized);
        let resize_us = started.elapsed().as_micros();
        println!(
            "{}",
            serde_json::json!({"workload":"provisioned_png_fp1","debug_assertions":cfg!(debug_assertions),
            "iteration":iteration,"path":path,"bytes":read.bytes.len(),"width":width,"height":height,
            "read_us":read_us,"sha_us":sha_us,"fingerprint_us":fingerprint_us,"decode_us":decode_us,"resize_us":resize_us,
            "fingerprint":fingerprint.encoded,"algorithm":fingerprint.algorithm})
        );
    }
}
