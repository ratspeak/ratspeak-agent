//! Voice messages as 16-bit mono WAV for transcription. rsLXST decodes Ogg Opus and Codec2 700C,
//! the two formats Ratspeak records.
use lxst_core::OpusPacketDecoder;

const OPUS_OGG: u8 = 0x10;
const CODEC2_700C: u8 = 0x03;
const MAX_SECONDS: usize = 300;

/// The clip as WAV bytes and its length in seconds, or None for a mode or stream it cannot read.
pub fn wav(mode: u8, data: &[u8]) -> Option<(Vec<u8>, f32)> {
    let (rate, samples) = match mode {
        OPUS_OGG => (16_000, opus(data)?),
        CODEC2_700C => (8_000, codec2(data)?),
        _ => return None,
    };
    if samples.is_empty() {
        return None;
    }
    Some((encode(rate, &samples), samples.len() as f32 / rate as f32))
}

fn opus(data: &[u8]) -> Option<Vec<i16>> {
    let packets = ogg_packets(data)?;
    let head = packets.first()?;
    if head.len() < 19 || &head[..8] != b"OpusHead" || packets.get(1)?.get(..8)? != b"OpusTags" {
        return None;
    }
    let pre_skip = usize::from(u16::from_le_bytes([head[10], head[11]])) / 3; // 48 kHz samples at 16 kHz
    let mut decoder = OpusPacketDecoder::new(16_000, 1).ok()?;
    let mut out = Vec::new();
    for packet in &packets[2..] {
        let frame = decoder.decode_packet(packet).ok()?;
        out.extend(frame.samples.iter().map(|s| (s.clamp(-1.0, 1.0) * 32767.0) as i16));
        if out.len() >= MAX_SECONDS * 16_000 {
            out.truncate(MAX_SECONDS * 16_000);
            break;
        }
    }
    out.drain(..pre_skip.min(out.len()));
    Some(out)
}

/// The packets of a single-stream Ogg file. Lengths are bounded; CRCs are not checked (the message is signed).
fn ogg_packets(data: &[u8]) -> Option<Vec<Vec<u8>>> {
    let (mut packets, mut current, mut at, mut serial) = (Vec::new(), Vec::new(), 0, None);
    while at < data.len() {
        let header = data.get(at..at + 27)?;
        if &header[..4] != b"OggS" || header[4] != 0 {
            return None;
        }
        let page_serial = u32::from_le_bytes(header[14..18].try_into().ok()?);
        if *serial.get_or_insert(page_serial) != page_serial {
            return None;
        }
        let count = usize::from(header[26]);
        let mut body = at + 27 + count;
        for &size in data.get(at + 27..at + 27 + count)? {
            current.extend_from_slice(data.get(body..body + usize::from(size))?);
            body += usize::from(size);
            if size < 255 {
                packets.push(std::mem::take(&mut current));
            }
        }
        at = body;
    }
    Some(packets)
}

fn codec2(data: &[u8]) -> Option<Vec<i16>> {
    if data.is_empty() || !data.len().is_multiple_of(4) {
        return None;
    }
    let mut storage = Box::<lxst_codec2::Codec>::new_uninit();
    lxst_codec2::Codec::initialise(&mut storage, lxst_codec2::Mode::Rate700C);
    // SAFETY: initialise writes every field, as in Ratspeak's compact voice messages.
    let mut codec = unsafe { storage.assume_init() };
    let mut out = Vec::with_capacity((data.len() / 4).min(MAX_SECONDS * 25) * 320);
    for frame in data.chunks_exact(4).take(MAX_SECONDS * 25) {
        let mut pcm = [0i16; 320];
        codec.decode(frame, &mut pcm).ok()?;
        out.extend_from_slice(&pcm);
    }
    Some(out)
}

fn encode(rate: u32, samples: &[i16]) -> Vec<u8> {
    let bytes = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + samples.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + bytes).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&bytes.to_le_bytes());
    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use lxst_core::{OpusEncoderState, Profile, RawAudioFrame};

    fn page(serial: u32, sequence: u32, packets: &[Vec<u8>]) -> Vec<u8> {
        let mut lacing = Vec::new();
        for packet in packets {
            lacing.extend(std::iter::repeat_n(255u8, packet.len() / 255));
            lacing.push((packet.len() % 255) as u8);
        }
        let mut out = b"OggS\0\0".to_vec();
        out.extend_from_slice(&0u64.to_le_bytes());
        out.extend_from_slice(&serial.to_le_bytes());
        out.extend_from_slice(&sequence.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.push(lacing.len() as u8);
        out.extend_from_slice(&lacing);
        packets.iter().for_each(|p| out.extend_from_slice(p));
        out
    }

    /// An Ogg Opus clip of a 440 Hz tone, and its length in seconds after the pre-skip.
    fn ogg(frames: usize, pre_skip: u16) -> (Vec<u8>, f32) {
        let mut encoder = OpusEncoderState::new(Profile::QualityMedium).unwrap();
        let rate = 24_000.0;
        let mut head = b"OpusHead\x01\x01".to_vec();
        head.extend_from_slice(&pre_skip.to_le_bytes());
        head.extend_from_slice(&24_000u32.to_le_bytes());
        head.extend_from_slice(&[0, 0, 0]);
        let mut out = page(7, 0, &[head]);
        out.extend(page(7, 1, &[b"OpusTags\x08\0\0\0Ratspeak\0\0\0\0".to_vec()]));
        let audio: Vec<Vec<u8>> = (0..frames)
            .map(|n| {
                let count = encoder.sample_frames();
                let samples: Vec<f32> = (0..count)
                    .map(|i| (((n * count + i) as f32) * 440.0 * std::f32::consts::TAU / rate).sin() * 0.3)
                    .collect();
                encoder.encode_frame(&RawAudioFrame::new(1, samples).unwrap()).unwrap().payload
            })
            .collect();
        out.extend(page(7, 2, &audio));
        (out, (frames * encoder.sample_frames()) as f32 / rate - f32::from(pre_skip) / 48_000.0)
    }

    #[test]
    fn ogg_opus_becomes_16_khz_mono_wav_without_the_pre_skip() {
        let (data, expected) = ogg(25, 312);
        let (wav, seconds) = wav(OPUS_OGG, &data).unwrap();
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 16_000);
        let samples = (wav.len() - 44) / 2;
        assert_eq!(seconds, samples as f32 / 16_000.0);
        assert!((seconds - expected).abs() < 0.001, "{seconds} {expected}");
        assert!(wav[44..].chunks(2).any(|s| i16::from_le_bytes([s[0], s[1]]).abs() > 1000));
    }

    #[test]
    fn codec2_700c_frames_become_8_khz_wav() {
        let mut storage = Box::<lxst_codec2::Codec>::new_uninit();
        lxst_codec2::Codec::initialise(&mut storage, lxst_codec2::Mode::Rate700C);
        let mut codec = unsafe { storage.assume_init() };
        let mut data = Vec::new();
        for n in 0..25 {
            let pcm: Vec<i16> = (0..320).map(|i| ((((n * 320 + i) as f32) * 0.2).sin() * 8000.0) as i16).collect();
            let mut bytes = [0u8; 4];
            codec.encode(&pcm, &mut bytes).unwrap();
            data.extend_from_slice(&bytes);
        }
        let (wav, seconds) = wav(CODEC2_700C, &data).unwrap();
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 8_000);
        assert_eq!(seconds, 1.0);
    }

    #[test]
    fn unreadable_clips_are_refused() {
        let (good, _) = ogg(5, 0);
        for bad in [&b""[..], b"OggS", &good[..good.len() - 3], &good[1..]] {
            assert!(wav(OPUS_OGG, bad).is_none());
        }
        let mut two_streams = good.clone();
        two_streams.extend(page(8, 0, &[vec![1, 2, 3]]));
        assert!(wav(OPUS_OGG, &two_streams).is_none());
        assert!(wav(CODEC2_700C, &[1, 2, 3]).is_none());
        assert!(wav(0x16, &good).is_none());
    }
}
