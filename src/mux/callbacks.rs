use std::sync::mpsc::Sender;

/// Replies the emulator owes the child process.
///
/// `vt100` is a screen model, not a full terminal: it never answers device queries.
/// That is not optional in practice — ConPTY emits `ESC[6n` (report cursor position)
/// while starting up and *blocks* until it gets a response, so without this the very
/// first child produces no output at all.
pub struct PaneCallbacks {
    replies: Sender<Vec<u8>>,
    title: Option<String>,
    bell: bool,
}

impl PaneCallbacks {
    pub fn new(replies: Sender<Vec<u8>>) -> Self {
        Self {
            replies,
            title: None,
            bell: false,
        }
    }

    /// Window title reported by the child via OSC 0/2, if any.
    pub fn take_title(&mut self) -> Option<String> {
        self.title.take()
    }

    pub fn take_bell(&mut self) -> bool {
        std::mem::take(&mut self.bell)
    }

    fn reply(&self, bytes: Vec<u8>) {
        let _ = self.replies.send(bytes);
    }
}

impl vt100::Callbacks for PaneCallbacks {
    fn audible_bell(&mut self, _: &mut vt100::Screen) {
        self.bell = true;
    }

    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        self.title = Some(String::from_utf8_lossy(title).to_string());
    }

    fn set_window_icon_name(&mut self, _: &mut vt100::Screen, name: &[u8]) {
        if self.title.is_none() {
            self.title = Some(String::from_utf8_lossy(name).to_string());
        }
    }

    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        intermediate: Option<u8>,
        _i2: Option<u8>,
        params: &[&[u16]],
        final_byte: char,
    ) {
        let first = params.first().and_then(|group| group.first()).copied();
        match (final_byte, intermediate) {
            // DSR — device status report.
            ('n', None) => match first {
                // Terminal is OK.
                Some(5) => self.reply(b"\x1b[0n".to_vec()),
                // Cursor position report, 1-based.
                Some(6) => {
                    let (row, col) = screen.cursor_position();
                    self.reply(format!("\x1b[{};{}R", row + 1, col + 1).into_bytes());
                }
                _ => {}
            },
            // DA1 — primary device attributes: claim a VT220 with 132-column and
            // selective-erase support, which is what xterm-compatible apps expect.
            ('c', None) => self.reply(b"\x1b[?62;1;6c".to_vec()),
            // DA2 — secondary device attributes: report as xterm.
            ('c', Some(b'>')) => self.reply(b"\x1b[>0;10;1c".to_vec()),
            // XTWINOPS: report text area size in characters.
            ('t', None) if first == Some(18) => {
                let (rows, cols) = screen.size();
                self.reply(format!("\x1b[8;{};{}t", rows, cols).into_bytes());
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn parser_with_replies() -> (vt100::Parser<PaneCallbacks>, mpsc::Receiver<Vec<u8>>) {
        let (tx, rx) = mpsc::channel();
        (
            vt100::Parser::new_with_callbacks(24, 80, 0, PaneCallbacks::new(tx)),
            rx,
        )
    }

    #[test]
    fn answers_cursor_position_requests() {
        let (mut parser, rx) = parser_with_replies();

        // Move to row 3, col 5 (1-based), then ask where the cursor is.
        parser.process(b"\x1b[3;5H\x1b[6n");

        let reply = rx.try_recv().unwrap();
        assert_eq!(reply, b"\x1b[3;5R");
    }

    #[test]
    fn answers_device_status_and_attribute_queries() {
        let (mut parser, rx) = parser_with_replies();

        parser.process(b"\x1b[5n");
        assert_eq!(rx.try_recv().unwrap(), b"\x1b[0n");

        parser.process(b"\x1b[c");
        assert_eq!(rx.try_recv().unwrap(), b"\x1b[?62;1;6c");

        parser.process(b"\x1b[>c");
        assert_eq!(rx.try_recv().unwrap(), b"\x1b[>0;10;1c");
    }

    #[test]
    fn reports_the_text_area_size() {
        let (mut parser, rx) = parser_with_replies();

        parser.process(b"\x1b[18t");

        assert_eq!(rx.try_recv().unwrap(), b"\x1b[8;24;80t");
    }

    #[test]
    fn captures_the_window_title() {
        let (mut parser, _rx) = parser_with_replies();

        parser.process(b"\x1b]2;my session\x07");

        assert_eq!(
            parser.callbacks_mut().take_title().as_deref(),
            Some("my session")
        );
    }

    #[test]
    fn unrelated_sequences_produce_no_reply() {
        let (mut parser, rx) = parser_with_replies();

        parser.process(b"hello\x1b[1;1H");

        assert!(rx.try_recv().is_err());
    }
}
