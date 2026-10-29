//! Whitespace-tolerant base64 decoding with fixed-size filtering scratch.

use base64::Engine;

use crate::graphics_limits::{GraphicsBudget, GraphicsReservation};

pub(crate) fn decode(
    input: &str,
    budget: &GraphicsBudget,
) -> Option<(Vec<u8>, GraphicsReservation)> {
    let len = input.bytes().filter(|b| !b.is_ascii_whitespace()).count();
    let cap = len.checked_add(3)?.checked_div(4)?.checked_mul(3)?.max(1);
    let reservation = budget.reserve_transient_cpu(cap)?;
    let mut output = Vec::new();
    output.try_reserve_exact(cap).ok()?;
    output.resize(cap, 0);
    let mut input = input
        .bytes()
        .filter(|b| !b.is_ascii_whitespace())
        .peekable();
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
