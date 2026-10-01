//! WAV fingerprinting streams PCM frames into the existing 64 amplitude buckets;
//! it does not allocate a decoded sample vector proportional to recording length.

struct PcmWaveform<'a> {
    frames: &'a [u8],
    channels: usize,
    bits: u16,
    sample_width: usize,
}

pub(super) fn simhash(bytes: &[u8]) -> Option<u64> {
    let waveform = parse(bytes)?;
    let frame_width = waveform.sample_width.checked_mul(waveform.channels)?;
    let frame_count = waveform.frames.len() / frame_width;
    if frame_count == 0 {
        return Some(0);
    }
    let mut buckets = [0.0; 64];
    let mut counts = [0usize; 64];
    for (index, frame) in waveform.frames.chunks_exact(frame_width).enumerate() {
        let bucket = index * 64 / frame_count;
        buckets[bucket] += mixed_sample(frame, &waveform)?.abs();
        counts[bucket] += 1;
    }
    Some(bucket_hash(buckets, counts))
}

fn bucket_hash(mut buckets: [f64; 64], counts: [usize; 64]) -> u64 {
    for (bucket, count) in buckets.iter_mut().zip(counts) {
        if count > 0 {
            *bucket /= count as f64;
        }
    }
    let mean = buckets.iter().sum::<f64>() / 64.0;
    buckets
        .iter()
        .enumerate()
        .filter(|(_, bucket)| **bucket >= mean)
        .fold(0, |hash, (index, _)| hash | (1u64 << index))
}

fn parse(bytes: &[u8]) -> Option<PcmWaveform<'_>> {
    if bytes.len() < 44 || bytes.get(..4)? != b"RIFF" || bytes.get(8..12)? != b"WAVE" {
        return None;
    }
    let mut cursor: usize = 12;
    let mut format = None;
    let mut frames = None;
    while let Some(header) = bytes.get(cursor..cursor.checked_add(8)?) {
        let size = u32::from_le_bytes(header[4..8].try_into().ok()?) as usize;
        cursor += 8;
        let end = cursor.checked_add(size)?.min(bytes.len());
        match &header[..4] {
            b"fmt " if size >= 16 => format = Some(parse_format(bytes.get(cursor..end)?)?),
            b"data" => frames = bytes.get(cursor..end),
            _ => {}
        }
        cursor = end.checked_add(size % 2)?;
    }
    let (channels, bits) = format?;
    Some(PcmWaveform {
        frames: frames?,
        channels,
        bits,
        sample_width: usize::from(bits / 8),
    })
}

fn parse_format(format: &[u8]) -> Option<(usize, u16)> {
    if u16::from_le_bytes(format.get(..2)?.try_into().ok()?) != 1 {
        return None;
    }
    let channels = usize::from(u16::from_le_bytes(format.get(2..4)?.try_into().ok()?).max(1));
    let bits = u16::from_le_bytes(format.get(14..16)?.try_into().ok()?);
    matches!(bits, 8 | 16 | 24 | 32).then_some((channels, bits))
}

fn mixed_sample(frame: &[u8], waveform: &PcmWaveform<'_>) -> Option<f64> {
    let mut mixed = 0.0;
    for sample in frame.chunks_exact(waveform.sample_width) {
        mixed += pcm_sample(sample, waveform.bits)?;
    }
    Some(mixed / waveform.channels as f64)
}

fn pcm_sample(bytes: &[u8], bits: u16) -> Option<f64> {
    match bits {
        8 => Some((f64::from(*bytes.first()?) - 128.0) / 128.0),
        16 => Some(f64::from(i16::from_le_bytes(bytes.try_into().ok()?)) / 32768.0),
        24 => Some(
            f64::from(i32::from_le_bytes([
                bytes[0],
                bytes[1],
                bytes[2],
                if bytes[2] & 0x80 == 0 { 0 } else { 0xff },
            ])) / 8_388_608.0,
        ),
        32 => Some(f64::from(i32::from_le_bytes(bytes.try_into().ok()?)) / 2_147_483_648.0),
        _ => None,
    }
}
