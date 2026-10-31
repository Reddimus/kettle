//! Bounded framecrc timestamps for one independently decoded stream.

use std::io::BufRead;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Stream {
    Video,
    Audio,
}

impl Stream {
    pub fn index(self) -> usize {
        match self {
            Self::Video => 0,
            Self::Audio => 1,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Packet {
    pub stream: usize,
    pub at_us: u64,
    pub bytes: usize,
}

pub(super) struct Metadata<R> {
    input: R,
    stream: Stream,
    line: Vec<u8>,
    video_bytes: usize,
    start_us: u64,
    end_us: u64,
    time_base: Option<(u64, u64)>,
    seen: u8,
    headers: usize,
    next_us: Option<u64>,
}

impl<R: BufRead> Metadata<R> {
    pub fn new(
        input: R,
        stream: Stream,
        video_bytes: usize,
        start_ms: u64,
        duration_ms: u64,
    ) -> Self {
        Self {
            input,
            stream,
            line: Vec::with_capacity(256),
            video_bytes,
            start_us: start_ms.saturating_mul(1000),
            end_us: duration_ms.saturating_mul(1000),
            time_base: None,
            seen: 0,
            headers: 0,
            next_us: None,
        }
    }

    pub fn next(&mut self) -> Result<Option<Packet>, ()> {
        loop {
            self.line.clear();
            let mut limited = self.input.by_ref().take(1025);
            use std::io::Read as _;
            let n = limited.read_until(b'\n', &mut self.line).map_err(|_| ())?;
            if n == 0 {
                return if self.next_us.is_some()
                    || (self.stream == Stream::Audio && self.valid_headers())
                {
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
            if packet.is_some() {
                return Ok(packet);
            }
        }
    }

    fn header(&mut self, text: &str) -> Result<(), ()> {
        self.headers += 1;
        if self.next_us.is_some() || self.headers > 64 {
            return Err(());
        }
        if let Some(rest) = text.strip_prefix("#tb ") {
            let (index, value) = rest.split_once(": ").ok_or(())?;
            let (num, den) = super::ratio(value).ok_or(())?;
            if index != "0"
                || self.time_base.replace((num, den)).is_some()
                || (self.stream == Stream::Audio && (num, den) != (1, 48000))
                || (self.stream == Stream::Video && u128::from(num) > u128::from(den) * 1000)
            {
                return Err(());
            }
        }
        let (media, codec) = match self.stream {
            Stream::Video => ("video", "rawvideo"),
            Stream::Audio => ("audio", "pcm_f32le"),
        };
        for (bit, key, expected) in [
            (1, "#media_type 0: ", media),
            (2, "#codec_id 0: ", codec),
            (4, "#sample_rate 0: ", "48000"),
            (8, "#channel_layout_name 0: ", "stereo"),
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

    fn valid_headers(&self) -> bool {
        let required = if self.stream == Stream::Video { 3 } else { 15 };
        self.seen == required && self.time_base.is_some()
    }

    fn packet(&mut self, text: &str) -> Result<Packet, ()> {
        if !self.valid_headers() {
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
        if stream != 0
            || dts != pts
            || duration == 0
            || (self.stream == Stream::Video && (bytes != self.video_bytes || duration != 1))
            || (self.stream == Stream::Audio && (bytes != 7680 || duration != 960))
        {
            return Err(());
        }
        let (num, den) = self.time_base.ok_or(())?;
        let timestamp = |tick: u64| -> Result<u64, ()> {
            let us = u128::from(tick) * u128::from(num) * 1_000_000 / u128::from(den);
            self.start_us
                .checked_add(u64::try_from(us).map_err(|_| ())?)
                .ok_or(())
        };
        let at_us = timestamp(pts)?;
        let end_us = timestamp(pts.checked_add(duration).ok_or(())?)?;
        let allowance = if self.stream == Stream::Video {
            u64::try_from((u128::from(num) * 1_000_000).div_ceil(u128::from(den)))
                .map_err(|_| ())?
        } else {
            1_000_000
        };
        if at_us > self.end_us
            || end_us > self.end_us.saturating_add(allowance)
            || self.next_us.is_some_and(|next| at_us < next)
        {
            return Err(());
        }
        self.next_us = Some(end_us);
        Ok(Packet {
            stream: self.stream.index(),
            at_us,
            bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const VIDEO: &str = "#tb 0: 1/25\n#media_type 0: video\n#codec_id 0: rawvideo\n";
    const AUDIO: &str = "#tb 0: 1/48000\n#media_type 0: audio\n#codec_id 0: pcm_f32le\n#sample_rate 0: 48000\n#channel_layout_name 0: stereo\n";

    #[test]
    fn independent_streams_keep_the_common_timeline_and_interior_gaps() {
        let video = format!("{VIDEO}0, 3, 3, 1, 12288, 0x00000000\n");
        let audio = format!(
            "{AUDIO}0, 0, 0, 960, 7680, 0x00000000\n0, 96000, 96000, 960, 7680, 0x00000000\n"
        );
        let mut v = Metadata::new(video.as_bytes(), Stream::Video, 12288, 2500, 10_000);
        let mut a = Metadata::new(audio.as_bytes(), Stream::Audio, 12288, 2500, 10_000);
        assert_eq!(
            v.next(),
            Ok(Some(Packet {
                stream: 0,
                at_us: 2_620_000,
                bytes: 12288
            }))
        );
        assert_eq!(
            a.next(),
            Ok(Some(Packet {
                stream: 1,
                at_us: 2_500_000,
                bytes: 7680
            }))
        );
        assert_eq!(
            a.next(),
            Ok(Some(Packet {
                stream: 1,
                at_us: 4_500_000,
                bytes: 7680
            }))
        );
        assert_eq!(v.next(), Ok(None));
        assert_eq!(a.next(), Ok(None));
    }

    #[test]
    fn a_low_rate_final_packet_may_span_the_end() {
        let text = format!(
            "{}0, 4, 4, 1, 12288, 0x00000000\n",
            VIDEO.replace("1/25", "2/1")
        );
        let mut metadata = Metadata::new(text.as_bytes(), Stream::Video, 12288, 0, 8500);
        assert_eq!(
            metadata.next(),
            Ok(Some(Packet {
                stream: 0,
                at_us: 8_000_000,
                bytes: 12288
            }))
        );
    }

    #[test]
    fn malformed_or_unbounded_packets_are_refused() {
        for row in [
            "1, 0, 0, 1, 12288, 0x00000000",
            "0, 0, 0, 1, 12289, 0x00000000",
            "0, 0, -1, 1, 12288, 0x00000000",
            "0, 0, 999999999999999999999, 1, 12288, 0x00000000",
            "0, 0, 250, 1, 12288, 0x00000000",
            "0, 0, 0, 1, 12288, checksum",
        ] {
            let text = format!("{VIDEO}{row}\n");
            assert_eq!(
                Metadata::new(text.as_bytes(), Stream::Video, 12288, 2500, 10_000).next(),
                Err(()),
                "{row}"
            );
        }
        for row in [
            "0, 0, 0, 960, 7672, 0x00000000",
            "0, 0, 0, 959, 7680, 0x00000000",
        ] {
            let text = format!("{AUDIO}{row}\n");
            assert_eq!(
                Metadata::new(text.as_bytes(), Stream::Audio, 12288, 0, 10_000).next(),
                Err(())
            );
        }
        let text = format!("{VIDEO}{}\n", " ".repeat(1025));
        assert_eq!(
            Metadata::new(text.as_bytes(), Stream::Video, 12288, 0, 10_000).next(),
            Err(())
        );
    }

    #[test]
    fn headers_and_packet_order_cannot_change_midstream() {
        for tail in ["#tb 0: 1/30\n", "0, 0, 0, 1, 12288, 0x00000000\n"] {
            let text = format!("{VIDEO}0, 0, 0, 1, 12288, 0x00000000\n{tail}");
            let mut metadata = Metadata::new(text.as_bytes(), Stream::Video, 12288, 0, 10_000);
            assert!(metadata.next().unwrap().is_some());
            assert_eq!(metadata.next(), Err(()));
        }
        for head in [
            AUDIO.replace("1/48000", "1/44100"),
            AUDIO.replace("stereo", "mono"),
            AUDIO.replace("pcm_f32le", "pcm_s16le"),
        ] {
            let text = format!("{head}0, 0, 0, 960, 7680, 0x00000000\n");
            assert_eq!(
                Metadata::new(text.as_bytes(), Stream::Audio, 12288, 0, 10_000).next(),
                Err(())
            );
        }
        let text = format!(
            "{}0, 0, 0, 1, 12288, 0x00000000\n",
            VIDEO.replace("1/25", "0/25")
        );
        assert_eq!(
            Metadata::new(text.as_bytes(), Stream::Video, 12288, 0, 10_000).next(),
            Err(())
        );
    }

    #[test]
    fn only_audio_can_end_after_valid_headers_and_kinds_cannot_be_swapped() {
        assert_eq!(
            Metadata::new(VIDEO.as_bytes(), Stream::Video, 12288, 0, 10_000).next(),
            Err(())
        );
        assert_eq!(
            Metadata::new(AUDIO.as_bytes(), Stream::Audio, 12288, 0, 10_000).next(),
            Ok(None)
        );
        let text = format!("{AUDIO}0, 0, 0, 960, 7680, 0x00000000\n");
        assert_eq!(
            Metadata::new(text.as_bytes(), Stream::Video, 12288, 0, 10_000).next(),
            Err(())
        );
        let text = format!("{VIDEO}0, 0, 0, 1, 12288, 0x00000000\n");
        assert_eq!(
            Metadata::new(text.as_bytes(), Stream::Audio, 12288, 0, 10_000).next(),
            Err(())
        );
    }
}
