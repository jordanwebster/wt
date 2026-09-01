//! The pure half of `setup`'s inline terminal interface (A76, §14.7).
//!
//! `update` and `view` are total functions over the state, so every screen the
//! command can draw is reachable from a test without a terminal. Raw mode,
//! reads and writes live in wt-sys; nothing here observes a clock, so the
//! animation frame arrives as a counter.

use serde::{Deserialize, Serialize};

/// The glyphs a frame cycles through while work is outstanding.
const SPINNER: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];
const SPINNER_ASCII: [&str; 4] = ["|", "/", "-", "\\"];

/// One decoded keypress.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Key {
    Up,
    Down,
    Space,
    Enter,
    Backspace,
    Escape,
    Interrupt,
    Char(char),
}

/// What the terminal can do, gathered once and on every resize.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Viewport {
    pub rows: u16,
    pub cols: u16,
    pub color: bool,
    pub unicode: bool,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            rows: 24,
            cols: 80,
            color: false,
            unicode: true,
        }
    }
}

impl Viewport {
    /// The most lines the live region may occupy.
    ///
    /// A region taller than the viewport makes the cursor arithmetic that
    /// rewinds it wrong, which corrupts the scrollback permanently rather than
    /// merely looking bad.
    pub fn region_rows(self) -> usize {
        // Never more than the terminal actually has: a floor here would let
        // the region draw rows that do not exist, and the rewind that follows
        // walks up through the scrollback instead of its own frame.
        let rows = usize::from(self.rows);
        rows.saturating_sub(2).clamp(1, rows.max(1))
    }

    /// The widest line that will not wrap.
    pub fn line_width(self) -> usize {
        usize::from(self.cols).saturating_sub(1).max(1)
    }

    /// The "there is more here" marker this terminal can render.
    pub fn ellipsis(self) -> &'static str {
        if self.unicode {
            ELLIPSIS
        } else {
            ELLIPSIS_ASCII
        }
    }

    /// Truncates a line to this viewport's width.
    pub fn fit(self, text: &str) -> String {
        fit_with(text, self.line_width(), self.ellipsis())
    }
}

/// What a row in a card is for.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RowKind {
    /// A group heading; never selectable.
    Header,
    /// An ordinary selectable row.
    Row,
    /// An explanation attached to the rows above it; never selectable.
    Note,
}

/// One line of a card.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Row {
    pub kind: RowKind,
    /// The primary text, shown in the first column.
    pub text: String,
    /// Secondary text, aligned into its own column across the card.
    pub detail: String,
    /// An editable proposal shown after the detail, such as the label a
    /// checkout will be registered under. Empty when the row proposes nothing.
    pub value: String,
    /// The proposal is what the reader would assume anyway — a label equal
    /// to the directory name — so it is shown only while being edited, and
    /// stays shown once it has been changed.
    pub implicit: bool,
    /// Trailing text, dimmed.
    pub note: String,
    pub selected: bool,
    pub enabled: bool,
    /// Opaque identity the caller uses to map a row back to its subject.
    pub id: String,
    /// The row this one depends on: a worktree can only be adopted under a
    /// checkout that is being registered, so it is enabled exactly while its
    /// parent is selected.
    pub parent: Option<String>,
}

/// Reduces arbitrary text to something that occupies exactly one line and
/// carries no escape sequence of its own.
///
/// Row text comes from the filesystem, where a path may legally contain a
/// newline or an ESC. Either would add a row the renderer never counted, or
/// repaint the screen from under it.
pub fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        if character == '\u{1b}' || character.is_control() {
            out.push('?');
        } else {
            out.push(character);
        }
    }
    out
}

impl Row {
    pub fn header(text: impl Into<String>) -> Self {
        Self::new(RowKind::Header, "", text.into(), "", false, false)
    }

    pub fn note(text: impl Into<String>) -> Self {
        Self::new(RowKind::Note, "", text.into(), "", false, false)
    }

    pub fn item(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self::new(RowKind::Row, id.into(), text.into(), "", false, true)
    }

    fn new(
        kind: RowKind,
        id: impl Into<String>,
        text: impl Into<String>,
        detail: impl Into<String>,
        selected: bool,
        enabled: bool,
    ) -> Self {
        Self {
            kind,
            text: plain(&text.into()),
            detail: plain(&detail.into()),
            value: String::new(),
            implicit: false,
            note: String::new(),
            selected,
            enabled,
            // The id addresses a subject and is never drawn, so it keeps
            // whatever the caller gave it.
            id: id.into(),
            parent: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = plain(&detail.into());
        self
    }

    pub fn with_value(mut self, value: impl Into<String>) -> Self {
        self.value = plain(&value.into());
        self
    }

    /// Marks the value as one the reader would assume, so the row does not
    /// repeat it.
    pub fn implicit(mut self) -> Self {
        self.implicit = true;
        self
    }

    /// Makes this row depend on the selection of `parent`.
    pub fn under(mut self, parent: impl Into<String>) -> Self {
        self.parent = Some(parent.into());
        self
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = plain(&note.into());
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }
}

/// How a card answers.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Any number of rows may be selected.
    Multi,
    /// Exactly one row is selected.
    Choice,
    /// A single editable line.
    Text,
}

/// One question, which collapses to a single line once answered.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Card {
    pub key: String,
    pub title: String,
    pub mode: Mode,
    pub rows: Vec<Row>,
    pub cursor: usize,
    /// The editable value, for `Mode::Text`.
    pub value: String,
    /// True while rows are still arriving from a background scan.
    pub pending: bool,
    /// Set when the card has nothing to ask and should be skipped.
    pub skipped: bool,
    /// Rows past this index are hidden until the tail is expanded.
    pub collapse_after: Option<usize>,
    pub expanded: bool,
    /// One line explaining what the card is for.
    pub blurb: String,
    /// A few words after the title: what was found, what is settled.
    pub status: String,
    /// What Enter does with the selection, as a verb: "register", "install".
    /// Empty when Enter merely continues.
    pub verb: String,
    /// The value the cursor row had before editing began, while an edit is
    /// open; restoring it is what cancelling means.
    pub editing: Option<String>,
}

impl Card {
    pub fn new(key: impl Into<String>, title: impl Into<String>, mode: Mode) -> Self {
        Self {
            key: key.into(),
            title: plain(&title.into()),
            mode,
            rows: Vec::new(),
            cursor: 0,
            value: String::new(),
            pending: false,
            skipped: false,
            collapse_after: None,
            expanded: false,
            blurb: String::new(),
            status: String::new(),
            verb: String::new(),
            editing: None,
        }
    }

    pub fn with_rows(mut self, rows: Vec<Row>) -> Self {
        self.rows = rows;
        self.settle_dependents();
        // A one-of-many card has exactly one answer, and the cursor is how the
        // reader is shown which. Opening on the first row instead would put the
        // highlight and the marked row on different lines, which reads as the
        // highlighted row being the answer. Only the opening position is set
        // here; once the reader moves, the cursor is theirs.
        if self.mode == Mode::Choice {
            let chosen = self
                .visible_rows()
                .iter()
                .position(|row| row.enabled && row.selected);
            if let Some(chosen) = chosen {
                self.cursor = chosen;
                return self;
            }
        }
        self.settle_cursor();
        self
    }

    pub fn with_value(mut self, value: impl Into<String>) -> Self {
        self.value = plain(&value.into());
        self
    }

    pub fn with_blurb(mut self, blurb: impl Into<String>) -> Self {
        self.blurb = plain(&blurb.into());
        self
    }

    pub fn with_status(mut self, status: impl Into<String>) -> Self {
        self.status = plain(&status.into());
        self
    }

    pub fn with_verb(mut self, verb: impl Into<String>) -> Self {
        self.verb = plain(&verb.into());
        self
    }

    fn first_selectable(&self) -> Option<usize> {
        // Within the visible rows: a cursor parked in a collapsed tail is out
        // of range for every movement and toggle until the tail is expanded.
        self.visible_rows().iter().position(|row| row.enabled)
    }

    /// Whether any row the reader can currently reach can be acted on.
    pub fn actionable(&self) -> bool {
        self.rows.iter().any(|row| row.enabled)
    }

    /// Puts the cursor back on a reachable row after the rows changed.
    pub fn settle_cursor(&mut self) {
        let len = self.visible_len();
        if len == 0 {
            self.cursor = 0;
            return;
        }
        if self.cursor < len && self.visible_rows()[self.cursor].enabled {
            return;
        }
        self.cursor = self.first_selectable().unwrap_or(0);
    }

    /// The rows currently on screen, which excludes a collapsed tail.
    pub fn visible_rows(&self) -> &[Row] {
        match self.collapse_after {
            Some(index) if !self.expanded => &self.rows[..index],
            _ => &self.rows[..],
        }
    }

    fn visible_len(&self) -> usize {
        self.visible_rows().len()
    }

    /// The ids of every selected row that can act. A dependent row keeps its
    /// mark while its parent is deselected, so the mark comes back when the
    /// parent does, but it is not part of the answer meanwhile.
    pub fn selection(&self) -> Vec<String> {
        self.rows
            .iter()
            .filter(|row| row.kind == RowKind::Row && row.selected && row.enabled)
            .map(|row| row.id.clone())
            .collect()
    }

    /// The edited value of a row, by id.
    pub fn value_of(&self, id: &str) -> Option<&str> {
        self.rows
            .iter()
            .find(|row| row.id == id)
            .map(|row| row.value.as_str())
    }

    /// Enables each dependent row exactly while its parent is selected.
    pub fn settle_dependents(&mut self) {
        let parents: Vec<(String, bool)> = self
            .rows
            .iter()
            .filter(|row| row.kind == RowKind::Row)
            .map(|row| (row.id.clone(), row.selected && row.enabled))
            .collect();
        for row in &mut self.rows {
            let Some(parent) = &row.parent else {
                continue;
            };
            row.enabled = parents
                .iter()
                .any(|(id, selected)| id == parent && *selected);
        }
    }

    /// Whether the cursor row carries a proposal that can be edited.
    fn editable(&self) -> bool {
        self.visible_rows()
            .get(self.cursor)
            .is_some_and(|row| row.enabled && !row.value.is_empty())
    }

    fn move_cursor(&mut self, delta: isize) {
        let len = self.visible_len();
        if len == 0 {
            return;
        }
        let mut index = self.cursor.min(len.saturating_sub(1));
        for _ in 0..len {
            let next = index as isize + delta;
            index = if next < 0 {
                len - 1
            } else if next as usize >= len {
                0
            } else {
                next as usize
            };
            if self.visible_rows()[index].enabled {
                self.cursor = index;
                return;
            }
        }
    }

    fn toggle(&mut self) {
        let len = self.visible_len();
        if self.cursor >= len {
            return;
        }
        if !self.rows[self.cursor].enabled {
            return;
        }
        match self.mode {
            Mode::Multi => {
                let selected = self.rows[self.cursor].selected;
                self.rows[self.cursor].selected = !selected;
                self.settle_dependents();
            }
            Mode::Choice => {
                for (index, row) in self.rows.iter_mut().enumerate() {
                    row.selected = index == self.cursor;
                }
            }
            Mode::Text => {}
        }
    }

    /// Ticks or unticks every row the reader could reach.
    pub fn set_all(&mut self, selected: bool) {
        if self.mode != Mode::Multi {
            return;
        }
        // Parents first, so a dependent row's enabled state is settled by the
        // time it is considered.
        for row in &mut self.rows {
            if row.kind == RowKind::Row && row.enabled && row.parent.is_none() {
                row.selected = selected;
            }
        }
        self.settle_dependents();
        for row in &mut self.rows {
            if row.kind == RowKind::Row && row.enabled && row.parent.is_some() {
                row.selected = selected;
            }
        }
    }

    /// The one line this card leaves in the transcript once answered.
    pub fn summary(&self) -> String {
        if self.skipped {
            return format!("{:<14} skipped", self.title);
        }
        let value = match self.mode {
            Mode::Text => self.value.clone(),
            Mode::Choice => self
                .rows
                .iter()
                .find(|row| row.selected)
                .map_or_else(|| "none".to_owned(), |row| row.text.clone()),
            Mode::Multi => {
                let chosen: Vec<&str> = self
                    .rows
                    .iter()
                    .filter(|row| row.kind == RowKind::Row && row.selected && row.enabled)
                    // The proposal is the name the reader will use from now
                    // on, so the receipt says `api`, not `~/src/api`.
                    .map(|row| {
                        if row.value.is_empty() {
                            row.text.trim()
                        } else {
                            row.value.as_str()
                        }
                    })
                    .collect();
                let total = self
                    .rows
                    .iter()
                    .filter(|row| row.kind == RowKind::Row)
                    .count();
                let named = chosen.join(", ");
                // Naming what was chosen reads better than counting it, but
                // only while the names still fit on the summary line.
                if chosen.is_empty() {
                    "none".to_owned()
                } else if display_width(&named) <= 40 {
                    named
                } else {
                    format!("{} of {total}", chosen.len())
                }
            }
        };
        format!("{:<14} {value}", self.title)
    }
}

/// The whole interface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct State {
    pub cards: Vec<Card>,
    pub active: usize,
    /// Cards before this index have been printed into the transcript.
    pub committed: usize,
    pub quit: bool,
    pub accepted: bool,
    pub frame: u64,
    /// Live progress text while a background scan runs.
    pub scan: Option<String>,
}

/// What the caller should do after an update.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    /// Keep reading keys.
    Continue,
    /// Every card is answered and the plan was accepted.
    Accepted,
    /// The user left; nothing has been applied.
    Quit,
}

impl State {
    pub fn new(cards: Vec<Card>) -> Self {
        let mut state = Self {
            cards,
            active: 0,
            committed: 0,
            quit: false,
            accepted: false,
            frame: 0,
            scan: None,
        };
        state.skip_forward();
        state
    }

    /// Moves past every card that has nothing to ask. A card can become
    /// skippable while it is active — a scan that finds nothing does that —
    /// so the caller runs this after changing a card as well.
    pub fn skip_forward(&mut self) {
        while self.active < self.cards.len() && self.cards[self.active].skipped {
            self.active += 1;
        }
        if self.active >= self.cards.len() && !self.quit {
            self.accepted = true;
        }
    }

    pub fn card(&self) -> Option<&Card> {
        self.cards.get(self.active)
    }

    /// The card whose answer identifies `key`, whether or not it is active.
    pub fn answer(&self, key: &str) -> Option<&Card> {
        self.cards.iter().find(|card| card.key == key)
    }

    pub fn finished(&self) -> bool {
        self.quit || self.active >= self.cards.len()
    }

    /// Summaries not yet printed into the transcript, and advances the mark.
    pub fn take_committed(&mut self) -> Vec<String> {
        let upto = self.active.min(self.cards.len());
        let mut lines = Vec::new();
        while self.committed < upto {
            let card = &self.cards[self.committed];
            if !card.skipped {
                lines.push(card.summary());
            }
            self.committed += 1;
        }
        lines
    }

    /// Applies one keypress.
    pub fn update(&mut self, key: Key) -> Outcome {
        if self.finished() {
            return self.outcome();
        }
        if let Key::Interrupt = key {
            self.quit = true;
            return Outcome::Quit;
        }
        let card = &mut self.cards[self.active];
        // An open edit owns the keyboard: every printable character is text,
        // and only enter and escape leave.
        if let Some(original) = card.editing.clone() {
            let cursor = card.cursor;
            match key {
                Key::Enter => {
                    if card.rows[cursor].value.trim().is_empty() {
                        card.rows[cursor].value = original;
                    } else if card.rows[cursor].value != original {
                        card.rows[cursor].implicit = false;
                    }
                    card.editing = None;
                }
                Key::Escape => {
                    card.rows[cursor].value = original;
                    card.editing = None;
                }
                Key::Backspace => {
                    card.rows[cursor].value.pop();
                }
                Key::Char(character) => card.rows[cursor].value.push(character),
                Key::Space => card.rows[cursor].value.push(' '),
                _ => {}
            }
            return Outcome::Continue;
        }
        match key {
            Key::Up => card.move_cursor(-1),
            Key::Down => card.move_cursor(1),
            Key::Enter => {
                // A card still waiting on the scan cannot be answered yet.
                if card.pending {
                    return Outcome::Continue;
                }
                self.active += 1;
                self.skip_forward();
                if self.active >= self.cards.len() {
                    return Outcome::Accepted;
                }
            }
            Key::Backspace if card.mode == Mode::Text => {
                card.value.pop();
            }
            Key::Char(character) if card.mode == Mode::Text => card.value.push(character),
            Key::Space if card.mode == Mode::Text => card.value.push(' '),
            Key::Space => card.toggle(),
            Key::Char('a') => card.set_all(true),
            Key::Char('n') => card.set_all(false),
            Key::Char('e') if card.editable() => {
                card.editing = Some(card.rows[card.cursor].value.clone());
            }
            Key::Char('t') => {
                if card.collapse_after.is_some() {
                    card.expanded = !card.expanded;
                    // Collapsing can strand the cursor in the hidden tail.
                    card.settle_cursor();
                }
            }
            Key::Char('q') => {
                self.quit = true;
                return Outcome::Quit;
            }
            // Escape is not a way out: a reader who reaches for it to cancel
            // a keystroke would lose every answer given so far.
            Key::Escape | Key::Char(_) | Key::Backspace | Key::Interrupt => {}
        }
        Outcome::Continue
    }

    fn outcome(&self) -> Outcome {
        if self.quit {
            Outcome::Quit
        } else if self.accepted {
            Outcome::Accepted
        } else {
            Outcome::Continue
        }
    }

    /// Renders the live region.
    ///
    /// The result never exceeds `viewport.region_rows()` lines, and no line
    /// exceeds `viewport.line_width()` display columns.
    pub fn view(&self, viewport: Viewport) -> Vec<String> {
        let Some(card) = self.card() else {
            return Vec::new();
        };
        let mut lines = Vec::new();
        let marker = if viewport.unicode { "›" } else { ">" };
        let mut title = paint(
            &format!("{marker} {}", card.title),
            Paint::Accent,
            viewport.color,
        );
        if !card.status.is_empty() {
            title.push_str(&paint(
                &format!("   {}", card.status),
                Paint::Dim,
                viewport.color,
            ));
        }
        lines.push(viewport.fit(&title));
        if !card.blurb.is_empty() {
            lines.push(viewport.fit(&paint(
                &format!("  {}", card.blurb),
                Paint::Dim,
                viewport.color,
            )));
        }
        if let Some(scan) = &self.scan {
            let spin = self.spinner(viewport.unicode);
            lines.push(viewport.fit(&paint(
                &format!("  {spin} {scan}"),
                Paint::Dim,
                viewport.color,
            )));
        }

        match card.mode {
            Mode::Text => lines.push(viewport.fit(&format!(
                "  {}{}",
                card.value,
                if self.frame % 2 == 0 { "_" } else { " " }
            ))),
            Mode::Multi | Mode::Choice => {
                lines.extend(self.rows_view(card, viewport, lines.len()));
            }
        }
        lines.push(viewport.fit(&paint(
            &format!("  {}", self.keys(card, viewport.unicode)),
            Paint::Dim,
            viewport.color,
        )));
        lines.truncate(viewport.region_rows());
        lines
    }

    fn rows_view(&self, card: &Card, viewport: Viewport, used: usize) -> Vec<String> {
        let rows = card.visible_rows();
        // One line is reserved for the key hints below the list.
        let budget = viewport.region_rows().saturating_sub(used + 1).max(1);
        let (start, end) = window(card.cursor, rows.len(), budget);
        let items = rows[start..end]
            .iter()
            .filter(|row| row.kind == RowKind::Row);
        let widest_text = items
            .clone()
            .map(|row| display_width(&row.text))
            .max()
            .unwrap_or(0);
        // The proposal column is what the reader is deciding about, so it is
        // what must stay on screen: the text column gives way first, and a
        // path that does not fit loses its middle rather than its end.
        let widest_trailing = items.map(trailing_width).max().unwrap_or(0);
        let text_width = widest_text
            .min(
                viewport
                    .line_width()
                    .saturating_sub(ROW_PREFIX + widest_trailing),
            )
            .max(MIN_TEXT_WIDTH.min(widest_text));
        let more = viewport.ellipsis();
        let mut lines = Vec::new();
        if start > 0 {
            lines.push(viewport.fit(&paint(&format!("  {more}"), Paint::Dim, viewport.color)));
        }
        for (offset, row) in rows[start..end].iter().enumerate() {
            let index = start + offset;
            lines.push(self.row_view(card, row, index == card.cursor, text_width, viewport));
        }
        if end < rows.len() {
            lines.push(viewport.fit(&paint(&format!("  {more}"), Paint::Dim, viewport.color)));
        }
        if let Some(hidden) = card.collapse_after {
            if !card.expanded && hidden < card.rows.len() {
                let count = card.rows.len() - hidden;
                lines.push(viewport.fit(&paint(
                    &format!("  {more} {count} more, not touched recently   [t] show"),
                    Paint::Dim,
                    viewport.color,
                )));
            }
        }
        lines
    }

    fn row_view(
        &self,
        card: &Card,
        row: &Row,
        active: bool,
        text_width: usize,
        viewport: Viewport,
    ) -> String {
        match row.kind {
            RowKind::Header => viewport.fit(&paint(
                &format!("  {}", row.text),
                Paint::Bold,
                viewport.color,
            )),
            RowKind::Note => viewport.fit(&paint(
                &format!("    {}", row.text),
                Paint::Dim,
                viewport.color,
            )),
            RowKind::Row => {
                let cursor = if active {
                    if viewport.unicode {
                        "▸"
                    } else {
                        ">"
                    }
                } else {
                    " "
                };
                let box_glyph = match (card.mode, row.selected, row.enabled) {
                    (_, _, false) => "  ".to_owned(),
                    (Mode::Choice, true, _) => {
                        if viewport.unicode { "◉ " } else { "(*)" }.to_owned()
                    }
                    (Mode::Choice, false, _) => {
                        if viewport.unicode { "○ " } else { "( )" }.to_owned()
                    }
                    (_, true, _) => if viewport.unicode { "✓ " } else { "[x]" }.to_owned(),
                    (_, false, _) => if viewport.unicode { "· " } else { "[ ]" }.to_owned(),
                };
                // Column widths are computed once across the window, so the
                // detail column lines up rather than stepping with the text.
                let text = pad(
                    &truncate_middle_with(&row.text, text_width, viewport.ellipsis()),
                    text_width,
                );
                let mut line = format!(" {cursor} {box_glyph}{text}");
                let editing = active && card.editing.is_some();
                if editing {
                    line.push_str(&format!(
                        "  {} {}{}",
                        row.detail,
                        row.value,
                        if self.frame % 2 == 0 { "_" } else { " " }
                    ));
                } else if !row.value.is_empty() && !row.implicit {
                    line.push_str(&format!("  {} {}", row.detail, row.value));
                } else if !row.detail.is_empty() && row.value.is_empty() {
                    line.push_str(&format!("  {}", row.detail));
                }
                let mut line = if row.enabled {
                    line
                } else {
                    paint(&line, Paint::Dim, viewport.color)
                };
                let note = if row.note.is_empty() && row.parent.is_some() && !row.enabled {
                    "its checkout is not selected"
                } else {
                    row.note.as_str()
                };
                if !note.is_empty() {
                    line.push_str(&paint(&format!("  {note}"), Paint::Dim, viewport.color));
                }
                if active && row.enabled {
                    line = paint(&line, Paint::Accent, viewport.color);
                }
                viewport.fit(&line)
            }
        }
    }

    fn keys(&self, card: &Card, unicode: bool) -> String {
        let separator = if unicode { " · " } else { " | " };
        if card.editing.is_some() {
            return ["type to edit", "enter done", "esc cancel"].join(separator);
        }
        // The action comes first: the hint line is the call to action, and
        // it says what Enter will do with what is ticked.
        let action = match (card.mode, card.verb.is_empty()) {
            (Mode::Multi, false) => {
                let count = card.selection().len();
                if count == 0 {
                    "enter skip".to_owned()
                } else {
                    format!("enter {} {count}", card.verb)
                }
            }
            (Mode::Choice, false) => format!("enter {}", card.verb),
            _ => "enter continue".to_owned(),
        };
        let mut parts = vec![action];
        match card.mode {
            Mode::Multi => parts.push("space tick".to_owned()),
            Mode::Choice => parts.push("space choose".to_owned()),
            Mode::Text => parts.push("type to edit".to_owned()),
        }
        if card.editable() {
            parts.push("e rename".to_owned());
        }
        // The collapsed tail's own line already says `[t] show`.
        parts.push("q quit".to_owned());
        parts.join(separator)
    }

    fn spinner(&self, unicode: bool) -> &'static str {
        if unicode {
            SPINNER[(self.frame as usize) % SPINNER.len()]
        } else {
            SPINNER_ASCII[(self.frame as usize) % SPINNER_ASCII.len()]
        }
    }
}

/// Columns taken by the cursor and the box before a row's text.
const ROW_PREFIX: usize = 5;
/// The least a text column shrinks to before the trailing columns overflow
/// instead: below this a path is unrecognisable anyway.
const MIN_TEXT_WIDTH: usize = 16;

/// Columns a row needs after its text: the detail, the proposal and the note,
/// with the gaps that separate them.
fn trailing_width(row: &Row) -> usize {
    let mut width = 0;
    let proposal_shown = !row.value.is_empty() && !row.implicit;
    if !row.detail.is_empty() && (proposal_shown || row.value.is_empty()) {
        width += 2 + display_width(&row.detail);
    }
    if proposal_shown {
        width += 1 + display_width(&row.value);
    }
    if !row.note.is_empty() {
        width += 2 + display_width(&row.note);
    }
    width
}

/// Chooses the slice of a list to show so the cursor stays inside it.
fn window(cursor: usize, len: usize, budget: usize) -> (usize, usize) {
    if len <= budget {
        return (0, len);
    }
    let half = budget / 2;
    let start = cursor.saturating_sub(half).min(len - budget);
    (start, start + budget)
}

#[derive(Clone, Copy)]
enum Paint {
    Accent,
    Dim,
    Bold,
}

/// Wraps text in one of the sixteen ANSI colours, which are the reader's own
/// palette rather than a guess at their theme.
fn paint(text: &str, paint: Paint, color: bool) -> String {
    if !color {
        return text.to_owned();
    }
    let code = match paint {
        Paint::Accent => "36",
        Paint::Dim => "2",
        Paint::Bold => "1",
    };
    format!("\u{1b}[{code}m{text}\u{1b}[0m")
}

/// The columns a string occupies, counting escape sequences as nothing and
/// wide characters as two.
pub fn display_width(text: &str) -> usize {
    let mut width = 0;
    let mut chars = text.chars();
    while let Some(character) = chars.next() {
        if character == '\u{1b}' {
            for escape in chars.by_ref() {
                if escape.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        width += char_width(character);
    }
    width
}

fn char_width(character: char) -> usize {
    let code = character as u32;
    // Combining marks and zero-width joiners occupy no column.
    if matches!(code, 0x0300..=0x036F | 0x200B..=0x200F | 0xFE00..=0xFE0F) {
        return 0;
    }
    if character.is_control() {
        return 0;
    }
    // The East Asian Wide and Fullwidth blocks, plus emoji, take two columns.
    if matches!(code,
        0x1100..=0x115F
        | 0x2E80..=0x303E
        | 0x3041..=0x33FF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xA000..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE6F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1F64F
        | 0x1F900..=0x1F9FF
        | 0x20000..=0x3FFFD
    ) {
        return 2;
    }
    1
}

/// Truncates to `max` display columns, closing any open escape sequence.
///
/// Every line the region draws passes through this: a line that wraps makes
/// the region taller than the renderer believes it is, and every rewind after
/// that is off by one.
pub fn fit(text: &str, max: usize) -> String {
    fit_with(text, max, ELLIPSIS)
}

/// The Unicode and ASCII spellings of "there is more here".
pub const ELLIPSIS: &str = "…";
pub const ELLIPSIS_ASCII: &str = "..";

/// [`fit`], with the marker chosen for what the terminal can render.
pub fn fit_with(text: &str, max: usize, ellipsis: &str) -> String {
    if display_width(text) <= max {
        return text.to_owned();
    }
    let marker = display_width(ellipsis);
    let budget = max.saturating_sub(marker);
    let mut out = String::new();
    let mut width = 0;
    let mut chars = text.chars();
    while let Some(character) = chars.next() {
        if character == '\u{1b}' {
            out.push(character);
            for escape in chars.by_ref() {
                out.push(escape);
                if escape.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        let next = char_width(character);
        if width + next > budget {
            break;
        }
        out.push(character);
        width += next;
    }
    out.push_str(ellipsis);
    // A cut may have landed between an SGR and its reset. A redundant reset
    // costs nothing; a missing one paints the rest of the screen.
    if text.contains('\u{1b}') {
        out.push_str("\u{1b}[0m");
    }
    out
}

/// Shortens text from the middle, which for a path keeps both the root and
/// the leaf — the two parts that identify it.
pub fn truncate_middle_with(text: &str, max: usize, ellipsis: &str) -> String {
    let marker = display_width(ellipsis);
    if display_width(text) <= max || max < marker + 4 {
        return text.to_owned();
    }
    let chars: Vec<char> = text.chars().collect();
    let keep = max - marker;
    let head = keep / 2;
    let tail = keep - head;
    let mut out: String = chars[..head.min(chars.len())].iter().collect();
    out.push_str(ellipsis);
    out.extend(chars[chars.len().saturating_sub(tail)..].iter());
    out
}

/// Pads to a column width, so a table's columns align.
fn pad(text: &str, width: usize) -> String {
    let current = display_width(text);
    if current >= width {
        return text.to_owned();
    }
    format!("{text}{}", " ".repeat(width - current))
}

#[cfg(test)]
impl Card {
    fn collapsing_after(mut self, index: usize) -> Self {
        self.collapse_after = Some(index);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card() -> Card {
        Card::new("repos", "repos", Mode::Multi).with_rows(vec![
            Row::header("github.com/acme/api"),
            Row::item("a", "~/src/api").with_detail("register as api"),
            Row::item("b", "~/src/api-old").with_detail("register as acme-api"),
        ])
    }

    fn viewport() -> Viewport {
        Viewport {
            rows: 24,
            cols: 80,
            color: false,
            unicode: true,
        }
    }

    #[test]
    fn the_cursor_starts_on_the_first_selectable_row() {
        assert_eq!(card().cursor, 1, "a header is never selectable");
    }

    #[test]
    fn the_cursor_skips_headers_and_wraps() {
        let mut card = card();
        card.move_cursor(1);
        assert_eq!(card.cursor, 2);
        card.move_cursor(1);
        assert_eq!(card.cursor, 1, "wraps past the header");
        card.move_cursor(-1);
        assert_eq!(card.cursor, 2);
    }

    #[test]
    fn choice_cards_hold_exactly_one_selection() {
        let mut card = Card::new("agent", "agent", Mode::Choice).with_rows(vec![
            Row::item("claude", "claude"),
            Row::item("codex", "codex"),
        ]);
        card.toggle();
        assert_eq!(card.selection(), vec!["claude".to_owned()]);
        card.move_cursor(1);
        card.toggle();
        assert_eq!(card.selection(), vec!["codex".to_owned()]);
    }

    #[test]
    fn a_choice_card_opens_with_the_cursor_on_its_answer() {
        let card = Card::new("agent", "agent", Mode::Choice).with_rows(vec![
            Row::item("claude", "claude"),
            Row::item("codex", "codex"),
            Row::item("none", "none").selected(true),
        ]);
        assert_eq!(card.cursor, 2, "the highlight starts on the marked row");

        let unanswered = Card::new("agent", "agent", Mode::Choice).with_rows(vec![
            Row::item("claude", "claude"),
            Row::item("codex", "codex"),
        ]);
        assert_eq!(unanswered.cursor, 0, "with no answer it starts at the top");
    }

    #[test]
    fn select_all_leaves_disabled_rows_alone() {
        let mut card = Card::new("k", "k", Mode::Multi)
            .with_rows(vec![Row::item("a", "a"), Row::item("b", "b").disabled()]);
        card.set_all(true);
        assert_eq!(card.selection(), vec!["a".to_owned()]);
    }

    #[test]
    fn quitting_reports_quit_and_mutates_nothing() {
        let mut state = State::new(vec![card()]);
        assert_eq!(state.update(Key::Interrupt), Outcome::Quit);
        assert!(state.quit);
        assert!(!state.accepted);
    }

    #[test]
    fn answering_every_card_accepts() {
        let mut state = State::new(vec![card(), card()]);
        assert_eq!(state.update(Key::Enter), Outcome::Continue);
        assert_eq!(state.update(Key::Enter), Outcome::Accepted);
        assert!(state.finished());
    }

    #[test]
    fn a_pending_card_cannot_be_answered() {
        let mut pending = card();
        pending.pending = true;
        let mut state = State::new(vec![pending]);
        assert_eq!(state.update(Key::Enter), Outcome::Continue);
        assert_eq!(state.active, 0);
    }

    #[test]
    fn skipped_cards_are_stepped_over_and_leave_no_summary() {
        let mut skipped = Card::new("agent", "agent", Mode::Choice);
        skipped.skipped = true;
        let mut state = State::new(vec![skipped, card()]);
        assert_eq!(state.active, 1, "a leading skipped card is stepped over");
        state.update(Key::Enter);
        assert_eq!(
            state.take_committed(),
            vec!["repos          none".to_owned()]
        );
    }

    #[test]
    fn a_long_selection_is_counted_rather_than_listed() {
        let rows = (0..9)
            .map(|index| {
                Row::item(index.to_string(), format!("/very/long/path/number-{index}"))
                    .selected(true)
            })
            .collect();
        let card = Card::new("repos", "repos", Mode::Multi).with_rows(rows);
        assert_eq!(card.summary(), "repos          9 of 9");
    }

    #[test]
    fn summaries_commit_once_each() {
        let mut state = State::new(vec![card(), card()]);
        assert!(state.take_committed().is_empty());
        state.update(Key::Space);
        state.update(Key::Enter);
        assert_eq!(
            state.take_committed(),
            vec!["repos          ~/src/api".to_owned()],
            "a short selection is named rather than counted"
        );
        assert!(state.take_committed().is_empty(), "committed only once");
    }

    #[test]
    fn no_rendered_line_exceeds_the_viewport_width() {
        let viewport = Viewport {
            rows: 24,
            cols: 40,
            color: true,
            unicode: true,
        };
        let mut card = card();
        card.rows.push(
            Row::item("c", "~/src/a-very-long-repository-name-that-will-not-fit")
                .with_detail("register as a-very-long-label"),
        );
        let state = State::new(vec![card]);
        for line in state.view(viewport) {
            assert!(
                display_width(&line) <= viewport.line_width(),
                "line overflows: {line:?} ({})",
                display_width(&line)
            );
        }
    }

    #[test]
    fn the_region_never_exceeds_its_row_budget() {
        let viewport = Viewport {
            rows: 8,
            cols: 80,
            color: false,
            unicode: true,
        };
        let rows = (0..50)
            .map(|index| Row::item(index.to_string(), format!("row {index}")))
            .collect();
        let state = State::new(vec![Card::new("k", "many", Mode::Multi).with_rows(rows)]);
        assert!(state.view(viewport).len() <= viewport.region_rows());
    }

    #[test]
    fn a_tiny_viewport_still_renders_something() {
        let viewport = Viewport {
            rows: 1,
            cols: 20,
            color: false,
            unicode: true,
        };
        let state = State::new(vec![card()]);
        let lines = state.view(viewport);
        assert!(!lines.is_empty());
        assert!(lines.len() <= viewport.region_rows());
    }

    #[test]
    fn the_scrolling_window_keeps_the_cursor_inside() {
        assert_eq!(window(0, 50, 10), (0, 10));
        assert_eq!(window(49, 50, 10), (40, 50));
        assert_eq!(window(25, 50, 10), (20, 30));
        assert_eq!(window(3, 5, 10), (0, 5), "a short list is not windowed");
    }

    #[test]
    fn a_collapsed_tail_hides_rows_until_expanded() {
        let rows = (0..10)
            .map(|index| Row::item(index.to_string(), format!("row {index}")))
            .collect();
        let mut card = Card::new("k", "repos", Mode::Multi)
            .with_rows(rows)
            .collapsing_after(3);
        assert_eq!(card.visible_rows().len(), 3);
        card.expanded = true;
        assert_eq!(card.visible_rows().len(), 10);
    }

    #[test]
    fn expanding_the_tail_is_a_keystroke() {
        let rows = (0..10)
            .map(|index| Row::item(index.to_string(), format!("row {index}")))
            .collect();
        let card = Card::new("k", "repos", Mode::Multi)
            .with_rows(rows)
            .collapsing_after(3);
        let mut state = State::new(vec![card]);
        state.update(Key::Char('t'));
        assert!(state.cards[0].expanded);
    }

    #[test]
    fn text_cards_edit_their_value() {
        let card = Card::new("trees", "trees", Mode::Text).with_value("/a");
        let mut state = State::new(vec![card]);
        state.update(Key::Char('b'));
        assert_eq!(state.cards[0].value, "/ab");
        state.update(Key::Backspace);
        assert_eq!(state.cards[0].value, "/a");
    }

    #[test]
    fn a_terminal_smaller_than_the_region_is_not_overdrawn() {
        for rows in 1..6u16 {
            for cols in [1u16, 4, 12, 80] {
                let viewport = Viewport {
                    rows,
                    cols,
                    color: true,
                    unicode: true,
                };
                let state = State::new(vec![card()]);
                let lines = state.view(viewport);
                assert!(
                    lines.len() <= usize::from(rows),
                    "{rows}x{cols}: {} lines drawn into {rows} rows",
                    lines.len()
                );
                for line in &lines {
                    assert!(
                        display_width(line) < usize::from(cols).max(1) + 1,
                        "{rows}x{cols}: line wider than the terminal: {line:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_path_cannot_smuggle_a_newline_or_an_escape_into_the_region() {
        // Both are legal in a Unix path; a newline adds a row the renderer
        // never counted, and an escape repaints the screen from under it.
        let hostile = Row::item("id", "/src/a\nb\u{1b}[2Jc").with_detail("x\u{1b}[31my");
        assert!(!hostile.text.contains('\n'));
        assert!(!hostile.text.contains('\u{1b}'));
        assert!(!hostile.detail.contains('\u{1b}'));

        let state = State::new(vec![
            Card::new("k", "k", Mode::Multi).with_rows(vec![hostile])
        ]);
        let lines = state.view(viewport());
        assert!(lines.iter().all(|line| !line.contains('\n')));
        // The only escapes left are the ones this renderer wrote itself.
        let painted = lines.join("");
        assert!(!painted.contains("\u{1b}[2J"));
    }

    #[test]
    fn a_card_with_no_reachable_row_says_so() {
        let card = Card::new("k", "k", Mode::Multi)
            .with_rows(vec![Row::item("a", "a").disabled(), Row::header("h")]);
        assert!(!card.actionable());
        assert_eq!(card.cursor, 0);
    }

    #[test]
    fn collapsing_a_tail_brings_the_cursor_back_into_view() {
        let rows = (0..10)
            .map(|index| Row::item(index.to_string(), format!("row {index}")))
            .collect();
        let card = Card::new("k", "repos", Mode::Multi)
            .with_rows(rows)
            .collapsing_after(3);
        let mut state = State::new(vec![card]);
        state.update(Key::Char('t'));
        for _ in 0..8 {
            state.update(Key::Down);
        }
        assert!(state.cards[0].cursor >= 3, "cursor moved into the tail");
        state.update(Key::Char('t'));
        assert!(
            state.cards[0].cursor < 3,
            "a collapsed tail must not keep the cursor"
        );
        // The toggle must still land on a row rather than panicking.
        state.update(Key::Space);
        assert_eq!(state.cards[0].selection().len(), 1);
    }

    #[test]
    fn display_width_ignores_colour_and_counts_wide_characters() {
        assert_eq!(display_width("abc"), 3);
        assert_eq!(display_width("\u{1b}[36mabc\u{1b}[0m"), 3);
        assert_eq!(display_width("日本"), 4);
        assert_eq!(display_width("e\u{301}"), 1);
    }

    #[test]
    fn fit_never_exceeds_its_budget_for_wide_text() {
        let text = "日本語のとても長いテキスト";
        let fitted = fit(text, 10);
        assert!(display_width(&fitted) <= 10, "{fitted:?}");
    }

    #[test]
    fn fit_closes_a_colour_it_cut_through() {
        let fitted = fit("\u{1b}[36mabcdefghij\u{1b}[0m", 6);
        assert!(fitted.ends_with("\u{1b}[0m"), "{fitted:?}");
        assert!(display_width(&fitted) <= 6);
    }

    #[test]
    fn a_long_path_gives_way_to_the_proposal_column() {
        let viewport = Viewport {
            rows: 24,
            cols: 60,
            color: false,
            unicode: true,
        };
        let card = Card::new("repos", "repos", Mode::Multi).with_rows(vec![
            Row::item("a", "~/source/clients/acme/services/billing/api-gateway")
                .with_detail("as")
                .with_value("api-gateway"),
            Row::item("b", "~/src/tiny")
                .with_detail("as")
                .with_value("tiny"),
        ]);
        let state = State::new(vec![card]);
        let lines = state.view(viewport);
        let row = lines
            .iter()
            .find(|line| line.contains("api-gateway"))
            .unwrap();
        assert!(
            row.contains("as api-gateway"),
            "the proposal stays on screen: {row:?}"
        );
        assert!(
            row.contains("~/sou") && row.contains("…"),
            "the path lost its middle: {row:?}"
        );
        assert!(display_width(row) <= viewport.line_width());
        let short = lines.iter().find(|line| line.contains("tiny")).unwrap();
        let column = |line: &str| display_width(&line[..line.find("as ").unwrap()]);
        assert_eq!(column(short), column(row), "the columns still align");
    }

    #[test]
    fn truncate_middle_keeps_both_ends() {
        let path = "/Users/me/source/deep/nested/project";
        let short = truncate_middle_with(path, 20, ELLIPSIS);
        assert!(display_width(&short) <= 20);
        assert!(short.starts_with("/Users"));
        assert!(short.ends_with("project"));
    }

    #[test]
    fn columns_align_across_a_card() {
        let state = State::new(vec![card()]);
        let lines = state.view(viewport());
        // Compared in display columns: the cursor glyph is three bytes wide
        // and one column wide, so byte offsets would disagree by design.
        let detail_columns: Vec<_> = lines
            .iter()
            .filter(|line| line.contains("register as"))
            .map(|line| display_width(&line[..line.find("register as").unwrap()]))
            .collect();
        assert_eq!(detail_columns.len(), 2);
        assert_eq!(
            detail_columns[0], detail_columns[1],
            "detail column steps with the text"
        );
    }

    #[test]
    fn colour_is_absent_when_the_viewport_refuses_it() {
        let plain = Viewport {
            color: false,
            ..viewport()
        };
        assert!(state_view_joined(plain).find('\u{1b}').is_none());
        let painted = Viewport {
            color: true,
            ..viewport()
        };
        assert!(state_view_joined(painted).contains('\u{1b}'));
    }

    fn state_view_joined(viewport: Viewport) -> String {
        State::new(vec![card()]).view(viewport).join("\n")
    }

    #[test]
    fn ascii_fallback_avoids_every_non_ascii_glyph() {
        let viewport = Viewport {
            unicode: false,
            ..viewport()
        };
        let mut state = State::new(vec![card()]);
        state.scan = Some("scanning".to_owned());
        let rendered = state.view(viewport).join("\n");
        assert!(
            rendered.is_ascii(),
            "non-ascii glyph in ascii fallback: {rendered:?}"
        );
    }

    #[test]
    fn a_dependent_row_follows_its_parent() {
        let mut card = Card::new("repos", "repos", Mode::Multi).with_rows(vec![
            Row::item("/src/api", "/src/api")
                .selected(true)
                .with_value("api"),
            Row::item("/t/one", "  /t/one")
                .under("/src/api")
                .selected(true)
                .with_value("one"),
        ]);
        assert_eq!(
            card.selection(),
            vec!["/src/api".to_owned(), "/t/one".to_owned()],
            "a worktree of a selected checkout is offered"
        );
        card.toggle();
        assert!(
            !card.rows[1].enabled,
            "deselecting the checkout disables its worktree"
        );
        assert_eq!(card.selection(), Vec::<String>::new());
        card.toggle();
        assert_eq!(
            card.selection(),
            vec!["/src/api".to_owned(), "/t/one".to_owned()],
            "the worktree's own mark survives its parent's round trip"
        );
        card.set_all(false);
        assert!(card.selection().is_empty());
        card.set_all(true);
        assert_eq!(
            card.selection().len(),
            2,
            "select-all reaches dependents too"
        );
    }

    #[test]
    fn a_disabled_dependent_says_why() {
        let card = Card::new("repos", "repos", Mode::Multi).with_rows(vec![
            Row::item("/src/api", "/src/api").with_value("api"),
            Row::item("/t/one", "  /t/one")
                .under("/src/api")
                .with_value("one"),
        ]);
        let state = State::new(vec![card]);
        let rendered = state.view(viewport()).join("\n");
        assert!(
            rendered.contains("its checkout is not selected"),
            "{rendered}"
        );
    }

    #[test]
    fn a_proposal_is_edited_in_place_and_escape_restores_it() {
        let card = Card::new("repos", "repos", Mode::Multi)
            .with_rows(vec![Row::item("/src/api", "/src/api").with_value("api-2")]);
        let mut state = State::new(vec![card]);
        state.update(Key::Char('e'));
        assert!(state.cards[0].editing.is_some());
        for _ in 0..5 {
            state.update(Key::Backspace);
        }
        for character in "acme".chars() {
            state.update(Key::Char(character));
        }
        // Keys that would otherwise act on the card are text while editing.
        state.update(Key::Char('q'));
        assert!(!state.quit, "q is a letter while an edit is open");
        state.update(Key::Enter);
        assert_eq!(state.cards[0].editing, None);
        assert_eq!(state.cards[0].value_of("/src/api"), Some("acmeq"));
        assert_eq!(
            state.active, 0,
            "finishing an edit does not answer the card"
        );

        state.update(Key::Char('e'));
        state.update(Key::Char('x'));
        state.update(Key::Escape);
        assert_eq!(state.cards[0].value_of("/src/api"), Some("acmeq"));

        state.update(Key::Char('e'));
        for _ in 0..10 {
            state.update(Key::Backspace);
        }
        state.update(Key::Enter);
        assert_eq!(
            state.cards[0].value_of("/src/api"),
            Some("acmeq"),
            "an emptied proposal comes back rather than registering nothing"
        );
    }

    #[test]
    fn an_implicit_proposal_is_hidden_until_it_changes() {
        let card = Card::new("repos", "repositories", Mode::Multi)
            .with_verb("register")
            .with_rows(vec![
                Row::item("/src/api", "~/src/api")
                    .with_detail("as")
                    .with_value("api")
                    .implicit()
                    .selected(true),
                Row::item("/oss/api", "~/oss/api")
                    .with_detail("as")
                    .with_value("acme-api")
                    .selected(true),
            ]);
        let mut state = State::new(vec![card]);
        let rendered = state.view(viewport()).join("\n");
        assert!(
            !rendered.contains("as api\n") && !rendered.contains("as api "),
            "{rendered}"
        );
        assert!(rendered.contains("as acme-api"), "{rendered}");
        assert!(rendered.contains("enter register 2"), "{rendered}");

        // Editing reveals it, and a change keeps it shown.
        state.update(Key::Char('e'));
        assert!(state.view(viewport()).join("\n").contains("as api"));
        state.update(Key::Char('x'));
        state.update(Key::Enter);
        let rendered = state.view(viewport()).join("\n");
        assert!(rendered.contains("as apix"), "{rendered}");
        // Unticking everything turns the action into a skip.
        state.update(Key::Char('n'));
        assert!(state.view(viewport()).join("\n").contains("enter skip"));
    }

    #[test]
    fn escape_outside_an_edit_does_nothing() {
        let mut state = State::new(vec![card()]);
        assert_eq!(state.update(Key::Escape), Outcome::Continue);
        assert!(!state.quit);
    }

    #[test]
    fn a_card_that_becomes_skippable_while_active_is_stepped_over() {
        let mut state = State::new(vec![card(), card()]);
        state.cards[0].skipped = true;
        state.skip_forward();
        assert_eq!(state.active, 1);
        state.cards[1].skipped = true;
        state.skip_forward();
        assert!(state.finished());
        assert!(state.accepted, "nothing left to ask is an accepted run");
    }

    #[test]
    fn a_text_card_accepts_a_space() {
        let card = Card::new("trees", "trees", Mode::Text).with_value("/a");
        let mut state = State::new(vec![card]);
        state.update(Key::Space);
        assert_eq!(state.cards[0].value, "/a ");
    }
}
