//! Whitespace-tolerant base64 decoding with fixed-size filtering scratch.

use base64::Engine;

use crate::graphics_limits::{GraphicsBudget, GraphicsReservation};

pub(crate) fn decode(
    input: &str,
    budget: &GraphicsBudget,
) -> Option<(Vec<u8>, GraphicsReservation)> {
    decode_bytes(input.bytes(), budget)
}

pub(crate) fn decode_bytes(
    input: impl Iterator<Item = u8> + Clone,
    budget: &GraphicsBudget,
) -> Option<(Vec<u8>, GraphicsReservation)> {
    let len = input.clone().filter(|b| !b.is_ascii_whitespace()).count();
    let cap = len.checked_add(3)?.checked_div(4)?.checked_mul(3)?.max(1);
    let reservation = budget.reserve_transient_cpu(cap)?;
    let mut output = Vec::new();
    output.try_reserve_exact(cap).ok()?;
    output.resize(cap, 0);
    let mut input = input.filter(|b| !b.is_ascii_whitespace()).peekable();
    let mut block = [0; 4096];
    let mut written = 0;
    loop {
        let mut used = 0;
        for (index, byte) in input.by_ref().take(block.len()).enumerate() {
            block[index] = byte;
            used = index + 1;
        }
        let last = input.peek().is_none();
        // Padding is legal only in the final quartet of the entire stream.
        // Decoding blocks independently must not accept data after padding.
        if !last && block[..used].contains(&b'=') {
            return None;
        }
        written += base64::engine::general_purpose::STANDARD
            .decode_slice(&block[..used], &mut output[written..])
            .ok()?;
        if last {
            break;
        }
    }
    output.truncate(written);
    Some((output, reservation))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GraphicsLimits;

    #[test]
    fn segmented_base64_preserves_whitespace_and_padding_validation() {
        let budget = GraphicsBudget::isolated(GraphicsLimits::default()).unwrap();
        for (input, expected) in [
            ("A Q\nIDBA==", Some(vec![1, 2, 3, 4])),
            ("AQID", Some(vec![1, 2, 3])),
            ("AQ==IDBA", None),
            ("AQIDBA=", None),
            ("AQIDBA==!", None),
            ("AQIDBA==\n", Some(vec![1, 2, 3, 4])),
        ] {
            for size in 1..=9 {
                let segments: Vec<_> = input
                    .as_bytes()
                    .chunks(size)
                    .map(|chunk| chunk.to_vec().into_boxed_slice())
                    .collect();
                let actual = decode_bytes(
                    segments.iter().flat_map(|chunk| chunk.iter().copied()),
                    &budget,
                )
                .map(|(bytes, _)| bytes);
                assert_eq!(actual, expected, "segment size {size}, {input:?}");
                assert_eq!(budget.usage(), (0, 0, 0, 0));
            }
        }
        // Padding before a later filtering block is invalid even across segments.
        let input = format!("{}AQ==AAAA", "AAAA".repeat(1023));
        assert!(
            decode_bytes(
                input
                    .as_bytes()
                    .chunks(3)
                    .flat_map(|chunk| chunk.iter().copied()),
                &budget
            )
            .is_none()
        );
        assert_eq!(budget.usage(), (0, 0, 0, 0));
    }
}
