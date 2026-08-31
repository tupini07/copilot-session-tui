use crate::mux::MuxEvent;
use std::sync::mpsc::Sender;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PaneSignals {
    pub title: Option<String>,
    pub events: Vec<PaneSignalEvent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneSignalEvent {
    Bell,
    Progress(crate::host_terminal::ProgressState),
}

/// Replies the emulator owes the child process.
///
/// `vt100` is a screen model, not a full terminal: it never answers device queries.
/// That is not optional in practice — ConPTY emits `ESC[6n` (report cursor position)
/// while starting up and *blocks* until it gets a response, so without this the very
/// first child produces no output at all.
pub struct PaneCallbacks {
    pane_id: crate::mux::PaneId,
    replies: Sender<Vec<u8>>,
    events: Sender<MuxEvent>,
    title: Option<String>,
    signals: Vec<PaneSignalEvent>,
    terminal_light_mode: Option<bool>,
    theme_updates_requested: bool,
}

impl PaneCallbacks {
    pub fn new(
        pane_id: crate::mux::PaneId,
        replies: Sender<Vec<u8>>,
        events: Sender<MuxEvent>,
        terminal_light_mode: Option<bool>,
    ) -> Self {
        Self {
            pane_id,
            replies,
            events,
            title: None,
            signals: Vec::new(),
            terminal_light_mode,
            theme_updates_requested: false,
        }
    }

    /// Window title reported by the child via OSC 0/2, if any.
    pub fn take_title(&mut self) -> Option<String> {
        self.title.take()
    }

    pub fn take_signals(&mut self) -> PaneSignals {
        PaneSignals {
            title: self.take_title(),
            events: std::mem::take(&mut self.signals),
        }
    }

    fn reply(&self, bytes: Vec<u8>) {
        let _ = self.replies.send(bytes);
    }

    pub fn set_terminal_light_mode(&mut self, terminal_light_mode: Option<bool>) {
        if self.terminal_light_mode == terminal_light_mode {
            return;
        }
        self.terminal_light_mode = terminal_light_mode;
        if self.theme_updates_requested {
            if let Some(light_theme) = terminal_light_mode {
                self.reply(theme_report(light_theme));
            }
        }
    }
}

fn theme_report(light_theme: bool) -> Vec<u8> {
    format!("\x1b[?997;{}n", if light_theme { 2 } else { 1 }).into_bytes()
}

impl vt100::Callbacks for PaneCallbacks {
    fn copy_to_clipboard(&mut self, _: &mut vt100::Screen, selector: &[u8], data: &[u8]) {
        let mut sequence = Vec::with_capacity(selector.len() + data.len() + 10);
        sequence.extend_from_slice(b"\x1b]52;");
        sequence.extend_from_slice(selector);
        sequence.push(b';');
        sequence.extend_from_slice(data);
        sequence.extend_from_slice(b"\x1b\\");
        let _ = self
            .events
            .send(MuxEvent::HostSequence(self.pane_id, sequence));
    }

    fn audible_bell(&mut self, _: &mut vt100::Screen) {
        self.signals.push(PaneSignalEvent::Bell);
    }

    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        self.title = Some(String::from_utf8_lossy(title).to_string());
    }

    fn set_window_icon_name(&mut self, _: &mut vt100::Screen, name: &[u8]) {
        if self.title.is_none() {
            self.title = Some(String::from_utf8_lossy(name).to_string());
        }
    }

    fn unhandled_osc(&mut self, _: &mut vt100::Screen, params: &[&[u8]]) {
        if let Some(progress) = crate::host_terminal::progress_state(params) {
            self.signals.push(PaneSignalEvent::Progress(progress));
        }
        if let Some(sequence) = crate::host_terminal::progress_sequence(params) {
            let _ = self
                .events
                .send(MuxEvent::HostSequence(self.pane_id, sequence));
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
            // Report the appearance of CST's nested terminal rather than the host
            // operating-system theme. Copilot's `github` theme uses this response.
            ('n', Some(b'?')) if first == Some(996) => {
                if let Some(light_theme) = self.terminal_light_mode {
                    self.reply(theme_report(light_theme));
                }
            }
            // Applications may request an unsolicited report when the palette changes.
            ('h', Some(b'?')) if first == Some(2031) => {
                self.theme_updates_requested = true;
            }
            ('l', Some(b'?')) if first == Some(2031) => {
                self.theme_updates_requested = false;
            }
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

    fn parser_with_replies(
        light_theme: bool,
    ) -> (
        vt100::Parser<PaneCallbacks>,
        mpsc::Receiver<Vec<u8>>,
        mpsc::Receiver<MuxEvent>,
    ) {
        let (tx, rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        (
            vt100::Parser::new_with_callbacks(
                24,
                80,
                0,
                PaneCallbacks::new(7, tx, event_tx, Some(light_theme)),
            ),
            rx,
            event_rx,
        )
    }

    #[test]
    fn answers_cursor_position_requests() {
        let (mut parser, rx, _) = parser_with_replies(false);

        // Move to row 3, col 5 (1-based), then ask where the cursor is.
        parser.process(b"\x1b[3;5H\x1b[6n");

        let reply = rx.try_recv().unwrap();
        assert_eq!(reply, b"\x1b[3;5R");
    }

    #[test]
    fn answers_device_status_and_attribute_queries() {
        let (mut parser, rx, _) = parser_with_replies(false);

        parser.process(b"\x1b[5n");
        assert_eq!(rx.try_recv().unwrap(), b"\x1b[0n");

        parser.process(b"\x1b[c");
        assert_eq!(rx.try_recv().unwrap(), b"\x1b[?62;1;6c");

        parser.process(b"\x1b[>c");
        assert_eq!(rx.try_recv().unwrap(), b"\x1b[>0;10;1c");
    }

    #[test]
    fn reports_the_text_area_size() {
        let (mut parser, rx, _) = parser_with_replies(false);

        parser.process(b"\x1b[18t");

        assert_eq!(rx.try_recv().unwrap(), b"\x1b[8;24;80t");
    }

    #[test]
    fn captures_the_window_title() {
        let (mut parser, _rx, _) = parser_with_replies(false);

        parser.process(b"\x1b]2;my session\x07");

        assert_eq!(
            parser.callbacks_mut().take_title().as_deref(),
            Some("my session")
        );
    }

    #[test]
    fn unrelated_sequences_produce_no_reply() {
        let (mut parser, rx, _) = parser_with_replies(false);

        parser.process(b"hello\x1b[1;1H");

        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn forwards_clipboard_requests_to_the_host() {
        let (mut parser, _, events) = parser_with_replies(false);

        parser.process(b"\x1b]52;c;Q29waWVkIHRleHQ=\x07");

        let MuxEvent::HostSequence(id, sequence) = events.try_recv().unwrap() else {
            panic!("expected host sequence");
        };
        assert_eq!(id, 7);
        assert_eq!(sequence, b"\x1b]52;c;Q29waWVkIHRleHQ=\x1b\\");
    }

    #[test]
    fn forwards_progress_state_to_the_host() {
        let (mut parser, _, events) = parser_with_replies(false);

        parser.process(b"\x1b]9;4;3;0\x07");

        let MuxEvent::HostSequence(id, sequence) = events.try_recv().unwrap() else {
            panic!("expected host sequence");
        };
        assert_eq!(id, 7);
        assert_eq!(sequence, b"\x1b]9;4;3;0\x1b\\");
        assert_eq!(
            parser.callbacks_mut().take_signals().events,
            vec![PaneSignalEvent::Progress(
                crate::host_terminal::ProgressState::Indeterminate
            )]
        );
    }

    #[test]
    fn progress_signals_are_drained_per_output_chunk() {
        let (mut parser, _, _) = parser_with_replies(false);

        parser.process(b"\x1b]9;4;3;0\x07");
        let working = parser.callbacks_mut().take_signals();
        parser.process(b"\x1b]9;4;0;0\x07");
        let complete = parser.callbacks_mut().take_signals();

        assert_eq!(
            working.events,
            vec![PaneSignalEvent::Progress(
                crate::host_terminal::ProgressState::Indeterminate
            )]
        );
        assert_eq!(
            complete.events,
            vec![PaneSignalEvent::Progress(
                crate::host_terminal::ProgressState::Clear
            )]
        );
        assert!(parser.callbacks_mut().take_signals().events.is_empty());
    }

    #[test]
    fn does_not_forward_unrelated_osc_commands() {
        let (mut parser, _, events) = parser_with_replies(false);

        parser.process(b"\x1b]8;;https://example.com\x07");

        assert!(events.try_recv().is_err());
    }

    #[test]
    fn reports_the_nested_cst_theme_to_copilot() {
        let (mut dark, dark_replies, _) = parser_with_replies(false);
        dark.process(b"\x1b[?996n");
        assert_eq!(dark_replies.try_recv().unwrap(), b"\x1b[?997;1n");

        let (mut light, light_replies, _) = parser_with_replies(true);
        light.process(b"\x1b[?996n");
        assert_eq!(light_replies.try_recv().unwrap(), b"\x1b[?997;2n");
    }

    #[test]
    fn unspecified_classic_theme_leaves_detection_to_the_host_environment() {
        let (tx, replies) = mpsc::channel();
        let (events, _) = mpsc::channel();
        let mut parser =
            vt100::Parser::new_with_callbacks(24, 80, 0, PaneCallbacks::new(7, tx, events, None));

        parser.process(b"\x1b[?996n");

        assert!(replies.try_recv().is_err());
    }

    #[test]
    fn subscribed_children_receive_only_real_appearance_changes() {
        let (mut parser, replies, _) = parser_with_replies(false);
        parser.process(b"\x1b[?2031h");

        parser.callbacks_mut().set_terminal_light_mode(Some(true));
        assert_eq!(replies.try_recv().unwrap(), b"\x1b[?997;2n");
        parser.callbacks_mut().set_terminal_light_mode(Some(true));
        assert!(replies.try_recv().is_err());

        parser.process(b"\x1b[?2031l");
        parser.callbacks_mut().set_terminal_light_mode(Some(false));
        assert!(replies.try_recv().is_err());
    }
}
