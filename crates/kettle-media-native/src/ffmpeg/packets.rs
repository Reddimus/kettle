//! Bounded framecrc timestamps shared by the raw video and PCM pipes.

use std::io::BufRead;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Packet {
    pub stream: usize,
    pub at_us: u64,
    pub bytes: usize,
}

pub(super) struct Metadata<R> {
    input: R,
    line: Vec<u8>,
    video_bytes: usize,
    start_us: u64,
    end_us: u64,
    time_bases: [Option<(u64, u64)>; 2],
    seen: u8,
    headers: usize,
    started: bool,
    next_us: [Option<u64>; 2],
}

impl<R: BufRead> Metadata<R> {
    pub fn new(input: R, video_bytes: usize, start_ms: u64, duration_ms: u64) -> Self {
        Self {
            input,
            line: Vec::with_capacity(256),
            video_bytes,
            start_us: start_ms.saturating_mul(1000),
            end_us: duration_ms.saturating_mul(1000),
            time_bases: [None; 2],
            seen: 0,
            headers: 0,
            started: false,
            next_us: [None; 2],
        }
    }

    pub fn next(&mut self) -> Result<Option<Packet>, ()> {
        loop {
            self.line.clear();
            // read_until alone could grow without bound before finding a newline.
            let mut limited = self.input.by_ref().take(1025);
            use std::io::Read as _;
            let n = limited.read_until(b'\n', &mut self.line).map_err(|_| ())?;
            if n == 0 {
                return if self.next_us[0].is_some() {
                    Ok(None)
                } else {
                    Err(())
                };
            }
            if n > 1024 || self.line.last() != Some(&b'\n') {
                return Err(());
            }
            let owned = std::mem::take(&mut self.line);
            let text = std::str::from_utf8(&owned).map_err(|_| ())?.trim_end();
            let packet = if text.starts_with('#') {
                self.header(text)?;
                None
            } else {
                Some(self.packet(text)?)
            };
            self.line = owned;
            if let Some(packet) = packet {
                return Ok(Some(packet));
            }
        }
    }

    fn header(&mut self, text: &str) -> Result<(), ()> {
        self.headers += 1;
        if self.started || self.headers > 64 {
            return Err(());
        }
        if let Some(rest) = text.strip_prefix("#tb ") {
            let (index, value) = rest.split_once(": ").ok_or(())?;
            let index: usize = index.parse().map_err(|_| ())?;
            let (num, den) = super::ratio(value).ok_or(())?;
            let slot = self.time_bases.get_mut(index).ok_or(())?;
            if slot.replace((num, den)).is_some() || (index == 1 && (num, den) != (1, 48000)) {
                return Err(());
            }
        }
        for (bit, key, expected) in [
            (1, "#media_type 0: ", "video"),
            (2, "#codec_id 0: ", "rawvideo"),
            (4, "#media_type 1: ", "audio"),
            (8, "#codec_id 1: ", "pcm_f32le"),
            (16, "#sample_rate 1: ", "48000"),
            (32, "#channel_layout_name 1: ", "stereo"),
        ] {
            if let Some(value) = text.strip_prefix(key) {
                if value != expected || self.seen & bit != 0 {
                    return Err(());
                }
                self.seen |= bit;
            }
        }
        Ok(())
    }

    fn packet(&mut self, text: &str) -> Result<Packet, ()> {
        if self.seen != 63 || self.time_bases.iter().any(Option::is_none) {
            return Err(());
        }
        let mut fields = text.split(',').map(str::trim);
        let mut take = || fields.next().ok_or(());
        let stream: usize = take()?.parse().map_err(|_| ())?;
        let dts: u64 = take()?.parse().map_err(|_| ())?;
        let pts: u64 = take()?.parse().map_err(|_| ())?;
        let duration: u64 = take()?.parse().map_err(|_| ())?;
        let bytes: usize = take()?.parse().map_err(|_| ())?;
        let checksum = take()?.strip_prefix("0x").ok_or(())?;
        if fields.next().is_some()
            || checksum.len() != 8
            || !checksum.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(());
        }
        if stream > 1
            || dts != pts
            || duration == 0
            || (stream == 0 && (bytes != self.video_bytes || duration != 1))
            || (stream == 1 && (bytes != 7680 || duration != 960))
        {
            return Err(());
        }
        let (num, den) = self.time_bases[stream].ok_or(())?;
        let timestamp = |tick: u64| -> Result<u64, ()> {
            let us = u128::from(tick) * u128::from(num) * 1_000_000 / u128::from(den);
            self.start_us
                .checked_add(u64::try_from(us).map_err(|_| ())?)
                .ok_or(())
        };
        let at_us = timestamp(pts)?;
        let end_us = timestamp(pts.checked_add(duration).ok_or(())?)?;
        if at_us > self.end_us
            || end_us > self.end_us.saturating_add(1_000_000)
            || self.next_us[stream].is_some_and(|next| at_us < next)
        {
            return Err(());
        }
        self.started = true;
        self.next_us[stream] = Some(end_us);
        Ok(Packet {
            stream,
            at_us,
            bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: &str = "#software: Lavf63.1.102\n#tb 0: 1/25\n#media_type 0: video\n#codec_id 0: rawvideo\n#dimensions 0: 64x48\n#sar 0: 1/1\n#tb 1: 1/48000\n#media_type 1: audio\n#codec_id 1: pcm_f32le\n#sample_rate 1: 48000\n#channel_layout_name 1: stereo\n";

    #[test]
    fn video_and_audio_keep_their_common_timeline() {
        let text = format!(
            "{HEAD}1, 0, 0, 960, 7680, 0x00000000\n0, 3, 3, 1, 12288, 0x00000000\n1, 960, 960, 960, 7680, 0x00000000\n"
        );
        let mut metadata = Metadata::new(text.as_bytes(), 12288, 2500, 10_000);
        assert_eq!(
            metadata.next(),
            Ok(Some(Packet {
                stream: 1,
                at_us: 2_500_000,
                bytes: 7680
            }))
        );
        assert_eq!(
            metadata.next(),
            Ok(Some(Packet {
                stream: 0,
                at_us: 2_620_000,
                bytes: 12288
            }))
        );
        assert_eq!(
            metadata.next(),
            Ok(Some(Packet {
                stream: 1,
                at_us: 2_520_000,
                bytes: 7680
            }))
        );
        assert_eq!(metadata.next(), Ok(None));
    }

    #[test]
    fn audio_packets_cannot_stand_in_for_a_video() {
        let text = format!("{HEAD}1, 0, 0, 960, 7680, 0x00000000\n");
        let mut metadata = Metadata::new(text.as_bytes(), 12288, 0, 10_000);
        assert!(metadata.next().unwrap().is_some());
        assert_eq!(metadata.next(), Err(()));
    }

    #[test]
    fn malformed_or_unbounded_packets_are_refused() {
        for row in [
            "2, 0, 0, 960, 7680, 0x00000000",
            "1, 0, 0, 960, 7672, 0x00000000",
            "1, 0, 0, 959, 7680, 0x00000000",
            "0, 0, 0, 1, 12289, 0x00000000",
            "0, 0, -1, 1, 12288, 0x00000000",
            "0, 0, 999999999999999999999, 1, 12288, 0x00000000",
            "0, 0, 250, 1, 12288, 0x00000000",
            "0, 0, 0, 1, 12288, checksum",
        ] {
            let text = format!("{HEAD}{row}\n");
            assert_eq!(
                Metadata::new(text.as_bytes(), 12288, 2500, 10_000).next(),
                Err(()),
                "{row}"
            );
        }
        let text = format!("{HEAD}{}\n", " ".repeat(1025));
        assert_eq!(
            Metadata::new(text.as_bytes(), 12288, 0, 10_000).next(),
            Err(())
        );
    }

    #[test]
    fn headers_and_packet_order_cannot_change_midstream() {
        for tail in ["#tb 0: 1/30\n", "0, 0, 0, 1, 12288, 0x00000000\n"] {
            let text = format!("{HEAD}0, 0, 0, 1, 12288, 0x00000000\n{tail}");
            let mut metadata = Metadata::new(text.as_bytes(), 12288, 0, 10_000);
            assert!(metadata.next().unwrap().is_some());
            assert_eq!(metadata.next(), Err(()));
        }
        for head in [
            HEAD.replace("1/25", "0/25"),
            HEAD.replace("1/48000", "1/44100"),
            HEAD.replace("stereo", "mono"),
            HEAD.replace("pcm_f32le", "pcm_s16le"),
        ] {
            let text = format!("{head}1, 0, 0, 960, 7680, 0x00000000\n");
            assert_eq!(
                Metadata::new(text.as_bytes(), 12288, 0, 10_000).next(),
                Err(())
            );
        }
    }
}
