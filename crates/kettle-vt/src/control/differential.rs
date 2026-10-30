// Included only by test modules in all three consumers so their existing vectors
// exercise the shared stripper and both frozen implementations.
mod legacy_exec {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../kettle-vt/src/control/legacy_exec.rs"
    ));
}

#[allow(dead_code)]
mod legacy_log {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../kettle-vt/src/control/legacy_log.rs"
    ));

    impl AnsiStripper {
        // The only adaptation to this oracle is the intentional C1 policy:
        // introduce C1 sequences in unshielded Plain, as the exec oracle does.
        pub(super) fn strip_with_c1(&mut self, input: &[u8]) -> Vec<u8> {
            if !input
                .iter()
                .any(|byte| matches!(byte, 0x90 | 0x98 | 0x9b..=0x9f))
            {
                return self.strip(input);
            }
            let mut out = Vec::new();
            for &byte in input {
                if self.state == StripState::Plain && self.utf8_continuation == 0 {
                    self.state = match byte {
                        0x9b => StripState::Csi,
                        0x9d => StripState::Osc,
                        0x90 | 0x98 | 0x9e | 0x9f => StripState::String,
                        0x9c => continue,
                        _ => {
                            out.extend(self.strip(&[byte]));
                            continue;
                        }
                    };
                    self.control_bytes = 1;
                } else {
                    out.extend(self.strip(&[byte]));
                }
            }
            out
        }
    }
}

#[derive(Debug)]
pub struct CheckedStripper {
    shared: SharedStripper,
    exec: legacy_exec::AnsiStripper,
    log: legacy_log::AnsiStripper,
    log_with_c1: legacy_log::AnsiStripper,
    actual: Vec<u8>,
    exec_output: Vec<u8>,
    log_output: Vec<u8>,
    require_original: bool,
}

impl Default for CheckedStripper {
    fn default() -> Self {
        Self {
            shared: SharedStripper::default(),
            exec: legacy_exec::AnsiStripper::default(),
            log: legacy_log::AnsiStripper::new(),
            log_with_c1: legacy_log::AnsiStripper::new(),
            actual: Vec::new(),
            exec_output: Vec::new(),
            log_output: Vec::new(),
            require_original: true,
        }
    }
}

impl CheckedStripper {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, input: &[u8], output: &mut Vec<u8>) {
        let start = output.len();
        self.shared.push(input, output);
        let expected = self.log_with_c1.strip_with_c1(input);
        assert_eq!(
            &output[start..],
            expected,
            "C1-adapted log oracle disagrees"
        );
        self.actual.extend_from_slice(&output[start..]);
        self.exec.push(input, &mut self.exec_output);
        self.log_output.extend(self.log.strip(input));
        if self.require_original {
            self.assert_matches_original();
        }
    }

    pub fn strip(&mut self, input: &[u8]) -> Vec<u8> {
        let mut output = Vec::new();
        self.push(input, &mut output);
        output
    }

    pub fn assert_matches_original(&self) {
        assert!(
            self.actual == self.exec_output || self.actual == self.log_output,
            "output differs from both original oracles ({} shared / {} exec / {} log bytes)",
            self.actual.len(),
            self.exec_output.len(),
            self.log_output.len()
        );
    }

    pub fn original_outputs(&self) -> (&[u8], &[u8]) {
        (&self.exec_output, &self.log_output)
    }

    pub fn shared_output(&self) -> &[u8] {
        &self.actual
    }

    pub fn combining_intended_differences() -> Self {
        Self {
            require_original: false,
            ..Self::default()
        }
    }
}
