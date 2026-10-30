use super::{MAX_IMAGE_DIM, rgba_bytes};

#[derive(Clone, Copy, Default)]
struct Component {
    id: u8,
    h: usize,
    v: usize,
}

struct Frame {
    width: usize,
    height: usize,
    progressive: bool,
    components: [Component; 4],
    count: usize,
}

/// Scratch in addition to the separately reserved RGBA output. Audited against
/// image 0.25.10 codecs/jpeg/decoder.rs (new: copied input; read_image: native
/// pixels) and zune-jpeg 0.5.15: mcu_prog.rs decode_mcu_ycbcr_progressive
/// (full i16 coefficient planes), mcu.rs decode_mcu_ycbcr_baseline (the same
/// for separate scans), misc.rs setup_component_params (MCU-padded strides),
/// components.rs setup_upsample_scanline and worker.rs upsample (i16 row and
/// upsampling buffers, including resize). Native pixels can coexist with RGBA
/// during conversion. Eight input lengths cover read_to_end growth, metadata
/// copies and metadata Vec growth (headers.rs APP parsers). 64 KiB covers
/// bounded decoder tables and small scratch; row reallocations get double room.
/// Re-audit these bounds when either pinned decoder changes.
pub(super) fn scratch_bytes(bytes: &[u8], decode_cap: usize) -> Option<usize> {
    let (frame, first_scan_count) = headers(bytes)?;
    let rgba = rgba_bytes(frame.width as u32, frame.height as u32)?;
    if rgba > decode_cap {
        return None;
    }
    let components = &frame.components[..frame.count];
    let h_max = components.iter().map(|c| c.h).max()?;
    let v_max = components.iter().map(|c| c.v).max()?;
    let mcu_x = frame.width.div_ceil(h_max * 8);
    let mcu_y = frame.height.div_ceil(v_max * 8);
    let mut scratch = bytes.len().checked_mul(8)?.checked_add(64 * 1024)?;
    // Four-component JPEGs are converted to RGB by image; grayscale stays L8.
    let native_channels = if frame.count == 1 { 1 } else { 3 };
    scratch = scratch.checked_add(
        frame
            .width
            .checked_mul(frame.height)?
            .checked_mul(native_channels)?,
    )?;
    for c in components {
        if frame.progressive || first_scan_count != frame.count {
            let coefficients = mcu_x
                .checked_mul(mcu_y)?
                .checked_mul(c.h)?
                .checked_mul(c.v)?
                .checked_mul(64 * 2)?;
            scratch = scratch.checked_add(coefficients)?;
        }
        // setup_component_params updates h_max in component order. Ignoring
        // downsampling here bounds every intermediate stride, including a
        // subsampled grayscale decoder's reset to full width.
        let stride = frame.width.div_ceil(8).checked_mul(8)?.checked_mul(c.h)?;
        let ratio = (h_max / c.h).checked_mul(v_max / c.v)?;
        let raw = stride.checked_mul(c.v)?.checked_mul(8)?;
        let rows = stride.checked_mul(c.v)?.checked_mul(2 + ratio)?;
        let upsample = stride
            .checked_mul(ratio)?
            .checked_mul(16)?
            .max(raw.checked_mul(2)?);
        let row_bytes = raw
            .checked_add(rows)?
            .checked_add(upsample.checked_mul(2)?)?
            .checked_mul(2)?;
        scratch = scratch.checked_add(row_bytes)?;
    }
    // Baseline's upsampler scratch is at most eight times its largest stride.
    scratch.checked_add(frame.width.div_ceil(8).checked_mul(8 * h_max * 8 * 2)?)
}

// Fixed-size state, one forward pass, and no allocation. Scan entropy too so a
// later SOF cannot replace the geometry used to admit the decode. The caller
// caps input length at the transmission limit before entering this parser.
fn headers(bytes: &[u8]) -> Option<(Frame, usize)> {
    if !bytes.starts_with(&[0xff, 0xd8]) {
        return None;
    }
    let mut pos = 2usize;
    let mut entropy = false;
    let mut frame = None;
    let mut first_scan_count = None;
    while pos < bytes.len() {
        if entropy && bytes[pos] != 0xff {
            pos += 1;
            continue;
        }
        if bytes[pos] != 0xff {
            return None;
        }
        while bytes.get(pos) == Some(&0xff) {
            pos += 1;
        }
        let marker = *bytes.get(pos)?;
        pos += 1;
        if entropy && (marker == 0 || (0xd0..=0xd7).contains(&marker)) {
            continue;
        }
        entropy = false;
        if marker == 0xd9 {
            return Some((frame?, first_scan_count?));
        }
        if marker == 0x01 {
            continue;
        }
        if marker == 0 || (0xd0..=0xd8).contains(&marker) {
            return None;
        }
        let length = u16::from_be_bytes(bytes.get(pos..pos + 2)?.try_into().ok()?) as usize;
        let end = pos.checked_add(length)?;
        let data = bytes.get(pos + 2..end)?;
        pos = end;
        match marker {
            0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf => {
                if frame.is_some()
                    || !matches!(marker, 0xc0..=0xc2)
                    || data.len() < 6
                    || data[0] != 8
                {
                    return None;
                }
                let height = u16::from_be_bytes([data[1], data[2]]) as usize;
                let width = u16::from_be_bytes([data[3], data[4]]) as usize;
                let count = data[5] as usize;
                if width == 0
                    || height == 0
                    || width > MAX_IMAGE_DIM as usize
                    || height > MAX_IMAGE_DIM as usize
                    || !matches!(count, 1 | 3 | 4)
                    || data.len() != 6 + 3 * count
                {
                    return None;
                }
                let mut components = [Component::default(); 4];
                for (i, value) in data[6..].as_chunks::<3>().0.iter().enumerate() {
                    let h = (value[1] >> 4) as usize;
                    let v = (value[1] & 15) as usize;
                    if !h.is_power_of_two()
                        || v == 0
                        || components[..i].iter().any(|c| c.id == value[0])
                    {
                        return None;
                    }
                    components[i] = Component { id: value[0], h, v };
                }
                frame = Some(Frame {
                    width,
                    height,
                    progressive: marker == 0xc2,
                    components,
                    count,
                });
            }
            0xda => {
                let frame = frame.as_ref()?;
                let count = *data.first()? as usize;
                if count == 0 || count > frame.count || data.len() != 4 + count * 2 {
                    return None;
                }
                let mut seen = [false; 4];
                for value in data[1..1 + count * 2].as_chunks::<2>().0 {
                    let index = frame.components[..frame.count]
                        .iter()
                        .position(|c| c.id == value[0])?;
                    if seen[index] {
                        return None;
                    }
                    seen[index] = true;
                }
                first_scan_count.get_or_insert(count);
                entropy = true;
            }
            // DNL changes geometry after the admission decision.
            0xdc => return None,
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::scratch_bytes;

    fn headers(progressive: bool, separate_scan: bool) -> Vec<u8> {
        let mut bytes = vec![
            0xff,
            0xd8,
            0xff,
            if progressive { 0xc2 } else { 0xc0 },
            0,
            17,
            8,
            0,
            17,
            0,
            17,
            3,
            1,
            0x11,
            0,
            2,
            0x11,
            0,
            3,
            0x11,
            0,
        ];
        if separate_scan {
            bytes.extend_from_slice(&[0xff, 0xda, 0, 8, 1, 1, 0, 0, 63, 0]);
        } else {
            bytes.extend_from_slice(&[0xff, 0xda, 0, 12, 3, 1, 0, 2, 0, 3, 0, 0, 63, 0]);
        }
        bytes.extend_from_slice(&[0xff, 0xd9]);
        bytes
    }

    #[test]
    fn coefficient_planes_include_mcu_padding_and_separate_baseline_scans() {
        let baseline = scratch_bytes(&headers(false, false), 4096).unwrap();
        let progressive = scratch_bytes(&headers(true, false), 4096).unwrap();
        // Three 24x24 i16 planes for a 17x17 4:4:4 image.
        assert_eq!(progressive - baseline, 3456);
        let separate = headers(false, true);
        let interleaved = headers(false, false);
        assert_eq!(
            scratch_bytes(&separate, 4096).unwrap() - separate.len() * 8,
            progressive - interleaved.len() * 8
        );
    }

    #[test]
    fn malformed_jpeg_headers_fail_closed() {
        let valid = headers(true, false);
        for end in 0..valid.len() {
            assert!(
                scratch_bytes(&valid[..end], 4096).is_none(),
                "truncation at {end}"
            );
        }
        assert!(scratch_bytes(&valid, 1024).is_none(), "pixel cap");
        let mut invalid = valid.clone();
        invalid[13] = 0;
        assert!(scratch_bytes(&invalid, 4096).is_none(), "zero sampling");
        let mut duplicate = valid.clone();
        duplicate.splice(21..21, valid[2..21].iter().copied());
        assert!(scratch_bytes(&duplicate, 4096).is_none(), "duplicate SOF");
        let mut invalid = valid.clone();
        invalid[26] = 9;
        assert!(
            scratch_bytes(&invalid, 4096).is_none(),
            "unknown scan component"
        );
        let mut stuffed = valid.clone();
        stuffed.splice(valid.len() - 2..valid.len() - 2, [0xff, 0, 0xff, 0xd0]);
        assert!(
            scratch_bytes(&stuffed, 4096).is_some(),
            "entropy stuffing and restart markers"
        );
    }
}
