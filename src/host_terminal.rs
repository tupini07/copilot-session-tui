pub const CLEAR_PROGRESS: &[u8] = b"\x1b]9;4;0;0\x1b\\";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressState {
    Clear,
    Normal,
    Error,
    Indeterminate,
    Warning,
}

impl ProgressState {
    pub fn is_working(self) -> bool {
        self != Self::Clear
    }
}

pub fn progress_state(params: &[&[u8]]) -> Option<ProgressState> {
    if !(3..=4).contains(&params.len()) || params[0] != b"9" || params[1] != b"4" {
        return None;
    }
    match parse_number(params[2])? {
        0 => Some(ProgressState::Clear),
        1 => Some(ProgressState::Normal),
        2 => Some(ProgressState::Error),
        3 => Some(ProgressState::Indeterminate),
        4 => Some(ProgressState::Warning),
        _ => None,
    }
}

/// Rebuild a validated Windows Terminal progress sequence for the outer terminal.
///
/// OSC 9;4 uses `state` values 0-4 and an optional percentage 0-100. Restricting
/// passthrough to that shape prevents arbitrary child OSC commands from escaping
/// the nested terminal.
pub fn progress_sequence(params: &[&[u8]]) -> Option<Vec<u8>> {
    progress_state(params)?;
    if params.len() == 4 && parse_number(params[3])? > 100 {
        return None;
    }

    let mut sequence = b"\x1b]".to_vec();
    for (index, param) in params.iter().enumerate() {
        if index > 0 {
            sequence.push(b';');
        }
        sequence.extend_from_slice(param);
    }
    sequence.extend_from_slice(b"\x1b\\");
    Some(sequence)
}

pub fn progress_sequence_for_state(state: ProgressState) -> Vec<u8> {
    let state = match state {
        ProgressState::Clear => 0,
        ProgressState::Normal => 1,
        ProgressState::Error => 2,
        ProgressState::Indeterminate => 3,
        ProgressState::Warning => 4,
    };
    format!("\x1b]9;4;{state};0\x1b\\").into_bytes()
}

pub fn progress_state_from_sequence(sequence: &[u8]) -> Option<ProgressState> {
    let body = sequence
        .strip_prefix(b"\x1b]")?
        .strip_suffix(b"\x1b\\")
        .or_else(|| sequence.strip_prefix(b"\x1b]")?.strip_suffix(b"\x07"))?;
    let params: Vec<&[u8]> = body.split(|byte| *byte == b';').collect();
    progress_state(&params)
}

fn parse_number(value: &[u8]) -> Option<u16> {
    if value.is_empty() || !value.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(value).ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_windows_terminal_progress_states() {
        assert_eq!(
            progress_sequence(&[b"9", b"4", b"3", b"0"]).unwrap(),
            b"\x1b]9;4;3;0\x1b\\"
        );
        assert_eq!(
            progress_sequence(&[b"9", b"4", b"0"]).unwrap(),
            b"\x1b]9;4;0\x1b\\"
        );
    }

    #[test]
    fn progress_states_distinguish_working_from_complete() {
        assert_eq!(
            progress_state(&[b"9", b"4", b"3", b"0"]),
            Some(ProgressState::Indeterminate)
        );
        assert!(progress_state(&[b"9", b"4", b"3"]).unwrap().is_working());
        assert_eq!(
            progress_state(&[b"9", b"4", b"0"]),
            Some(ProgressState::Clear)
        );
        assert!(!progress_state(&[b"9", b"4", b"0"]).unwrap().is_working());
    }

    #[test]
    fn rejects_unrelated_or_invalid_osc_sequences() {
        assert!(progress_sequence(&[b"2", b"title"]).is_none());
        assert!(progress_sequence(&[b"9", b"4", b"5", b"0"]).is_none());
        assert!(progress_sequence(&[b"9", b"4", b"1", b"101"]).is_none());
        assert!(progress_sequence(&[b"9", b"4", b"busy", b"0"]).is_none());
    }

    #[test]
    fn canonical_progress_sequences_round_trip() {
        for state in [
            ProgressState::Clear,
            ProgressState::Normal,
            ProgressState::Error,
            ProgressState::Indeterminate,
            ProgressState::Warning,
        ] {
            assert_eq!(
                progress_state_from_sequence(&progress_sequence_for_state(state)),
                Some(state)
            );
        }
        assert!(progress_state_from_sequence(b"\x1b]52;c;copy\x1b\\").is_none());
    }
}
