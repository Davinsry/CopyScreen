#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RectF {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl RectF {
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width: width.max(0.0),
            height: height.max(0.0),
        }
    }

    pub fn from_points(x1: f32, y1: f32, x2: f32, y2: f32) -> Self {
        let min_x = x1.min(x2);
        let min_y = y1.min(y2);
        let max_x = x1.max(x2);
        let max_y = y1.max(y2);
        Self::new(min_x, min_y, max_x - min_x, max_y - min_y)
    }

    pub fn right(&self) -> f32 {
        self.x + self.width
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.height
    }

    pub fn center_x(&self) -> f32 {
        self.x + self.width * 0.5
    }

    pub fn center_y(&self) -> f32 {
        self.y + self.height * 0.5
    }

    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px <= self.right() && py >= self.y && py <= self.bottom()
    }

    pub fn contains_padded(&self, px: f32, py: f32, pad_x: f32, pad_y: f32) -> bool {
        px >= (self.x - pad_x)
            && px <= (self.right() + pad_x)
            && py >= (self.y - pad_y)
            && py <= (self.bottom() + pad_y)
    }

    #[allow(dead_code)]
    pub fn intersects(&self, other: &RectF) -> bool {
        self.x < other.right()
            && self.right() > other.x
            && self.y < other.bottom()
            && self.bottom() > other.y
    }

    pub fn intersection_area(&self, other: &RectF) -> f32 {
        let ix1 = self.x.max(other.x);
        let iy1 = self.y.max(other.y);
        let ix2 = self.right().min(other.right());
        let iy2 = self.bottom().min(other.bottom());
        if ix2 > ix1 && iy2 > iy1 {
            (ix2 - ix1) * (iy2 - iy1)
        } else {
            0.0
        }
    }

    pub fn area(&self) -> f32 {
        self.width * self.height
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DetectedWord {
    pub text: String,
    pub bounds: RectF,
    pub line_index: usize,
    pub word_index_in_line: usize,
    pub global_index: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DetectedLine {
    pub words: Vec<DetectedWord>,
    pub bounds: RectF,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionMode {
    /// Select words in natural reading order from start point to end point (like browser/PDF text selection).
    Linear,
    /// Select all words inside a rectangular region (ideal for tables or columns).
    Box,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SelectionResult {
    /// Selected text ready to be copied to the clipboard (with spaces and newlines preserved).
    pub text: String,
    /// Visual highlight rectangles to draw on the overlay.
    pub highlight_rects: Vec<RectF>,
    /// Global word indices that are part of the current selection.
    pub selected_word_indices: Vec<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct OcrLayout {
    pub lines: Vec<DetectedLine>,
    pub words: Vec<DetectedWord>,
}

impl OcrLayout {
    /// Builds a normalized `OcrLayout` from raw lines of `(word_text, word_bounds)`.
    /// Lines are sorted top-to-bottom (grouping lines with significant vertical overlap),
    /// and words within each line are sorted left-to-right.
    pub fn from_raw_lines(raw_lines: Vec<Vec<(String, RectF)>>) -> Self {
        let mut temp_lines: Vec<(RectF, Vec<(String, RectF)>)> = Vec::new();

        for mut line_words in raw_lines {
            line_words.retain(|(t, b)| !t.trim().is_empty() && b.width > 0.0 && b.height > 0.0);
            if line_words.is_empty() {
                continue;
            }
            line_words.sort_by(|a, b| {
                a.1.x
                    .partial_cmp(&b.1.x)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

            // Split lines that jump across wide column gaps (e.g. separate File Explorer icon labels)
            let mut current_seg: Vec<(String, RectF)> = Vec::new();
            for (text, rect) in line_words {
                if let Some((_, prev_rect)) = current_seg.last() {
                    let min_h = prev_rect.height.min(rect.height).max(10.0);
                    let max_col_gap = (min_h * 1.8).clamp(16.0, 38.0);
                    if (rect.x - prev_rect.right()) > max_col_gap {
                        let seg_bounds = compute_union_bounds(current_seg.iter().map(|(_, r)| *r));
                        temp_lines.push((seg_bounds, std::mem::take(&mut current_seg)));
                    }
                }
                current_seg.push((text, rect));
            }
            if !current_seg.is_empty() {
                let seg_bounds = compute_union_bounds(current_seg.iter().map(|(_, r)| *r));
                temp_lines.push((seg_bounds, current_seg));
            }
        }

        // Sort lines in natural reading order: top-to-bottom, with left-to-right tie-breaking
        // when two line segments are on roughly the same vertical band.
        temp_lines.sort_by(|(a, _), (b, _)| {
            let vertical_overlap = (a.bottom().min(b.bottom()) - a.y.max(b.y)).max(0.0);
            let min_h = a.height.min(b.height).max(1.0);
            if vertical_overlap / min_h > 0.5 {
                a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal)
            } else {
                a.center_y()
                    .partial_cmp(&b.center_y())
                    .unwrap_or(std::cmp::Ordering::Equal)
            }
        });

        let mut lines = Vec::with_capacity(temp_lines.len());
        let mut all_words = Vec::new();
        let mut global_index = 0;

        for (line_index, (line_bounds, raw_words)) in temp_lines.into_iter().enumerate() {
            let mut detected_words = Vec::with_capacity(raw_words.len());
            for (word_index_in_line, (text, bounds)) in raw_words.into_iter().enumerate() {
                let word = DetectedWord {
                    text,
                    bounds,
                    line_index,
                    word_index_in_line,
                    global_index,
                };
                global_index += 1;
                all_words.push(word.clone());
                detected_words.push(word);
            }
            lines.push(DetectedLine {
                words: detected_words,
                bounds: line_bounds,
            });
        }

        Self {
            lines,
            words: all_words,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// Returns the word at or very close to `(px, py)` (used for cursor I-Beam feedback).
    pub fn word_at_point(&self, px: f32, py: f32) -> Option<&DetectedWord> {
        for word in &self.words {
            let pad_y = (word.bounds.height * 0.25).clamp(2.0, 8.0);
            let pad_x = (word.bounds.height * 0.35).clamp(3.0, 10.0);
            if word.bounds.contains_padded(px, py, pad_x, pad_y) {
                return Some(word);
            }
        }
        None
    }

    /// Returns all text in the layout formatted with spaces and newlines.
    pub fn full_text(&self) -> String {
        let mut out = String::new();
        for (i, line) in self.lines.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            for (j, word) in line.words.iter().enumerate() {
                if j > 0 {
                    out.push(' ');
                }
                out.push_str(&word.text);
            }
        }
        out
    }

    /// Computes the selection between drag start `(x1, y1)` and current/end `(x2, y2)`.
    pub fn compute_selection(
        &self,
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        mode: SelectionMode,
    ) -> SelectionResult {
        if self.words.is_empty() {
            return SelectionResult::default();
        }

        let drag_dist_sq = (x2 - x1).powi(2) + (y2 - y1).powi(2);
        // Single click on a word selects that word
        if drag_dist_sq < 9.0 {
            if let Some(word) = self.word_at_point(x1, y1) {
                return SelectionResult {
                    text: word.text.clone(),
                    highlight_rects: vec![word.bounds],
                    selected_word_indices: vec![word.global_index],
                };
            }
            return SelectionResult::default();
        }

        match mode {
            SelectionMode::Linear => self.compute_linear_selection(x1, y1, x2, y2),
            SelectionMode::Box => self.compute_box_selection(x1, y1, x2, y2),
        }
    }

    fn compute_linear_selection(&self, x1: f32, y1: f32, x2: f32, y2: f32) -> SelectionResult {
        // Find the anchor word near start and target word near end.
        // If the user started dragging slightly outside a word, check if the drag box intersects any words.
        let start_idx = self.find_reading_anchor(x1, y1, true);
        let end_idx = self.find_reading_anchor(x2, y2, false);

        let (Some(a), Some(b)) = (start_idx, end_idx) else {
            return self.compute_box_selection(x1, y1, x2, y2);
        };

        // Check if the user is dragging strictly within a single word to select a character slice
        if a == b {
            let word = &self.words[a];
            if word.bounds.contains_padded(x1, y1, 8.0, 8.0)
                && word.bounds.contains_padded(x2, y2, 8.0, 8.0)
            {
                return slice_single_word(word, x1, x2);
            }
        }

        let (first, last) = if a <= b { (a, b) } else { (b, a) };
        let selected: Vec<&DetectedWord> = self.words[first..=last].iter().collect();
        build_selection_from_words(&selected)
    }

    fn compute_box_selection(&self, x1: f32, y1: f32, x2: f32, y2: f32) -> SelectionResult {
        let drag_rect = RectF::from_points(x1, y1, x2, y2);
        let mut selected = Vec::new();

        for word in &self.words {
            let center_in = drag_rect.contains(word.bounds.center_x(), word.bounds.center_y());
            let overlap_ratio = if word.bounds.area() > 0.0 {
                word.bounds.intersection_area(&drag_rect) / word.bounds.area()
            } else {
                0.0
            };

            if center_in || overlap_ratio >= 0.50 {
                selected.push(word);
            }
        }

        build_selection_from_words(&selected)
    }

    /// Finds the global word index closest in reading order to `(px, py)`.
    fn find_reading_anchor(&self, px: f32, py: f32, is_start: bool) -> Option<usize> {
        if let Some(exact) = self.word_at_point(px, py) {
            return Some(exact.global_index);
        }

        // First look for a line whose vertical band contains or is closest to `py`.
        let mut best_line_idx = None;
        let mut best_vertical_dist = f32::MAX;

        for (idx, line) in self.lines.iter().enumerate() {
            let pad_y = (line.bounds.height * 0.6).clamp(6.0, 28.0);
            let dist_y = if py < line.bounds.y {
                line.bounds.y - py
            } else if py > line.bounds.bottom() {
                py - line.bounds.bottom()
            } else {
                0.0
            };

            if dist_y <= pad_y && dist_y < best_vertical_dist {
                best_vertical_dist = dist_y;
                best_line_idx = Some(idx);
            }
        }

        if let Some(line_idx) = best_line_idx {
            let line = &self.lines[line_idx];
            // Pick word within this line based on horizontal coordinate `px`
            let mut best_word = None;
            let mut best_dx = f32::MAX;
            for word in &line.words {
                let dx = if px < word.bounds.x {
                    word.bounds.x - px
                } else if px > word.bounds.right() {
                    px - word.bounds.right()
                } else {
                    0.0
                };
                if dx < best_dx {
                    best_dx = dx;
                    best_word = Some(word.global_index);
                }
            }
            if best_word.is_some() {
                return best_word;
            }
        }

        // Fallback: nearest word within a reasonable radius (e.g. 96 px)
        let max_dist_sq = 96.0_f32.powi(2);
        let mut best_idx = None;
        let mut best_dist_sq = max_dist_sq;
        for word in &self.words {
            let anchor_x = if is_start {
                px.clamp(word.bounds.x, word.bounds.right())
            } else {
                px.clamp(word.bounds.x, word.bounds.right())
            };
            let anchor_y = py.clamp(word.bounds.y, word.bounds.bottom());
            let d2 = (px - anchor_x).powi(2) + (py - anchor_y).powi(2);
            if d2 <= best_dist_sq {
                best_dist_sq = d2;
                best_idx = Some(word.global_index);
            }
        }
        best_idx
    }
}

fn slice_single_word(word: &DetectedWord, x1: f32, x2: f32) -> SelectionResult {
    let chars: Vec<char> = word.text.chars().collect();
    let char_count = chars.len();
    if char_count <= 1 || word.bounds.width <= 1.0 {
        return SelectionResult {
            text: word.text.clone(),
            highlight_rects: vec![word.bounds],
            selected_word_indices: vec![word.global_index],
        };
    }

    let min_x = x1.min(x2).clamp(word.bounds.x, word.bounds.right());
    let max_x = x1.max(x2).clamp(word.bounds.x, word.bounds.right());
    let span_ratio = (max_x - min_x) / word.bounds.width;

    // If the user dragged across >= 80% of the word, select the whole word cleanly
    if span_ratio >= 0.80 {
        return SelectionResult {
            text: word.text.clone(),
            highlight_rects: vec![word.bounds],
            selected_word_indices: vec![word.global_index],
        };
    }

    let rel_start = ((min_x - word.bounds.x) / word.bounds.width).clamp(0.0, 1.0);
    let rel_end = ((max_x - word.bounds.x) / word.bounds.width).clamp(0.0, 1.0);

    let start_char = ((rel_start * char_count as f32).floor() as usize).min(char_count - 1);
    let end_char = ((rel_end * char_count as f32).ceil() as usize)
        .clamp(start_char + 1, char_count);

    let sliced_text: String = chars[start_char..end_char].iter().collect();
    let char_w = word.bounds.width / char_count as f32;
    let sub_rect = RectF::new(
        word.bounds.x + start_char as f32 * char_w,
        word.bounds.y,
        (end_char - start_char) as f32 * char_w,
        word.bounds.height,
    );

    SelectionResult {
        text: sliced_text,
        highlight_rects: vec![sub_rect],
        selected_word_indices: vec![word.global_index],
    }
}

fn build_selection_from_words(words: &[&DetectedWord]) -> SelectionResult {
    if words.is_empty() {
        return SelectionResult::default();
    }

    let mut text = String::new();
    let mut highlight_rects = Vec::new();
    let mut selected_word_indices = Vec::with_capacity(words.len());

    let mut current_line: Option<usize> = None;
    let mut current_line_rect: Option<RectF> = None;

    for word in words {
        selected_word_indices.push(word.global_index);

        match current_line {
            None => {
                text.push_str(&word.text);
                current_line = Some(word.line_index);
                current_line_rect = Some(word.bounds);
            }
            Some(prev_line) if prev_line == word.line_index => {
                text.push(' ');
                text.push_str(&word.text);
                if let Some(r) = current_line_rect.as_mut() {
                    // Merge adjacent word highlights on the same line if they are reasonably close
                    if (word.bounds.x - r.right()) <= r.height * 2.5 {
                        let min_x = r.x.min(word.bounds.x);
                        let min_y = r.y.min(word.bounds.y);
                        let max_x = r.right().max(word.bounds.right());
                        let max_y = r.bottom().max(word.bounds.bottom());
                        *r = RectF::new(min_x, min_y, max_x - min_x, max_y - min_y);
                    } else {
                        highlight_rects.push(*r);
                        *r = word.bounds;
                    }
                }
            }
            Some(_) => {
                if let Some(r) = current_line_rect.take() {
                    highlight_rects.push(r);
                }
                text.push('\n');
                text.push_str(&word.text);
                current_line = Some(word.line_index);
                current_line_rect = Some(word.bounds);
            }
        }
    }

    if let Some(r) = current_line_rect {
        highlight_rects.push(r);
    }

    SelectionResult {
        text,
        highlight_rects,
        selected_word_indices,
    }
}

fn compute_union_bounds(rects: impl Iterator<Item = RectF>) -> RectF {
    let mut min_x = f32::MAX;
    let mut min_y = f32::MAX;
    let mut max_x = f32::MIN;
    let mut max_y = f32::MIN;
    let mut any = false;

    for r in rects {
        any = true;
        min_x = min_x.min(r.x);
        min_y = min_y.min(r.y);
        max_x = max_x.max(r.right());
        max_y = max_y.max(r.bottom());
    }

    if !any {
        RectF::new(0.0, 0.0, 0.0, 0.0)
    } else {
        RectF::new(min_x, min_y, max_x - min_x, max_y - min_y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_layout() -> OcrLayout {
        // Line 1: "Hey" "Davin," "what" "should" "we" "build" "today?"
        // Line 2: "CopyScreen" "is" "open" "source"
        let line1 = vec![
            ("Hey".to_string(), RectF::new(100.0, 50.0, 35.0, 20.0)),
            ("Davin,".to_string(), RectF::new(142.0, 50.0, 55.0, 20.0)),
            ("what".to_string(), RectF::new(204.0, 50.0, 42.0, 20.0)),
            ("should".to_string(), RectF::new(253.0, 50.0, 58.0, 20.0)),
            ("we".to_string(), RectF::new(318.0, 50.0, 26.0, 20.0)),
            ("build".to_string(), RectF::new(351.0, 50.0, 44.0, 20.0)),
            ("today?".to_string(), RectF::new(402.0, 50.0, 58.0, 20.0)),
        ];
        let line2 = vec![
            ("CopyScreen".to_string(), RectF::new(100.0, 90.0, 95.0, 20.0)),
            ("is".to_string(), RectF::new(202.0, 90.0, 18.0, 20.0)),
            ("open".to_string(), RectF::new(228.0, 90.0, 42.0, 20.0)),
            ("source".to_string(), RectF::new(278.0, 90.0, 56.0, 20.0)),
        ];
        // Intentionally pass line2 before line1 to verify reading-order sorting
        OcrLayout::from_raw_lines(vec![line2, line1])
    }

    #[test]
    fn sorts_lines_and_words_in_reading_order() {
        let layout = sample_layout();
        assert_eq!(layout.lines.len(), 2);
        assert_eq!(layout.words.len(), 11);
        assert_eq!(layout.words[0].text, "Hey");
        assert_eq!(layout.words[6].text, "today?");
        assert_eq!(layout.words[7].text, "CopyScreen");
        assert_eq!(
            layout.full_text(),
            "Hey Davin, what should we build today?\nCopyScreen is open source"
        );
    }

    #[test]
    fn linear_drag_selects_range_of_words_on_same_line() {
        let layout = sample_layout();
        // Drag from "Davin," (x=150, y=60) to "build" (x=370, y=60)
        let res = layout.compute_selection(150.0, 60.0, 370.0, 60.0, SelectionMode::Linear);
        assert_eq!(res.text, "Davin, what should we build");
        assert_eq!(res.selected_word_indices, vec![1, 2, 3, 4, 5]);
        // Merged into a single continuous highlight rect on line 0
        assert_eq!(res.highlight_rects.len(), 1);
    }

    #[test]
    fn linear_drag_across_multiple_lines_preserves_newline() {
        let layout = sample_layout();
        // Drag from "build" on line 1 (x=360, y=60) to "is" on line 2 (x=210, y=100)
        let res = layout.compute_selection(360.0, 60.0, 210.0, 100.0, SelectionMode::Linear);
        assert_eq!(res.text, "build today?\nCopyScreen is");
        assert_eq!(res.highlight_rects.len(), 2);
    }

    #[test]
    fn reverse_linear_drag_still_outputs_natural_reading_order() {
        let layout = sample_layout();
        // Drag backwards from "today?" to "what"
        let res = layout.compute_selection(430.0, 60.0, 215.0, 60.0, SelectionMode::Linear);
        assert_eq!(res.text, "what should we build today?");
    }

    #[test]
    fn single_word_sub_slice_selects_characters_within_word() {
        let layout = sample_layout();
        // "CopyScreen" has 10 chars at x=100..195 (width 95 => 9.5px per char)
        // Drag across first 4 chars "Copy" => x=100..138
        let res = layout.compute_selection(101.0, 100.0, 136.0, 100.0, SelectionMode::Linear);
        assert_eq!(res.text, "Copy");
        assert_eq!(res.highlight_rects.len(), 1);
    }

    #[test]
    fn box_selection_selects_only_words_inside_rectangle() {
        let layout = sample_layout();
        // Box covering x=195..275 across both lines:
        // Line 1: "what" (204..246)
        // Line 2: "is" (202..220), "open" (228..270)
        let res = layout.compute_selection(195.0, 40.0, 275.0, 120.0, SelectionMode::Box);
        assert_eq!(res.text, "what\nis open");
    }
}
