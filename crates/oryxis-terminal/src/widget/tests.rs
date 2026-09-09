    use super::*;

    fn view_and_state() -> (TerminalView<()>, TerminalWidgetState) {
        let (view, ws, _) = view_state_and_term();
        (view, ws)
    }

    /// `view_and_state` plus the shared state: the selection lives in
    /// the emulator now, so a test that plants or reads a band needs it.
    fn view_state_and_term(
    ) -> (TerminalView<()>, TerminalWidgetState, Arc<Mutex<TerminalState>>) {
        let term = Arc::new(Mutex::new(TerminalState::new_no_pty(80, 24).unwrap()));
        let view = TerminalView::new(Arc::clone(&term));
        (view, TerminalWidgetState::default(), term)
    }

    fn band(term: &Arc<Mutex<TerminalState>>) -> Option<Selection> {
        term.lock().unwrap().live_selection()
    }

    fn bounds() -> Rectangle {
        Rectangle::new(Point::ORIGIN, iced::Size::new(800.0, 480.0))
    }

    /// SGR click tracking (mc, htop). Regression for the "must hold
    /// Shift to click the sidebar" report: a release whose press was
    /// never reported (it landed on a sibling widget, so the cursor is
    /// outside the canvas and no press is tracked) must NOT be consumed
    /// by the report path; capturing it starves sibling `button`s,
    /// which fire on release.
    #[test]
    fn untracked_release_is_not_reported() {
        use alacritty_terminal::term::TermMode;
        let (view, mut ws) = view_and_state();
        let mode = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        let ev = iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left));
        // Cursor over the sidebar (outside the canvas), no tracked press.
        let cursor = mouse::Cursor::Available(Point::new(2000.0, 100.0));
        assert!(ws.report_button.is_none());
        let action = view.handle_mouse_report(&mut ws, &ev, bounds(), cursor, mode, 80, 24);
        assert!(action.is_none(), "sidebar release must stay local");
    }

    /// The other half of issue #150: with the remote app holding mouse
    /// tracking (tmux `mouse on`, htop), a high-resolution wheel's
    /// fragments must accumulate into whole detents here too. Reporting
    /// each fragment as a notch — what `ceil()` alone did — scrolled the
    /// remote app eight times per click of the wheel. A residual-only
    /// fragment is still CONSUMED (publishing nothing): while the app
    /// holds tracking the wheel belongs to the report path, and falling
    /// through would hand the fragment to the local-scrollback arm,
    /// which shares the residual and would double-count it.
    #[test]
    fn fractional_line_wheel_reports_one_notch_per_detent() {
        use alacritty_terminal::term::TermMode;
        let (view, mut ws) = view_and_state();
        let view = view.on_terminal_input(|_| ());
        let mode = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        let cursor = mouse::Cursor::Available(Point::new(40.0, 40.0));
        let frag = iced::Event::Mouse(mouse::Event::WheelScrolled {
            delta: mouse::ScrollDelta::Lines { x: 0.0, y: 0.125 },
        });

        for _ in 0..7 {
            let action = view
                .handle_mouse_report(&mut ws, &frag, bounds(), cursor, mode, 80, 24)
                .expect("a partial detent is still consumed by the report path");
            let (msg, _, _) = action.into_inner();
            assert!(msg.is_none(), "a partial detent reports nothing");
        }
        let action = view
            .handle_mouse_report(&mut ws, &frag, bounds(), cursor, mode, 80, 24)
            .expect("the completed detent is consumed");
        let (msg, _, _) = action.into_inner();
        assert!(msg.is_some(), "the completed detent reports once");
    }

    /// The touchpad twin: `ScrollDelta::Pixels` fragments arrive a few
    /// pixels at a time, below one cell, and must accumulate on the
    /// cell scale before reporting. Ceiling each fragment to a notch
    /// flooded a tracking TUI with several times the gesture (a slow
    /// two-finger scroll became ~30 wheel reports where ~6 lines were
    /// scrolled), while the same gesture over local scrollback, which
    /// already accumulated, scrolled correctly.
    #[test]
    fn fractional_pixel_wheel_reports_whole_cells_only() {
        use alacritty_terminal::term::TermMode;
        let (view, mut ws) = view_and_state();
        let view = view.on_terminal_input(|_| ());
        let mode = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        let cursor = mouse::Cursor::Available(Point::new(40.0, 40.0));
        // Four fragments of a quarter-cell each: only the fourth
        // completes a cell and may report.
        let frag = iced::Event::Mouse(mouse::Event::WheelScrolled {
            delta: mouse::ScrollDelta::Pixels { x: 0.0, y: view.cell_height / 4.0 },
        });

        for _ in 0..3 {
            let action = view
                .handle_mouse_report(&mut ws, &frag, bounds(), cursor, mode, 80, 24)
                .expect("a sub-cell fragment is still consumed by the report path");
            let (msg, _, _) = action.into_inner();
            assert!(msg.is_none(), "a sub-cell fragment reports nothing");
        }
        let action = view
            .handle_mouse_report(&mut ws, &frag, bounds(), cursor, mode, 80, 24)
            .expect("the completed cell is consumed");
        let (msg, _, _) = action.into_inner();
        assert!(msg.is_some(), "the completed cell reports once");
    }

    /// The canvas-originated press → drag off-canvas → release flow must
    /// still report the release (apps need the button-up to end a drag),
    /// falling back to the last reported cell.
    #[test]
    fn tracked_release_still_reports_after_leaving_canvas() {
        use alacritty_terminal::term::TermMode;
        let (view, mut ws) = view_and_state();
        let mode = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;

        let press = iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        let inside = mouse::Cursor::Available(Point::new(40.0, 40.0));
        let action = view.handle_mouse_report(&mut ws, &press, bounds(), inside, mode, 80, 24);
        assert!(action.is_some(), "on-canvas press must be reported");
        assert_eq!(ws.report_button, Some(ReportButton::Left));

        let release = iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left));
        let outside = mouse::Cursor::Available(Point::new(2000.0, 100.0));
        let action = view.handle_mouse_report(&mut ws, &release, bounds(), outside, mode, 80, 24);
        assert!(action.is_some(), "release of a reported press must land");
        assert!(ws.report_button.is_none(), "press tracking cleared on release");
    }

    /// Pressing Shift AFTER a reported press must not swallow the
    /// release: `release_completes_tracked_press` lets it through the
    /// Shift bypass, so the app gets its button-up and `report_button`
    /// clears instead of sticking at `Some(Left)` (phantom held button,
    /// every later motion misread as a drag).
    #[test]
    fn shift_at_release_does_not_swallow_tracked_release() {
        use alacritty_terminal::term::TermMode;
        let (view, mut ws) = view_and_state();
        let mode = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;

        let press = iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        let inside = mouse::Cursor::Available(Point::new(40.0, 40.0));
        let action = view.handle_mouse_report(&mut ws, &press, bounds(), inside, mode, 80, 24);
        assert!(action.is_some(), "press without Shift must be reported");
        assert_eq!(ws.report_button, Some(ReportButton::Left));

        // Shift lands between press and release.
        ws.modifiers = iced::keyboard::Modifiers::SHIFT;
        let release = iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left));
        assert!(
            TerminalView::<()>::release_completes_tracked_press(&ws, &release),
            "tracked release must pierce the Shift bypass"
        );
        let action = view.handle_mouse_report(&mut ws, &release, bounds(), inside, mode, 80, 24);
        assert!(action.is_some(), "release of a tracked press reports despite Shift");
        assert!(ws.report_button.is_none(), "press tracking cleared on release");
    }

    /// The Shift bypass must keep blocking NEW gestures: with no
    /// tracked press, neither a Shift+press nor its release qualifies
    /// as completing a tracked press, so local selection stays in
    /// charge for the whole gesture.
    #[test]
    fn shift_bypass_still_blocks_new_gestures() {
        let (_view, ws) = view_and_state();
        assert!(ws.report_button.is_none());
        let press = iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        let release = iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left));
        assert!(
            !TerminalView::<()>::release_completes_tracked_press(&ws, &press),
            "a press never qualifies"
        );
        assert!(
            !TerminalView::<()>::release_completes_tracked_press(&ws, &release),
            "a release with no tracked press never qualifies"
        );
    }

    /// `right_click_copy` is a Paste-scheme sub-option: a stale `true`
    /// under Menu / Extend (Settings hides the toggle there, so the
    /// user can't see or clear it) must not defer, i.e. suppress, the
    /// copy-on-select auto-copy.
    #[test]
    fn right_click_copy_only_defers_auto_copy_under_paste_scheme() {
        let (view, _) = view_and_state();
        let paste = view.with_right_click_copy(true).with_right_click_action(RightClickAction::Paste);
        assert!(paste.defers_copy_to_right_click(), "Paste scheme honours the deferral");

        let (view, _) = view_and_state();
        let menu = view.with_right_click_copy(true).with_right_click_action(RightClickAction::Menu);
        assert!(!menu.defers_copy_to_right_click(), "stale flag under Menu must not defer");

        let (view, _) = view_and_state();
        let extend = view.with_right_click_copy(true).with_right_click_action(RightClickAction::Extend);
        assert!(!extend.defers_copy_to_right_click(), "stale flag under Extend must not defer");

        let (view, _) = view_and_state();
        let off = view.with_right_click_action(RightClickAction::Paste);
        assert!(!off.defers_copy_to_right_click(), "flag off never defers");
    }

    /// Build a view over a terminal with `lines` rows of scrollback, so
    /// there is somewhere to scroll to.
    fn scrolled_view(lines: usize) -> (TerminalView<()>, TerminalWidgetState) {
        let mut term = TerminalState::new_no_pty(80, 24).unwrap();
        for _ in 0..lines {
            term.process(b"line\r\n");
        }
        (
            TerminalView::new(Arc::new(Mutex::new(term))),
            TerminalWidgetState::default(),
        )
    }

    /// A scrolled-back terminal driven only by `ScrollDelta::Pixels`
    /// deltas smaller than one cell (Windows precision touchpads and
    /// high-res wheels deliver a few pixels per notch): the pre-#91
    /// handler floored each `y / cell_height` to zero, so scrollback
    /// never moved and the transcript viewer (no output to snap it back)
    /// was frozen. The residual accumulator now carries the sub-cell
    /// remainder across events and emits a whole line once the pixels
    /// cross a cell.
    #[test]
    fn subcell_pixel_wheel_accumulates_into_scroll() {
        let (view, mut ws) = scrolled_view(200);
        // Cursor over the canvas; start at the live edge (offset 0).
        let cursor = mouse::Cursor::Available(Point::new(40.0, 40.0));
        assert_eq!(ws.scroll_offset.get(), 0);

        // cell_height defaults to 14.0 * 1.15 = 16.1, so a 10px notch is
        // sub-cell: one alone must not move (correct), but the second
        // crosses a cell boundary and advances exactly one line, where
        // the old truncation stayed pinned at zero forever.
        let notch = iced::Event::Mouse(mouse::Event::WheelScrolled {
            delta: mouse::ScrollDelta::Pixels { x: 0.0, y: 10.0 },
        });
        let action = view.on_event(&mut ws, &notch, bounds(), cursor);
        assert!(action.is_some(), "the canvas consumes the wheel event");
        assert_eq!(ws.scroll_offset.get(), 0, "one sub-cell notch must not move");

        view.on_event(&mut ws, &notch, bounds(), cursor);
        assert_eq!(ws.scroll_offset.get(), 1, "two sub-cell notches cross a cell");

        // Five more keep it climbing, proving the residual never stalls.
        for _ in 0..5 {
            view.on_event(&mut ws, &notch, bounds(), cursor);
        }
        assert!(
            ws.scroll_offset.get() >= 4,
            "sub-cell pixel wheel keeps advancing, got {}",
            ws.scroll_offset.get()
        );
    }

    /// A `ScrollDelta::Lines` notch still moves whole lines and clears
    /// any carried pixel residual, so switching devices (touchpad →
    /// discrete wheel) can't leave a stale sub-cell fraction fighting the
    /// next notch.
    #[test]
    fn line_wheel_moves_and_clears_pixel_residual() {
        let (view, mut ws) = scrolled_view(200);
        let cursor = mouse::Cursor::Available(Point::new(40.0, 40.0));

        // Leave a sub-cell residual behind from a pixel notch.
        let px = iced::Event::Mouse(mouse::Event::WheelScrolled {
            delta: mouse::ScrollDelta::Pixels { x: 0.0, y: 10.0 },
        });
        view.on_event(&mut ws, &px, bounds(), cursor);
        assert_ne!(ws.scroll_px_residual.get(), 0.0, "pixel notch left a residual");

        // A line notch scrolls 3 lines (y * 3) and wipes the residual.
        let ln = iced::Event::Mouse(mouse::Event::WheelScrolled {
            delta: mouse::ScrollDelta::Lines { x: 0.0, y: 1.0 },
        });
        view.on_event(&mut ws, &ln, bounds(), cursor);
        assert_eq!(ws.scroll_offset.get(), 3, "one line notch scrolls 3 lines");
        assert_eq!(ws.scroll_px_residual.get(), 0.0, "line notch clears the residual");
    }

    /// A high-resolution wheel reports FRACTIONS of a detent, and the
    /// platform hands them through as `ScrollDelta::Lines` on the same
    /// 120-per-detent scale it already divided out: Wayland's
    /// `axis_value120` (which winit only started honouring once the
    /// toolkit began binding `wl_seat` v9, in the 0.31 bump that shipped
    /// with 0.13.0) and Windows' `WM_MOUSEWHEEL`. `y as i32` truncated
    /// every fragment to zero and then swallowed the event, so the wheel
    /// did nothing at all on those devices (issue #150). The notch
    /// residual accumulates them into whole detents instead.
    #[test]
    fn fractional_line_wheel_accumulates_into_scroll() {
        let (view, mut ws) = scrolled_view(200);
        let cursor = mouse::Cursor::Available(Point::new(40.0, 40.0));
        assert_eq!(ws.scroll_offset.get(), 0);

        // An eighth of a detent, the `value120 = 15` fragment.
        let frag = iced::Event::Mouse(mouse::Event::WheelScrolled {
            delta: mouse::ScrollDelta::Lines { x: 0.0, y: 0.125 },
        });
        for _ in 0..7 {
            let action = view.on_event(&mut ws, &frag, bounds(), cursor);
            assert!(action.is_some(), "the canvas consumes every fragment");
        }
        assert_eq!(ws.scroll_offset.get(), 0, "a partial detent must not move");

        // The eighth fragment completes the detent: 3 lines, once.
        view.on_event(&mut ws, &frag, bounds(), cursor);
        assert_eq!(ws.scroll_offset.get(), 3, "a whole detent scrolls 3 lines");

        // And it keeps going, which is what the truncation never did.
        for _ in 0..8 {
            view.on_event(&mut ws, &frag, bounds(), cursor);
        }
        assert_eq!(ws.scroll_offset.get(), 6, "the residual never stalls");
    }

    /// A direction reversal mid-detent responds on its first fragment
    /// instead of spending it unwinding the accumulated one; a
    /// horizontal-only event (a tilt wheel, `y == 0.0`) is NOT a
    /// reversal and must leave the vertical residual alone.
    #[test]
    fn fractional_line_wheel_reversal_and_tilt() {
        let (view, mut ws) = scrolled_view(200);
        let cursor = mouse::Cursor::Available(Point::new(40.0, 40.0));
        let wheel = |y: f32, x: f32| {
            iced::Event::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Lines { x, y },
            })
        };

        // Half a detent up, then a tilt: the residual survives it.
        view.on_event(&mut ws, &wheel(0.5, 0.0), bounds(), cursor);
        view.on_event(&mut ws, &wheel(0.0, 1.0), bounds(), cursor);
        assert_eq!(ws.scroll_line_residual.get(), 0.5, "a tilt is not a reversal");

        // Reversing drops the stale residual, so the next half-detent
        // down is a fresh 0.5 rather than cancelling back to zero.
        view.on_event(&mut ws, &wheel(-0.5, 0.0), bounds(), cursor);
        assert_eq!(ws.scroll_line_residual.get(), -0.5, "a reversal starts over");
    }

    // ── The selection is the emulator's, and it moves with its text ──
    //
    // Every case below used to need a signal of its own from outside the
    // grid, and two of them had no signal at all: at the scrollback cap
    // `history_size` and `total_lines` stop moving while rows keep
    // rotating, and at the live edge the viewport offset never moves.
    // `Term` rotates the range inside `scroll_up_relative`, before the
    // grid rotates, so none of them is a case there.

    /// The state, a band over `lines` of it, and the text that band marks.
    fn banded(
        scrollback: usize,
        rows: u16,
        fill: usize,
        sel: Selection,
    ) -> (Arc<Mutex<TerminalState>>, String) {
        let term = Arc::new(Mutex::new(
            TerminalState::new_no_pty_with_scrollback(24, rows, scrollback).unwrap(),
        ));
        for i in 0..fill {
            term.lock().unwrap().process(format!("l{i}\r\n").as_bytes());
        }
        let mut s = term.lock().unwrap();
        s.set_selection(sel);
        let marked = s.get_selection_text(&sel);
        drop(s);
        (term, marked)
    }

    fn banded_text(term: &Arc<Mutex<TerminalState>>) -> String {
        let s = term.lock().unwrap();
        let sel = s.live_selection().expect("the band survives");
        s.get_selection_text(&sel)
    }

    /// Output under a held (scrolled-up) viewport rotates rows into
    /// history while the same rows stay on screen. The band names content,
    /// not screen rows, so it has to move with the text.
    #[test]
    fn a_band_follows_content_under_a_held_viewport() {
        let (term, marked) = (
            {
                let t = Arc::new(Mutex::new(
                    TerminalState::new_no_pty_with_scrollback(24, 3, 100).unwrap(),
                ));
                for i in 0..7 {
                    t.lock().unwrap().process(format!("l{i}\r\n").as_bytes());
                }
                t.lock().unwrap().scroll_viewport_by(3);
                t
            },
            "l2\nl3\nl4".to_string(),
        );
        term.lock()
            .unwrap()
            .set_selection(Selection { start: (0, -3), end: (1, -1), block: false });
        assert_eq!(banded_text(&term), marked, "the band starts on its text");

        term.lock().unwrap().process(b"l7\r\nl8\r\n");
        assert_eq!(term.lock().unwrap().viewport_offset(), 5, "the rows are held");
        assert_eq!(banded_text(&term), marked, "and the band stayed on them");
    }

    /// The live edge has no offset drift to follow: the viewport sits at 0
    /// and output pushes rows past the bottom, so a band anchored to screen
    /// rows would sit still while its text scrolled away under it.
    #[test]
    fn a_band_follows_content_at_the_live_edge() {
        let (term, marked) = banded(
            100,
            3,
            6,
            Selection { start: (0, 0), end: (1, 1), block: false },
        );
        assert_eq!(term.lock().unwrap().viewport_offset(), 0, "at the live edge");

        term.lock().unwrap().process(b"l6\r\nl7\r\n");
        let sel = term.lock().unwrap().live_selection().unwrap();
        assert_eq!((sel.start.1, sel.end.1), (-2, -1), "the raw lines rode down");
        assert_eq!(banded_text(&term), marked, "onto the same text");
    }

    /// Once the scrollback is FULL the oldest line is dropped for each new
    /// one, so `history_size` and `total_lines` freeze while the content
    /// keeps rotating. Nothing outside the grid can see that, which is the
    /// case this whole design exists for.
    #[test]
    fn a_band_follows_content_once_the_scrollback_is_full() {
        let (term, marked) = banded(
            5,
            3,
            10,
            Selection { start: (0, 0), end: (1, 1), block: false },
        );
        let before = {
            use alacritty_terminal::grid::Dimensions;
            let s = term.lock().unwrap();
            let g = s.backend.term.grid();
            assert_eq!(g.history_size(), 5, "the cap is reached");
            g.total_lines()
        };

        term.lock().unwrap().process(b"l10\r\nl11\r\n");
        {
            use alacritty_terminal::grid::Dimensions;
            let s = term.lock().unwrap();
            assert_eq!(
                s.backend.term.grid().total_lines(),
                before,
                "the cap hides the rotation from every outside signal"
            );
            assert_eq!(s.viewport_offset(), 0, "and so does the live edge");
        }
        assert_eq!(banded_text(&term), marked, "the band followed anyway");
    }

    /// The other half of the rule: moving the VIEW is not moving the
    /// content, so a scroll gesture must leave the band exactly where it
    /// is. Getting this wrong drags the highlight along with the wheel.
    #[test]
    fn a_manual_scroll_does_not_move_the_band() {
        let (term, marked) = banded(
            1000,
            3,
            60,
            Selection { start: (0, 0), end: (1, 1), block: false },
        );
        let before = term.lock().unwrap().live_selection().unwrap();

        term.lock().unwrap().scroll_viewport_by(10);
        term.lock().unwrap().scroll_viewport_by(-4);
        let after = term.lock().unwrap().live_selection().unwrap();

        assert_eq!(before, after, "a view move leaves the raw lines alone");
        assert_eq!(banded_text(&term), marked, "and the same text under them");
    }

    /// A drag that runs while output does: the press plants the band, the
    /// rows rotate under it, and the motion has to extend from where the
    /// anchor's text is NOW. The anchor never leaves the emulator, so it
    /// rotates with the range rather than being re-derived from a stale
    /// viewport offset.
    #[test]
    fn a_drag_extends_from_its_own_text_while_output_runs() {
        let term = Arc::new(Mutex::new(
            TerminalState::new_no_pty_with_scrollback(24, 6, 100).unwrap(),
        ));
        for i in 0..20 {
            term.lock().unwrap().process(format!("l{i}\r\n").as_bytes());
        }
        term.lock().unwrap().scroll_viewport_by(5);
        let view: TerminalView<()> = TerminalView::new(Arc::clone(&term)).focused(true);
        let mut ws = TerminalWidgetState::default();

        let at = Point::new(2.0, 40.0);
        view.on_event(
            &mut ws,
            &iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            bounds(),
            mouse::Cursor::Available(at),
        );
        let born = term.lock().unwrap().live_selection().expect("a band is born");

        // Two lines land between the press and the first motion.
        term.lock().unwrap().process(b"l20\r\nl21\r\n");
        let to = Point::new(60.0, 40.0);
        view.on_event(
            &mut ws,
            &iced::Event::Mouse(mouse::Event::CursorMoved { position: to }),
            bounds(),
            mouse::Cursor::Available(to),
        );
        let after = term.lock().unwrap().live_selection().unwrap();

        assert_eq!(
            (after.start.1, after.end.1),
            (born.start.1 - 2, born.start.1 - 2),
            "both ends rode the rotation, and the drag stayed on one row"
        );
        assert!(after.end.0 > after.start.0, "the horizontal drag widened it");
    }

    /// Nothing but a rotation moves the band. There is one mechanism now,
    /// and it runs where the rows move; a handler that also tried to catch
    /// the band up would apply the same rotation a second time, which is
    /// the shape this replaced.
    #[test]
    fn only_a_rotation_moves_the_band() {
        let (term, marked) = banded(
            1000,
            4,
            40,
            Selection { start: (0, 0), end: (1, 1), block: false },
        );
        term.lock().unwrap().scroll_viewport_by(6);
        let view: TerminalView<()> = TerminalView::new(Arc::clone(&term)).focused(true);
        let mut ws = TerminalWidgetState::default();
        term.lock().unwrap().process(b"more\r\n");
        let after_output = term.lock().unwrap().live_selection().unwrap();

        // A hover and a wheel notch: neither is content moving, so
        // neither may touch the range.
        for ev in [
            iced::Event::Mouse(mouse::Event::CursorMoved { position: Point::new(40.0, 40.0) }),
            iced::Event::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Lines { x: 0.0, y: 1.0 },
            }),
        ] {
            view.on_event(
                &mut ws,
                &ev,
                bounds(),
                mouse::Cursor::Available(Point::new(40.0, 40.0)),
            );
        }
        assert_eq!(
            term.lock().unwrap().live_selection().unwrap(),
            after_output,
            "only the output moved it, and only once"
        );
        assert_eq!(banded_text(&term), marked, "so it is still on its own text");

        // And the one that used to: a press, output, then a motion that
        // never left the press cell. The band must ride that rotation
        // exactly once and stay a single cell. A second mechanism
        // catching it up beside the first is what doubled it.
        let at = Point::new(40.0, 40.0);
        view.on_event(
            &mut ws,
            &iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            bounds(),
            mouse::Cursor::Available(at),
        );
        let born = term.lock().unwrap().live_selection().expect("a band is born");
        assert!(born.is_empty(), "a press with no drag covers one cell");
        term.lock().unwrap().process(b"one\r\ntwo\r\n");
        view.on_event(
            &mut ws,
            &iced::Event::Mouse(mouse::Event::CursorMoved { position: at }),
            bounds(),
            mouse::Cursor::Available(at),
        );
        let dragged = term.lock().unwrap().live_selection().unwrap();
        assert_eq!(
            (dragged.start.1, dragged.end.1),
            (born.start.1 - 2, born.end.1 - 2),
            "one rotation, applied once"
        );
        assert!(dragged.is_empty(), "and the pointer never left its cell");
    }

    /// A DECSTBM sub-region scroll (a TUI redrawing under a fixed header)
    /// moves only the rows inside the region. The viewport offset cannot
    /// tell that apart from a full scroll, and a history counter cannot see
    /// it at all; the emulator rotates exactly the range it scrolled.
    #[test]
    fn a_sub_region_scroll_moves_only_the_band_inside_it() {
        let term = Arc::new(Mutex::new(
            TerminalState::new_no_pty_with_scrollback(24, 6, 100).unwrap(),
        ));
        // Six rows of content, then a scroll region over rows 3..6
        // (1-based), leaving rows 1 and 2 as the fixed header.
        for i in 0..6 {
            term.lock().unwrap().process(format!("l{i}\r\n").as_bytes());
        }
        term.lock().unwrap().process(b"\x1b[3;6r");

        // A band on the header (row 0), outside the region.
        term.lock()
            .unwrap()
            .set_selection(Selection { start: (0, 0), end: (1, 0), block: false });
        let header = banded_text(&term);
        let region_before = term.lock().unwrap().visible_text();
        term.lock().unwrap().process(b"\x1b[6;1Hnew\r\n");
        assert_ne!(
            term.lock().unwrap().visible_text(),
            region_before,
            "the region really did scroll (or the case below is vacuous)"
        );
        assert_eq!(
            banded_text(&term),
            header,
            "a band above the region never moved, because its rows never did"
        );

        // And one INSIDE it, which does move.
        term.lock()
            .unwrap()
            .set_selection(Selection { start: (0, 3), end: (1, 3), block: false });
        let inside = term.lock().unwrap().live_selection().unwrap();
        term.lock().unwrap().process(b"\x1b[6;1Hmore\r\n");
        let moved = term.lock().unwrap().live_selection().unwrap();
        assert_eq!(moved.start.1, inside.start.1 - 1, "a band inside it rode the scroll");
    }

    /// The alternate screen is a different buffer, so a band made against
    /// the main one is dropped on the flip, which is what alacritty's and
    /// kitty's own terminals do. The GHOST is the exception: it marks text
    /// in the main grid that the alt app is only covering, so it is parked
    /// and handed back when the app quits (VTE keeps a selection across the
    /// flip for the same reason).
    #[test]
    fn an_alt_screen_trip_drops_the_band_and_parks_the_ghost() {
        let (term, marked) = banded(
            100,
            4,
            6,
            Selection { start: (0, 0), end: (1, 1), block: false },
        );
        term.lock().unwrap().demote_selection();
        assert!(term.lock().unwrap().ghost_selection().is_some());

        term.lock().unwrap().process(b"\x1b[?1049h\x1b[Happ frame");
        {
            let s = term.lock().unwrap();
            assert!(s.in_alt_screen(), "the alt app is up");
            assert!(s.selection_range().is_none(), "nothing is banded in its grid");
            assert!(s.selection.alt_stash.is_some(), "the ghost is parked, not lost");
        }

        term.lock().unwrap().process(b"\x1b[?1049l");
        assert!(
            term.lock().unwrap().live_selection().is_none(),
            "it comes back demoted, never live"
        );
        assert_eq!(banded_text_ghost(&term), marked, "and still on its own text");
    }

    /// A LIVE band does not survive the trip: it is the highlight the user
    /// is working with, and the grid it named is not the one on screen.
    #[test]
    fn an_alt_screen_flip_drops_a_live_band() {
        let (term, _) = banded(
            100,
            4,
            6,
            Selection { start: (0, 0), end: (1, 1), block: false },
        );
        term.lock().unwrap().process(b"\x1b[?1049h");
        {
            let s = term.lock().unwrap();
            assert!(s.selection_range().is_none(), "the flip took it");
            assert!(
                s.selection.alt_stash.is_none(),
                "and nothing was parked: only a ghost is worth the trip back"
            );
        }
        term.lock().unwrap().process(b"\x1b[?1049l");
        assert!(
            term.lock().unwrap().selection_range().is_none(),
            "so there is nothing to come back to"
        );
    }

    fn banded_text_ghost(term: &Arc<Mutex<TerminalState>>) -> String {
        let s = term.lock().unwrap();
        let sel = s.ghost_selection().expect("the ghost survives");
        s.get_selection_text(&sel)
    }

    /// Overwriting the text under a band drops it. New behaviour, and the
    /// emulator's own: a band left standing over a line that has been
    /// rewritten highlights text the user never picked, and the copy
    /// gestures read the band.
    #[test]
    fn erasing_the_text_under_a_band_drops_it() {
        let (term, _) = banded(
            100,
            4,
            2,
            Selection { start: (0, 0), end: (1, 0), block: false },
        );
        assert!(term.lock().unwrap().live_selection().is_some());
        // Home, then erase the line the band is on.
        term.lock().unwrap().process(b"\x1b[1;1H\x1b[2K");
        assert!(
            term.lock().unwrap().selection_range().is_none(),
            "the text it marked is gone, so the band is too"
        );
    }

    /// A reflow reindexes every line, so no translation can put a band back
    /// on its text. alacritty drops it on a column change, which is what
    /// xterm and alacritty's own terminal do.
    #[test]
    fn a_reflow_drops_the_band() {
        let (term, _) = banded(
            100,
            4,
            6,
            Selection { start: (0, 0), end: (1, 1), block: false },
        );
        term.lock().unwrap().resize(12, 4);
        assert!(
            term.lock().unwrap().selection_range().is_none(),
            "a column change is not something a band survives"
        );
    }

    /// Shift+click extends from the press anchor, which lives inside the
    /// emulator. A backward drag leaves the range reading end-before-start
    /// once resolved, so an extend computed from the resolved range would
    /// anchor at the wrong corner.
    #[test]
    fn shift_click_extends_from_the_original_press() {
        let term = Arc::new(Mutex::new(
            TerminalState::new_no_pty_with_scrollback(24, 6, 100).unwrap(),
        ));
        term.lock().unwrap().process(b"abcdefghij\r\nklmnopqrst\r\n");
        let view: TerminalView<()> = TerminalView::new(Arc::clone(&term)).focused(true);
        let mut ws = TerminalWidgetState::default();

        // Press in the middle of row 1, drag LEFT (backwards).
        let press = Point::new(60.0, 40.0);
        view.on_event(
            &mut ws,
            &iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            bounds(),
            mouse::Cursor::Available(press),
        );
        let back = Point::new(10.0, 40.0);
        view.on_event(
            &mut ws,
            &iced::Event::Mouse(mouse::Event::CursorMoved { position: back }),
            bounds(),
            mouse::Cursor::Available(back),
        );
        view.on_event(
            &mut ws,
            &iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            bounds(),
            mouse::Cursor::Available(back),
        );
        let anchor_col = term.lock().unwrap().live_selection().unwrap().end.0;

        // Shift+click to the RIGHT of the press: the anchor holds, so the
        // range now runs from it rightwards.
        ws.modifiers = keyboard::Modifiers::SHIFT;
        let far = Point::new(200.0, 40.0);
        view.on_event(
            &mut ws,
            &iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            bounds(),
            mouse::Cursor::Available(far),
        );
        let sel = term.lock().unwrap().live_selection().unwrap();
        assert_eq!(sel.start.0, anchor_col, "the press anchor held");
        assert!(sel.end.0 > anchor_col, "and the click became the far end");
    }

    /// `screen_as_ansi` must reproduce the visible screen when fed to a
    /// fresh emulator: text, named / indexed / RGB colors, wide (CJK)
    /// glyphs and the visual attribute flags all round-trip cell-exact.
    /// Backs the transcript viewer's final-alt-frame materialization.
    #[test]
    fn screen_as_ansi_roundtrips_the_visible_screen() {
        use alacritty_terminal::grid::Dimensions;
        use alacritty_terminal::index::{Column, Line};
        use alacritty_terminal::term::cell::Flags as CellFlags;

        let mut a = TerminalState::new_no_pty(24, 7).unwrap();
        a.process(b"\x1b[31mred\x1b[0m plain\r\n");
        a.process(b"\x1b[1;44mB on blue\x1b[0m\r\n");
        a.process(b"\x1b[38;2;1;2;3mrgb\x1b[0m \x1b[7minv\x1b[0m \x1b[38;5;123midx\x1b[0m\r\n");
        a.process("wide 漢字 ok\r\n".as_bytes());
        // A background bar with trailing colored blanks (the top header
        // pattern) followed by default-styled text.
        a.process(b"\x1b[4mu\x1b[0m\x1b[42m  \x1b[0mtail\r\n");
        // Every underline variant, plus an RGB underline color: each
        // must keep its exact style through the round-trip, not reduce
        // to plain underline.
        a.process(b"\x1b[4:2md\x1b[0m\x1b[4:3mc\x1b[0m\x1b[4:4mo\x1b[0m\x1b[4:5ma\x1b[0m\r\n");
        a.process(b"\x1b[4m\x1b[58;2;10;20;30mUC\x1b[0m");

        let bytes = a.screen_as_ansi();
        let mut b = TerminalState::new_no_pty(24, 7).unwrap();
        b.process(&bytes);

        let style = CellFlags::INVERSE
            | CellFlags::BOLD
            | CellFlags::ITALIC
            | CellFlags::DIM
            | CellFlags::HIDDEN
            | CellFlags::STRIKEOUT
            | CellFlags::ALL_UNDERLINES;
        let ga = a.backend.term.grid();
        let gb = b.backend.term.grid();
        assert_eq!(ga.screen_lines(), gb.screen_lines());
        for r in 0..ga.screen_lines() as i32 {
            for c in 0..ga.columns() {
                let ca = &ga[Line(r)][Column(c)];
                let cb = &gb[Line(r)][Column(c)];
                let norm = |ch: char| if ch == '\0' { ' ' } else { ch };
                assert_eq!(norm(ca.c), norm(cb.c), "char at {r},{c}");
                assert_eq!(ca.fg, cb.fg, "fg at {r},{c}");
                assert_eq!(ca.bg, cb.bg, "bg at {r},{c}");
                assert_eq!(ca.flags & style, cb.flags & style, "flags at {r},{c}");
                assert_eq!(
                    ca.underline_color(),
                    cb.underline_color(),
                    "underline color at {r},{c}"
                );
            }
        }
    }

    /// A surface rendered unfocused BY CONSTRUCTION keeps its selection.
    /// The session player replays into such a widget (its keys are
    /// transport controls, so it never takes focus), and while the
    /// lose-focus sweep tested `!focused` alone, the first mouse motion
    /// of the drag that made a selection wiped it: nothing in a
    /// recording could be selected, let alone copied.
    #[test]
    fn a_never_focused_surface_keeps_its_selection() {
        let (view, mut ws, term) = view_state_and_term();
        let view = view.focused(false);
        term.lock()
            .unwrap()
            .set_selection(Selection { start: (0, 0), end: (5, 0), block: false });
        let ev = iced::Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(40.0, 40.0),
        });
        let cursor = mouse::Cursor::Available(Point::new(40.0, 40.0));
        view.on_event(&mut ws, &ev, bounds(), cursor);
        assert!(
            band(&term).is_some(),
            "a display-only surface never had focus to lose"
        );
    }

    /// The other half of the same rule: a pane that WAS focused drops its
    /// highlight once it isn't. Split a tab three ways and every pane you
    /// ever selected in would otherwise stay lit, with nothing saying
    /// which one the next copy takes.
    #[test]
    fn a_pane_that_loses_focus_drops_its_highlight() {
        let term = Arc::new(Mutex::new(TerminalState::new_no_pty(80, 24).unwrap()));
        let focused: TerminalView<()> = TerminalView::new(Arc::clone(&term)).focused(true);
        let unfocused: TerminalView<()> = TerminalView::new(Arc::clone(&term)).focused(false);
        let mut ws = TerminalWidgetState::default();
        term.lock()
            .unwrap()
            .set_selection(Selection { start: (0, 0), end: (5, 0), block: false });
        let ev = iced::Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(40.0, 40.0),
        });
        let cursor = mouse::Cursor::Available(Point::new(40.0, 40.0));

        focused.on_event(&mut ws, &ev, bounds(), cursor);
        assert!(band(&term).is_some(), "the focused pane keeps its highlight");

        unfocused.on_event(&mut ws, &ev, bounds(), cursor);
        assert!(band(&term).is_none(), "the pane being left drops its highlight");
        assert!(
            term.lock().unwrap().ghost_selection().is_some(),
            "it is demoted, not forgotten: the pane still answers what was last selected"
        );

        // Losing focus is a TRANSITION, not a standing condition: the
        // dismissal reads the emulator, and every widget is handed every
        // event, so a per-event test would take the state lock on each
        // mouse move in each unfocused pane. A band planted while the
        // pane is already unfocused (the session player selects into
        // one) has to survive the events that follow.
        term.lock()
            .unwrap()
            .set_selection(Selection { start: (0, 0), end: (5, 0), block: false });
        unfocused.on_event(&mut ws, &ev, bounds(), cursor);
        assert!(
            band(&term).is_some(),
            "an already-unfocused pane is not losing focus again"
        );
    }

    /// A key event carrying `key`, with no modifiers: enough for the
    /// chord arm, which resolves through the app's matcher rather than
    /// reading the modifiers itself.
    fn key_press(key: keyboard::Key) -> iced::Event {
        iced::Event::Keyboard(keyboard::Event::KeyPressed {
            key: key.clone(),
            modified_key: key.clone(),
            physical_key: keyboard::key::Physical::Unidentified(
                keyboard::key::NativeCode::Unidentified,
            ),
            location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::empty(),
            text: None,
            repeat: false,
        })
    }

    /// The session player renders its stage unfocused (its keys are the
    /// transport, so it must not take the "typing clears the highlight"
    /// path), which used to mean its chords never fired either: a
    /// recording could be selected with the mouse but not copied from
    /// the keyboard. `with_chords_unfocused` is the opt-in for a surface
    /// that is the only terminal on screen.
    #[test]
    fn chords_fire_unfocused_when_the_surface_opts_in() {
        let (view, mut ws, term) = view_state_and_term();
        let view = view
            .focused(false)
            .with_terminal_chords(Box::new(|_, _| Some(TerminalChordAction::SelectAll)))
            .with_chords_unfocused(true);
        view.on_event(
            &mut ws,
            &key_press(keyboard::Key::Character("a".into())),
            bounds(),
            mouse::Cursor::Unavailable,
        );
        assert!(band(&term).is_some(), "select-all must reach the replay");
    }

    /// The other side of the same gate: without the opt-in an unfocused
    /// widget ignores the chords. Key events reach every widget in the
    /// tree, so this is what keeps a three-way split from running the
    /// copy chord three times.
    #[test]
    fn chords_stay_focus_gated_by_default() {
        let (view, mut ws, term) = view_state_and_term();
        let view = view
            .focused(false)
            .with_terminal_chords(Box::new(|_, _| Some(TerminalChordAction::SelectAll)));
        view.on_event(
            &mut ws,
            &key_press(keyboard::Key::Character("a".into())),
            bounds(),
            mouse::Cursor::Unavailable,
        );
        assert!(band(&term).is_none(), "an unfocused pane declines the chord");
    }

    /// Everything a dead session can leave armed, in one pane, so the
    /// reset is asserted against the state it exists for.
    fn state_with_stale_modes() -> TerminalState {
        let mut term = TerminalState::new_no_pty(80, 24).unwrap();
        // What a killed tmux / vim leaves behind: any-motion tracking
        // (1003) with both encodings (1005/1006), focus reporting (1004),
        // bracketed paste (2004), application cursor keys (1), autowrap
        // off (7) and a hidden cursor (25).
        term.process(b"\x1b[?1;1003;1004;1005;1006;2004h\x1b[?7;25l");
        term
    }

    /// The reset the app feeds on disconnect and on every fresh session
    /// (`SESSION_MODE_RESET`) must clear every mode the widget's
    /// mouse-report gate reads. Guard for the reconnect-garbage bug: stale
    /// 1000/1002/1003/1006 left by a dead session made the widget keep
    /// synthesizing SGR reports into a shell that never asked for them,
    /// and the shell's echo of those reports landed on screen as text.
    /// Regression at the gate level: with the modes cleared, a pointer
    /// move must produce NO report.
    #[test]
    fn session_reset_clears_mouse_tracking_and_blocks_reports() {
        use alacritty_terminal::term::TermMode;

        let mut term = state_with_stale_modes();
        assert!(
            term.backend.term.mode().intersects(TermMode::MOUSE_MODE),
            "precondition: stale mouse tracking armed"
        );

        term.process(crate::SESSION_MODE_RESET);

        let mode = *term.backend.term.mode();
        assert!(
            !mode.intersects(TermMode::MOUSE_MODE),
            "mouse tracking cleared"
        );
        assert!(!mode.contains(TermMode::SGR_MOUSE), "SGR encoding cleared");
        assert!(!mode.contains(TermMode::UTF8_MOUSE), "UTF-8 encoding cleared");
        assert!(
            !mode.contains(TermMode::FOCUS_IN_OUT),
            "focus reporting cleared"
        );
        assert!(
            !mode.contains(TermMode::BRACKETED_PASTE),
            "bracketed paste cleared"
        );
        assert!(
            !mode.contains(TermMode::APP_CURSOR),
            "application cursor keys cleared"
        );
        assert!(mode.contains(TermMode::LINE_WRAP), "autowrap back on");
        assert!(mode.contains(TermMode::SHOW_CURSOR), "cursor shown again");

        // The widget's report gate reads the mode back from the state: a
        // pointer move must not synthesize a report any more.
        let view = TerminalView::<()>::new(Arc::new(Mutex::new(term)));
        let mut ws = TerminalWidgetState::default();
        let ev = iced::Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(40.0, 40.0),
        });
        let action = view.handle_mouse_report(
            &mut ws,
            &ev,
            bounds(),
            mouse::Cursor::Available(Point::new(40.0, 40.0)),
            mode,
            80,
            24,
        );
        assert!(action.is_none(), "no SGR report after the session reset");
    }

    /// The scrolling region a dead full-screen app left behind would pin
    /// the new shell's output inside its band, and DECSTBM homes the
    /// cursor, which is why the reset wraps it in DECSC/DECRC: the
    /// region goes back to the whole screen and the cursor stays where
    /// the session left it.
    #[test]
    fn session_reset_restores_the_scrolling_region_without_moving_the_cursor() {
        use alacritty_terminal::index::{Column, Line};

        let mut term = TerminalState::new_no_pty(80, 24).unwrap();
        // A band over the top 5 lines, cursor parked inside the pane.
        term.process(b"\x1b[1;5r\x1b[10;3H");
        let before = term.backend.term.grid().cursor.point;
        assert_eq!(before, alacritty_terminal::index::Point::new(Line(9), Column(2)));

        term.process(crate::SESSION_MODE_RESET);
        assert_eq!(
            term.backend.term.grid().cursor.point, before,
            "the region reset must not home the cursor"
        );

        // One line per row, no trailing newline: with the band still
        // armed the text would scroll inside rows 1-5 and the bottom of
        // the screen would stay empty.
        term.process(b"\x1b[H");
        for i in 0..24 {
            term.process(format!("L{i}").as_bytes());
            if i < 23 {
                term.process(b"\r\n");
            }
        }
        let last = (0..3)
            .map(|c| term.backend.term.grid()[Line(23)][Column(c)].c)
            .collect::<String>();
        assert_eq!(last, "L23", "output must reach the bottom row again");
    }

    /// A connection killed inside tmux / vim leaves the pane on the
    /// alternate screen. `LEAVE_ALT_SCREEN` puts it back on the real
    /// buffer (with its scrollback) exactly as the app's own clean exit
    /// would have, and is a no-op on a pane that never entered.
    #[test]
    fn leave_alt_screen_restores_the_primary_buffer() {
        use alacritty_terminal::index::{Column, Line};
        use alacritty_terminal::term::TermMode;

        let mut term = TerminalState::new_no_pty(80, 24).unwrap();
        term.process(b"shell output");
        // A full-screen app takes over and dies mid-frame.
        term.process(b"\x1b[?1049h\x1b[Happ frame");
        assert!(term.backend.term.mode().contains(TermMode::ALT_SCREEN));

        term.process(crate::LEAVE_ALT_SCREEN);

        assert!(
            !term.backend.term.mode().contains(TermMode::ALT_SCREEN),
            "back on the primary buffer"
        );
        let row0 = (0..12)
            .map(|c| term.backend.term.grid()[Line(0)][Column(c)].c)
            .collect::<String>();
        assert_eq!(row0, "shell output", "the real buffer is back");

        // Idempotent: a pane that never entered stays put.
        term.process(crate::LEAVE_ALT_SCREEN);
        assert!(!term.backend.term.mode().contains(TermMode::ALT_SCREEN));
        let row0 = (0..12)
            .map(|c| term.backend.term.grid()[Line(0)][Column(c)].c)
            .collect::<String>();
        assert_eq!(row0, "shell output");
    }

    // ── The viewport offset is the grid's display_offset ──

    /// The text on the viewport's top row, straight from the grid.
    fn top_row(state: &TerminalState) -> String {
        use alacritty_terminal::index::{Column, Line};
        let grid = state.backend.term.grid();
        let line = Line(-state.viewport_offset());
        (0..8)
            .map(|c| grid[line][Column(c)].c)
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    /// Feed `L<i>` lines, one per row, so a row's text says which one it is.
    fn numbered(state: &mut TerminalState, range: std::ops::Range<usize>) {
        for i in range {
            state.process(format!("L{i}\r\n").as_bytes());
        }
    }

    /// A viewport scrolled up keeps showing the same rows while output
    /// arrives: the grid raises `display_offset` as lines scroll into
    /// history, and the widget reads that back instead of keeping a count
    /// of its own.
    #[test]
    fn scrolled_viewport_holds_its_rows_through_output() {
        let (view, mut ws) = scrolled_view(200);
        numbered(&mut view.state.lock().unwrap(), 0..30);
        let cursor = mouse::Cursor::Available(Point::new(40.0, 40.0));
        let notch = iced::Event::Mouse(mouse::Event::WheelScrolled {
            delta: mouse::ScrollDelta::Lines { x: 0.0, y: 1.0 },
        });
        view.on_event(&mut ws, &notch, bounds(), cursor);
        assert_eq!(ws.scroll_offset.get(), 3, "one notch, three lines up");
        let before = top_row(&view.state.lock().unwrap());

        {
            let mut s = view.state.lock().unwrap();
            numbered(&mut s, 30..35);
            assert_eq!(s.viewport_offset(), 8, "five lines of output raise the offset by five");
            assert_eq!(top_row(&s), before, "the rows being read stay on screen");
        }
        // The next gesture builds on the grid's answer, not on a stale count.
        view.on_event(&mut ws, &notch, bounds(), cursor);
        assert_eq!(ws.scroll_offset.get(), 11);
    }

    /// A full scrollback keeps holding: the history stops growing at the
    /// cap, but lines still rotate through it, and the offset follows the
    /// rotation until the rows themselves leave the buffer.
    #[test]
    fn a_full_scrollback_still_holds_the_rows() {
        use alacritty_terminal::grid::Dimensions;
        let mut s = TerminalState::new_no_pty_with_scrollback(80, 3, 5).unwrap();
        numbered(&mut s, 0..8);
        assert_eq!(s.backend.term.grid().history_size(), 5, "at the cap");
        s.scroll_viewport_by(2);
        let before = top_row(&s);

        numbered(&mut s, 8..11);
        assert_eq!(s.backend.term.grid().history_size(), 5, "the cap does not move");
        assert_eq!(s.viewport_offset(), 5, "three more lines, three more up");
        assert_eq!(top_row(&s), before, "the rows being read stay on screen");

        // Once they fall off the top there is nothing older to show: the
        // offset stays at the cap and the view follows the oldest line
        // (fourteen lines plus the cursor row, eight kept: L7 on top).
        numbered(&mut s, 11..14);
        assert_eq!(s.viewport_offset(), 5);
        assert_eq!(top_row(&s), "L7");
    }

    /// Clearing the scrollback lands on the live edge (there is nothing
    /// above it to show), and output after that follows the edge until
    /// the user scrolls again.
    #[test]
    fn clear_scrollback_lands_on_the_live_edge() {
        let mut s = TerminalState::new_no_pty_with_scrollback(80, 3, 100).unwrap();
        numbered(&mut s, 0..10);
        s.scroll_viewport_by(4);
        assert_eq!(s.viewport_offset(), 4);
        s.clear_scrollback();
        assert_eq!(s.viewport_offset(), 0);
        numbered(&mut s, 10..13);
        assert_eq!(s.viewport_offset(), 0, "at the live edge, output is followed");
    }

    /// A resize keeps the same rows in view: a shorter window pushes lines
    /// into history and the offset rises with them, a taller one pulls
    /// lines back and the offset falls, down to the live edge.
    #[test]
    fn a_resize_keeps_the_rows_in_view() {
        let mut s = TerminalState::new_no_pty_with_scrollback(80, 5, 100).unwrap();
        numbered(&mut s, 0..10);
        s.scroll_viewport_by(3);
        let before = top_row(&s);
        s.resize(80, 3);
        assert_eq!(s.viewport_offset(), 5, "two rows pushed into history");
        assert_eq!(top_row(&s), before);
        s.resize(80, 8);
        assert_eq!(s.viewport_offset(), 0, "five rows pulled back onto the screen");
    }

    /// The alternate screen is a grid of its own with no history: it reads
    /// 0 while an app owns it, a scroll there goes nowhere, and the primary
    /// buffer's position is back untouched on the way out.
    #[test]
    fn alt_screen_round_trip_keeps_the_primary_offset() {
        let mut s = TerminalState::new_no_pty_with_scrollback(80, 3, 100).unwrap();
        numbered(&mut s, 0..10);
        s.scroll_viewport_by(3);
        s.process(b"\x1b[?1049h\x1b[Happ frame");
        assert_eq!(s.viewport_offset(), 0);
        assert_eq!(s.scroll_viewport_by(2), 0, "nothing to scroll into");
        s.process(b"\x1b[?1049l");
        assert_eq!(s.viewport_offset(), 3);
    }

    /// A program scrolling a region that does not start at the top row (a
    /// fixed header under DECSTBM) bumps the grid's raw offset without
    /// adding a line to history. What the widget reads never passes the
    /// top, the visible-screen export stays inside the buffer, and the
    /// first gesture after it measures from the row on screen rather than
    /// from the raw excess.
    #[test]
    fn a_region_scroll_never_reads_past_the_top() {
        use alacritty_terminal::grid::Dimensions;
        let mut s = TerminalState::new_no_pty_with_scrollback(80, 4, 100).unwrap();
        numbered(&mut s, 0..10);
        s.scroll_viewport_by(3);
        s.process(b"\x1b[2;4r\x1b[4;1H");
        for i in 0..10 {
            s.process(format!("R{i}\n").as_bytes());
        }
        let grid = s.backend.term.grid();
        let history = grid.history_size() as i32;
        assert!(grid.display_offset() as i32 > history, "the raw offset passed the top");
        assert_eq!(s.viewport_offset(), history, "read at the top, never past it");
        assert_eq!(top_row(&s), "L0", "the oldest row the grid holds");
        assert_eq!(s.visible_text(), "L0\nL1\nL2\nL3");
        assert_eq!(s.scroll_viewport_by(-3), history - 3, "a wheel-down moves from the row on screen");
    }

    /// A queued absolute target resolves against the grid as it is when
    /// applied: past the top lands on the oldest screen, 0 (or less) is
    /// the live edge.
    #[test]
    fn an_absolute_target_is_clamped_to_the_grid() {
        let mut s = TerminalState::new_no_pty_with_scrollback(80, 3, 100).unwrap();
        numbered(&mut s, 0..10);
        assert_eq!(s.scroll_viewport_to(i32::MAX), 8, "eleven rows, three on screen");
        assert_eq!(s.scroll_viewport_to(0), 0);
        assert_eq!(s.scroll_viewport_to(-4), 0);
    }
