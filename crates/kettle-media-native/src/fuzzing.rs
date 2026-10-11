//! The entry point for fuzzing the text the external decoder writes, which
//! the worker parses: ffprobe's description of a video and its packet list,
//! and ffmpeg's codec list. The fuzz target (feature `fuzzing`) and the
//! in-process sweep below call the same function.

/// Parse `data`, split at NUL bytes, as a description, a packet list and a
/// codec list; each must parse or fail without panicking, and what parses
/// must hold together (see the ffmpeg module's checks).
pub fn ffprobe(data: &[u8]) {
    crate::ffmpeg::check_text(data);
}

#[cfg(test)]
mod tests {
    use kettle_test_support::mutate::{cases, save_seeds};

    use super::*;

    /// A description, its packets and a codec list, mutated: nothing
    /// panics, and what parses holds together.
    #[test]
    fn mutated_decoder_text_never_breaks_the_parsers() {
        let description = "streams.stream.0.index=0\n\
            streams.stream.0.codec_name=\"h264\"\n\
            streams.stream.0.codec_type=\"video\"\n\
            streams.stream.0.width=1280\n\
            streams.stream.0.height=720\n\
            streams.stream.0.sample_aspect_ratio=\"4:3\"\n\
            streams.stream.0.avg_frame_rate=\"30000/1001\"\n\
            streams.stream.0.r_frame_rate=\"30/1\"\n\
            streams.stream.0.time_base=\"1/15360\"\n\
            streams.stream.0.start_time=\"0.066667\"\n\
            streams.stream.0.duration=\"9.976000\"\n\
            streams.stream.0.disposition.attached_pic=0\n\
            streams.stream.0.side_data_list.side_data.0.rotation=-90\n\
            streams.stream.1.index=1\n\
            streams.stream.1.codec_type=\"audio\"\n\
            format.start_time=\"0.000000\"\n\
            format.duration=\"10.000000\"\n";
        let packets = "0,K__\n1024,___\n512,___\n1536,___\n2048,K__\n2560,_D_\nN/A,___\n";
        let codecs = " DEV.LS h264                 H.264\n D.V.L. vp9                  VP9\n";
        let mut whole = Vec::new();
        for part in [description, packets, codecs] {
            whole.extend_from_slice(part.as_bytes());
            whole.push(0);
        }
        // The packet list without its untimed packet, which parses.
        let timed = "0,K__\n1024,___\n512,___\n1536,___\n2048,K__\n2560,_D_\n";
        let mut parsed = description.as_bytes().to_vec();
        parsed.push(0);
        parsed.extend_from_slice(timed.as_bytes());
        // At the edges: a rate of exactly a thousand frames a second, a
        // display matrix turned by an odd angle, as phones can write, and a
        // picture as wide as a video may be before its pixels widen it.
        let edges = description
            .replace("30000/1001", "1000/1")
            .replace("rotation=-90", "rotation=30")
            .replace(
                "width=1280",
                &format!("width={}", kettle_media::MAX_VIDEO_SIDE),
            );
        let mut edge = edges.into_bytes();
        edge.push(0);
        edge.extend_from_slice(timed.as_bytes());
        // Half a frame a second: a frame two seconds long.
        ffprobe(
            b"streams.stream.0.index=0\nstreams.stream.0.codec_type=video\n\
              streams.stream.0.width=1\nstreams.stream.0.height=1\n\
              streams.stream.0.avg_frame_rate=1/2\n",
        );
        save_seeds("ffprobe", &[&whole, &parsed, &edge]);
        for case in cases(&[&whole, &parsed, &edge], 9000, 0x5eed_0005) {
            ffprobe(&case);
        }
    }
}
