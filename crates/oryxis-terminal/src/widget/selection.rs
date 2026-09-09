use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection as AlacSelection, SelectionType};

/// A selection range in **grid-line coordinates** (alacritty `Line`,
/// signed: negative = scrollback, 0..screen_lines = live screen), with
/// both ends INCLUSIVE. The draw pass translates line → visible row
/// against the current scroll offset.
///
/// This is a resolved READING of the selection, not where it is kept: the
/// range itself lives in `Term::selection` (see the `TerminalState` impl
/// below), which is what makes it follow its own text. Every value of
/// this type comes from a `to_range` under the state lock, so it is
/// already ordered and already clamped to the grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub start: (u16, i32), // (col, line)
    pub end: (u16, i32),
    /// When true this is a rectangular (column / block) selection
    /// (Alt+drag): every row takes the same `[min_col, max_col]` slice,
    /// instead of the flowing line-by-line range. Carried so the draw
    /// highlight and text extraction agree.
    pub block: bool,
}

impl Selection {
    /// Returns (start, end) ordered top-left to bottom-right. Only
    /// meaningful for flowing (non-block) selections; block selections
    /// use [`Selection::block_bounds`] instead.
    pub fn ordered(&self) -> ((u16, i32), (u16, i32)) {
        if self.start.1 < self.end.1
            || (self.start.1 == self.end.1 && self.start.0 <= self.end.0)
        {
            (self.start, self.end)
        } else {
            (self.end, self.start)
        }
    }

    /// `(min_col, max_col, min_line, max_line)` of the rectangle spanned
    /// by the two corners. Used for block selections, where each axis is
    /// ordered independently.
    pub fn block_bounds(&self) -> (u16, u16, i32, i32) {
        (
            self.start.0.min(self.end.0),
            self.start.0.max(self.end.0),
            self.start.1.min(self.end.1),
            self.start.1.max(self.end.1),
        )
    }

    /// Whether the range covers exactly one cell. That is what a press
    /// with no drag leaves behind, so it is also the test for "this
    /// gesture was a click": the privacy pin and the link hint both ask
    /// it on release.
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// xterm right-click "extend": move the boundary NEARER to `click`
    /// onto the click point, keeping the far anchor fixed. Returns a new
    /// flowing selection anchored at the far end. Distance compares line
    /// first, then column, matching how a terminal grid reads. Block
    /// selections aren't extended this way (right-click extend is a
    /// flowing-selection gesture); the caller leaves them untouched.
    pub fn extended_to(&self, click: (u16, i32)) -> Selection {
        let (a, b) = self.ordered();
        let dist = |p: (u16, i32)| {
            (
                (p.1 - click.1).unsigned_abs(),
                (p.0 as i32 - click.0 as i32).unsigned_abs(),
            )
        };
        // Click nearer the top-left end (a) -> anchor at b; else anchor at a.
        let anchor = if dist(a) <= dist(b) { b } else { a };
        Selection { start: anchor, end: click, block: false }
    }
}

/// The selection lives in `Term::selection`, and this is the whole reason:
/// alacritty moves and invalidates it AT THE SOURCE. `scroll_up_relative`
/// rotates it before the grid rotates, `swap_alt` drops it, `resize` drops
/// it on a column change and rotates it otherwise, and the erase handlers
/// filter it out when the text under it is overwritten. None of that reads
/// `history_size`, `total_lines` or `display_offset`, so a full scrollback
/// (where all three stop moving while rows keep rotating) is not a case
/// there at all. Observing the grid from outside once per frame and
/// deducing what moved is the shape that needs a monotonic counter, a
/// baseline to diff it against, and a rule per invalidation; hooking the
/// mutation needs none of them.
///
/// The adapter is the only thing we owe. Our range is two INCLUSIVE cells;
/// alacritty's is two half-cell anchors, so a write is
/// `new(start, Left)` + `update(end, Right)` + `include_all()`, whose last
/// step fixes the sides for a backwards drag, and a read is `to_range`,
/// which resolves the sides back to inclusive cells. `to_range` also
/// answers `None` once the range has rotated below the oldest line the
/// grid still holds, which is why it is the single predicate for "is
/// there a selection": `term.selection.is_some()` stays true there and
/// would band nothing.
impl crate::widget::TerminalState {
    /// The live range in our own coordinates, or `None` when there is no
    /// selection or it has rotated out of the buffer entirely.
    pub fn selection_range(&self) -> Option<Selection> {
        let term = &self.backend.term;
        let range = term.selection.as_ref()?.to_range(term)?;
        Some(Selection {
            start: (range.start.column.0 as u16, range.start.line.0),
            end: (range.end.column.0 as u16, range.end.line.0),
            block: range.is_block,
        })
    }

    /// The range only while it is a LIVE highlight, skipping a demoted
    /// one. What the copy gestures and the strong band read.
    pub fn live_selection(&self) -> Option<Selection> {
        (!self.selection.demoted).then(|| self.selection_range()).flatten()
    }

    /// The range only while it is the faint PRIMARY ghost.
    pub fn ghost_selection(&self) -> Option<Selection> {
        self.selection.demoted.then(|| self.selection_range()).flatten()
    }

    /// Begin a selection at `cell`, live. `block` starts the rectangular
    /// (Alt+drag) kind, which alacritty tracks as its own selection type
    /// so the rotation and the range resolution both stay correct.
    pub fn start_selection(&mut self, cell: (u16, i32), block: bool) {
        self.write_selection(cell, cell, block);
    }

    /// Move the free end of the live selection to `cell`, keeping the
    /// anchor alacritty is holding. The anchor never leaves the emulator,
    /// so a rotation between two motion events moves both ends together.
    pub fn update_selection(&mut self, cell: (u16, i32)) {
        let point = Self::selection_point(&self.backend.term, cell);
        let Some(sel) = self.backend.term.selection.as_mut() else { return };
        sel.update(point, Side::Right);
        sel.include_all();
        self.selection.demoted = false;
    }

    /// Replace the range outright, live. For the selections we compute
    /// ourselves (word / line / paragraph expansion, the smart-select of a
    /// URL, right-click extend, select-all), where the two ends are known
    /// up front.
    pub fn set_selection(&mut self, sel: Selection) {
        self.write_selection(sel.start, sel.end, sel.block);
    }

    /// Drop the selection: nothing banded, no ghost left behind.
    pub fn clear_selection(&mut self) {
        self.backend.term.selection = None;
        self.selection.demoted = false;
        self.selection.alt_stash = None;
    }

    /// Demote the live band to the faint PRIMARY ghost. The range is
    /// untouched, so it keeps rotating and dying with its own text; only
    /// how it is drawn changes.
    pub fn demote_selection(&mut self) {
        if self.backend.term.selection.is_some() {
            self.selection.demoted = true;
        }
    }

    /// Whether an alternate-screen app is up. The alt grid is a different
    /// buffer, so a range made against the main one is neither drawn nor
    /// extended while it is.
    pub fn in_alt_screen(&self) -> bool {
        self.backend
            .term
            .mode()
            .contains(alacritty_terminal::term::TermMode::ALT_SCREEN)
    }

    fn write_selection(&mut self, start: (u16, i32), end: (u16, i32), block: bool) {
        let ty = if block { SelectionType::Block } else { SelectionType::Simple };
        let term = &mut self.backend.term;
        let mut sel =
            AlacSelection::new(ty, Self::selection_point(term, start), Side::Left);
        sel.update(Self::selection_point(term, end), Side::Right);
        // Both ends inclusive whichever way the drag runs: `include_all`
        // reads the two points' order and sets the sides to match, and on
        // a single cell it yields Left..Right, which resolves back to that
        // one cell rather than to nothing.
        sel.include_all();
        term.selection = Some(sel);
        self.selection.demoted = false;
    }

    /// A cell as an alacritty point, with the column clamped into the
    /// grid: `pixel_to_cell` floors the column low but not high, so a drag
    /// into the right padding reports one past the last column, and the
    /// range resolution would carry it into a row index. The LINE is left
    /// alone, since a range legitimately points outside the grid once
    /// rotation has carried it off the top.
    fn selection_point(
        term: &alacritty_terminal::Term<crate::backend::EventProxy>,
        cell: (u16, i32),
    ) -> Point {
        use alacritty_terminal::grid::Dimensions;
        let last_col = term.grid().columns().saturating_sub(1);
        Point::new(Line(cell.1), Column((cell.0 as usize).min(last_col)))
    }
}

/// Granularity of a double/triple-click selection. A plain single-click
/// drag uses per-cell granularity and is represented by `None` in the
/// widget's `select_anchor`, so this enum only needs the two expanded
/// modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SelectGranularity {
    Word,
    Line,
    Paragraph,
}

/// Next click count given the previous count and whether this press is
/// consecutive (within the time + distance window of the last one). Cycles
/// 1->2->3->4->1 so a fifth rapid click restarts at single-click, matching
/// the iTerm / GNOME behaviour. Pure so the classification can be tested
/// without driving real mouse timing.
pub(crate) fn next_click_count(prev: Option<u8>, consecutive: bool) -> u8 {
    match prev {
        Some(c) if consecutive => (c % 4) + 1,
        _ => 1,
    }
}

/// Combine two selections into the range spanning from the earliest
/// ordered start to the latest ordered end. Used to extend a word/line
/// drag: the anchor's word/line unioned with the cursor's word/line.
pub(crate) fn union_selection(a: Selection, b: Selection) -> Selection {
    let (a0, a1) = a.ordered();
    let (b0, b1) = b.ordered();
    // Compare cells in (line, col) order.
    let lt = |x: (u16, i32), y: (u16, i32)| x.1 < y.1 || (x.1 == y.1 && x.0 < y.0);
    Selection {
        start: if lt(a0, b0) { a0 } else { b0 },
        end: if lt(a1, b1) { b1 } else { a1 },
        block: false,
    }
}

#[cfg(test)]
mod selection_tests {
    use super::Selection;
    use crate::widget::TerminalState;

    fn state_with(lines: &[&str]) -> TerminalState {
        let mut state = TerminalState::new_no_pty(40, 6).unwrap();
        // Each line ends with CRLF so the emulator moves to column 0 of the
        // next row (LF alone only moves down, keeping the column).
        let mut text = String::new();
        for l in lines {
            text.push_str(l);
            text.push_str("\r\n");
        }
        state.process(text.as_bytes());
        state
    }

    #[test]
    fn extend_moves_the_nearer_boundary_keeping_the_far_anchor() {
        // Selection from (2, line 1) to (8, line 3); ordered a=(2,1),
        // b=(8,3). A click near the top end (1, line 0) moves the near
        // boundary and anchors at the far end b.
        let sel = Selection { start: (2, 1), end: (8, 3), block: false };
        let near_top = sel.extended_to((1, 0));
        assert_eq!(near_top.start, (8, 3), "far anchor kept");
        assert_eq!(near_top.end, (1, 0), "near end moved to the click");
        assert!(!near_top.block);

        // A click past the bottom end anchors at the top end a instead.
        let near_bottom = sel.extended_to((5, 9));
        assert_eq!(near_bottom.start, (2, 1), "far anchor kept");
        assert_eq!(near_bottom.end, (5, 9));
    }

    #[test]
    fn extend_uses_column_to_break_a_same_line_tie() {
        // Both ends on the same line: distance ties on line, so the
        // column decides which boundary is nearer.
        let sel = Selection { start: (2, 5), end: (10, 5), block: false };
        let near_left = sel.extended_to((3, 5));
        assert_eq!(near_left.start, (10, 5), "anchored at the right end");
        assert_eq!(near_left.end, (3, 5));
    }

    #[test]
    fn flowing_selection_spans_full_middle_rows() {
        // Selected from col 3 of row 0 to col 2 of row 2, a flowing
        // selection takes the tail of row 0, the WHOLE middle row, and the
        // head of row 2. The full middle row is what distinguishes it from
        // a block grab. (Trailing whitespace on intermediate rows is
        // pre-existing behaviour, so assert structure, not the exact run.)
        let state = state_with(&["abcde", "fghij", "klmno"]);
        let sel = Selection { start: (3, 0), end: (2, 2), block: false };
        let text = state.get_selection_text(&sel);
        let rows: Vec<&str> = text.split('\n').collect();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].trim_end(), "de");
        assert_eq!(rows[1].trim_end(), "fghij"); // full middle row
        assert_eq!(rows[2], "klm");
    }

    #[test]
    fn block_selection_takes_same_columns_each_row() {
        // The same two corners as a block grab only the [3..=4] column
        // slice of every row, not the flowing range.
        let state = state_with(&["abcde", "fghij", "klmno"]);
        let sel = Selection { start: (3, 0), end: (2, 2), block: true };
        // cols 2..=3 (min/max of 3 and 2) per row: "cd", "hi", "mn".
        assert_eq!(state.get_selection_text(&sel), "cd\nhi\nmn");
    }

    #[test]
    fn block_selection_preserves_interior_trailing_spaces() {
        // Two "columns" of unequal width: the block must keep each row's
        // slice verbatim (including trailing spaces) so the columns stay
        // aligned. Row 1's slice is shorter, padded with spaces.
        let state = state_with(&["aa  bb", "cc  dd", "x     "]);
        // Grab cols 0..=3: "aa  ", "cc  ", "x   ". Per-row trim would
        // collapse these to "aa", "cc", "x" and break alignment.
        let sel = Selection { start: (0, 0), end: (3, 2), block: true };
        assert_eq!(state.get_selection_text(&sel), "aa  \ncc  \nx   ");
    }

    #[test]
    fn flowing_selection_trims_trailing_per_row() {
        // A flowing single-row selection that runs into trailing blanks
        // drops them (standard terminal copy behaviour).
        let state = state_with(&["hello     ", "world"]);
        let sel = Selection { start: (0, 0), end: (9, 0), block: false };
        assert_eq!(state.get_selection_text(&sel), "hello");
    }

    #[test]
    fn flowing_selection_joins_soft_wrapped_rows() {
        // A 40-char line at width 20 occupies two physical rows with a
        // WRAPLINE on row 0's last cell. The copy must join them back into
        // one logical line (tmux / xterm behaviour), not insert a newline at
        // the wrap — that is what keeps a long URL intact when selected.
        let mut state = TerminalState::new_no_pty(20, 6).unwrap();
        let url = "u".repeat(40);
        state.process(url.as_bytes());
        let sel = Selection { start: (0, 0), end: (19, 1), block: false };
        assert_eq!(state.get_selection_text(&sel), url);
    }

    #[test]
    fn flowing_selection_keeps_real_break_after_wrapped_line() {
        // A wrapped line followed by a genuine CRLF: the wrap joins silently,
        // but the real line break still copies out as a newline.
        let mut state = TerminalState::new_no_pty(20, 6).unwrap();
        let line = format!("{}\r\n", "u".repeat(40));
        state.process(line.as_bytes());
        let sel = Selection { start: (0, 0), end: (19, 2), block: false };
        assert_eq!(state.get_selection_text(&sel), format!("{}\n", "u".repeat(40)));
    }

    #[test]
    fn flowing_selection_keeps_spaces_straddling_the_wrap() {
        // 17 chars plus 3 spaces fill row 0 exactly, so the run of spaces
        // sits at the wrap boundary and "bcd" continues on row 1. Those
        // spaces are interior content of the logical line: a wrapped row is
        // never trimmed (alacritty's `line_length` rule), so the copy must
        // keep them instead of eating them with the trailing-space trim.
        let mut state = TerminalState::new_no_pty(20, 6).unwrap();
        let logical = format!("{}   bcd", "a".repeat(17));
        state.process(logical.as_bytes());
        let sel = Selection { start: (0, 0), end: (19, 1), block: false };
        assert_eq!(state.get_selection_text(&sel), logical);
    }

    #[test]
    fn flowing_selection_skips_leading_spacer_at_wrap() {
        // A wide CJK glyph that doesn't fit in the last column wraps whole:
        // the row ends in a LEADING_WIDE_CHAR_SPACER ghost cell (which also
        // carries WRAPLINE). With wrapped rows kept untrimmed, that ghost
        // must be skipped or it copies out as a stray space inside the
        // joined line.
        let mut state = TerminalState::new_no_pty(20, 6).unwrap();
        let logical = format!("{}你x", "a".repeat(19));
        state.process(logical.as_bytes());
        let sel = Selection { start: (0, 0), end: (19, 1), block: false };
        assert_eq!(state.get_selection_text(&sel), logical);
    }

    #[test]
    fn flowing_selection_skips_wide_char_spacers() {
        // Each CJK glyph occupies two grid cells: the leading cell holds the
        // character, the trailing cell is a WIDE_CHAR_SPACER whose `c` is a
        // space. Copying must emit the glyph once, not "glyph + space".
        // (GitHub issue #51: extra spaces in the middle of copied content.)
        let state = state_with(&["专用发票"]);
        // 4 glyphs span columns 0..=7 (two cells each).
        let sel = Selection { start: (0, 0), end: (7, 0), block: false };
        assert_eq!(state.get_selection_text(&sel), "专用发票");
    }

    #[test]
    fn block_selection_skips_wide_char_spacers() {
        // The block branch must drop spacer cells too.
        let state = state_with(&["专用发票"]);
        let sel = Selection { start: (0, 0), end: (7, 0), block: true };
        assert_eq!(state.get_selection_text(&sel), "专用发票");
    }

    #[test]
    fn smart_select_grabs_whole_url() {
        // A click anywhere inside the URL returns the full token span,
        // even though "/" and ":" are word delimiters that would split it.
        let state = state_with(&["see https://example.com x"]);
        let (c0, c1) =
            crate::widget::smart_span_at(&state.backend.term, &state.palette, 0, 8)
                .expect("URL should be detected");
        let sel = Selection { start: (c0, 0), end: (c1, 0), block: false };
        assert_eq!(state.get_selection_text(&sel), "https://example.com");
    }

    #[test]
    fn smart_select_misses_plain_word() {
        // A click on a non-token word returns None so the caller falls
        // back to delimiter-word selection.
        let state = state_with(&["see https://example.com x"]);
        assert!(crate::widget::smart_span_at(&state.backend.term, &state.palette, 0, 1).is_none());
    }

    #[test]
    fn click_count_classification_cycles_1_to_4() {
        use super::next_click_count;
        // No previous click, or a non-consecutive press, is always single.
        assert_eq!(next_click_count(None, false), 1);
        assert_eq!(next_click_count(None, true), 1);
        assert_eq!(next_click_count(Some(2), false), 1);
        assert_eq!(next_click_count(Some(4), false), 1);
        // Consecutive presses advance single -> double -> triple -> quad,
        // then wrap back to single on the fifth.
        assert_eq!(next_click_count(Some(1), true), 2);
        assert_eq!(next_click_count(Some(2), true), 3);
        assert_eq!(next_click_count(Some(3), true), 4);
        assert_eq!(next_click_count(Some(4), true), 1);
    }

    #[test]
    fn paragraph_selection_spans_to_blank_lines() {
        use std::sync::{Arc, Mutex};
        let arc = Arc::new(Mutex::new(TerminalState::new_no_pty(40, 8).unwrap()));
        // Two paragraphs separated by a blank line.
        arc.lock()
            .unwrap()
            .process(b"para one a\r\npara one b\r\n\r\npara two\r\n");
        let view = crate::widget::TerminalView::<()>::new(Arc::clone(&arc));
        let sel = {
            let mut guard = arc.lock().unwrap();
            // Click on the second line of the first paragraph.
            view.semantic_selection(
                &mut guard.backend,
                (0, 1),
                super::SelectGranularity::Paragraph,
            )
        };
        let text = arc.lock().unwrap().get_selection_text(&sel);
        assert_eq!(text, "para one a\npara one b");
    }
}
