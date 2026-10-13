use super::*;
use crate::markdown_render::render_markdown_lines_with_width_and_cwd;
use crate::terminal_hyperlinks::HyperlinkParagraph;
use ratatui::style::Style;
use ratatui::widgets::Widget;

pub(crate) struct Graphics;
impl Graphics {
    pub(crate) fn enable() -> Self {
        TEST_CELL_SIZE.set(Some(CellSize {
            width: 10,
            height: 20,
        }));
        Self
    }
}
impl Drop for Graphics {
    fn drop(&mut self) {
        TEST_CELL_SIZE.set(None);
    }
}
fn render(source: &str, width: usize) -> Vec<HyperlinkLine> {
    render_markdown_lines_with_width_and_cwd(source, Some(width), None)
}
fn text(line: &HyperlinkLine) -> String {
    line.line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

#[test]
fn display_equations_are_centered_at_each_width() {
    let _graphics = Graphics::enable();
    for width in [24, 41, 80] {
        let lines = render(r"$$\frac{a}{b}$$", width);
        let rows: Vec<_> = lines.iter().flat_map(|line| &line.images).collect();
        assert!(!rows.is_empty());
        for row in rows {
            assert_eq!(row.columns.start, (width - row.image.columns) / 2);
            assert_eq!(row.columns.len(), row.image.columns);
            assert!(row.columns.end <= width);
        }
    }
}

#[test]
fn inline_math_keeps_prose_on_one_baseline() {
    let _graphics = Graphics::enable();
    let lines = render(r"Before $\frac{a}{b}$ after $x^2$ end.", 80);
    let baseline = lines
        .iter()
        .position(|line| text(line).contains("Before"))
        .unwrap();
    assert!(text(&lines[baseline]).contains("after"));
    assert!(text(&lines[baseline]).contains("end."));
    assert_eq!(lines[baseline].images.len(), 2);
    for image in &lines[baseline].images {
        assert_eq!(image.row, image.image.baseline_row);
    }
    for (index, line) in lines.iter().enumerate() {
        if index != baseline {
            assert!(!text(line).contains("after"));
        }
        assert!(line.width() <= 80);
    }
}

#[test]
fn equations_wrap_atomically_and_leave_punctuation_in_order() {
    let _graphics = Graphics::enable();
    let source = r"Some preceding text $x^2+y^2=z^2$, and following words.";
    for width in [20, 25, 40, 80] {
        let lines = render(source, width);
        assert!(lines.iter().all(|line| line.width() <= width));
        for image in lines.iter().flat_map(|line| &line.images) {
            assert_eq!(image.columns.len(), image.image.columns);
            assert_eq!(image.image_column, 0);
        }
        assert!(lines.iter().any(|line| !line.images.is_empty()));
        let prose: String = lines.iter().map(text).collect();
        assert!(prose.contains(","));
        assert!(prose.contains("following"));
    }
}

#[test]
fn code_money_and_unsupported_tex_never_become_graphics() {
    let _graphics = Graphics::enable();
    for source in [
        "\x60$\\frac{a}{b}$\x60",
        "\x60\x60\x60latex\n$x^2$\n\x60\x60\x60",
        "$5.00 and $10.00",
        r"$\input{/etc/passwd}$",
    ] {
        assert!(
            render(source, 80).iter().all(|line| line.images.is_empty()),
            "{source}"
        );
    }
    assert!(
        render(r"$x^2$", 5)
            .iter()
            .all(|line| line.images.is_empty())
    );
    assert!(typeset(&"{".repeat(4097), true).is_none());
    assert!(typeset(&format!("{}x{}", "{".repeat(40), "}".repeat(40)), true).is_none());
}

#[test]
fn terminal_without_graphics_retains_existing_text_rendering() {
    assert!(
        render(r"Before $x^2$ after.", 80)
            .iter()
            .all(|line| line.images.is_empty())
    );
}

#[test]
fn text_only_consumers_never_receive_image_slots() {
    let _graphics = Graphics::enable();
    let text = crate::markdown_render::render_markdown_text_with_width_and_cwd(
        r"Before $\frac{a}{b}$ after.",
        Some(80),
        None,
    );
    assert!(!text.to_string().contains(SLOT));
    use crate::history_cell::HistoryCell;
    let source = r"Before $\frac{a}{b}$ after.";
    let cell = crate::history_cell::AgentMessageCell::new_hyperlink_lines(render(source, 80), true);
    let raw: String = cell.raw_lines().iter().map(ToString::to_string).collect();
    assert_eq!(raw, source);
}

#[test]
fn image_uploads_are_chunked_deduplicated_and_restored_after_screen_changes() {
    let _graphics = Graphics::enable();
    let image = typeset(r"\sum_{i=1}^{n} x_i^2", true).unwrap();
    enqueue(&image);
    enqueue(&image);
    let mut bytes = Vec::new();
    write_pending(&mut bytes).unwrap();
    let output = String::from_utf8(bytes).unwrap();
    assert!(output.starts_with(&format!(
        "\x1b_Ga=T,U=1,f=100,q=2,i={},c={},r={},",
        image.id, image.columns, image.rows
    )));
    assert!(output.ends_with("\x1b\\"));
    let payload: String = output
        .split("\x1b\\")
        .filter_map(|chunk| chunk.split_once(';').map(|(_, payload)| payload))
        .collect();
    assert_eq!(payload, image.payload);
    assert!(
        output
            .split("\x1b\\")
            .filter_map(|chunk| chunk.split_once(';'))
            .all(|(_, payload)| payload.len() <= 4096)
    );
    let mut repeated = Vec::new();
    enqueue(&image);
    write_pending(&mut repeated).unwrap();
    assert!(repeated.is_empty());
    reset_transfers();
    enqueue(&image);
    write_pending(&mut repeated).unwrap();
    assert_eq!(repeated, output.as_bytes());
}

#[test]
fn clipping_renders_only_the_visible_tiles_inside_the_viewport() {
    let _graphics = Graphics::enable();
    let lines = render(r"$$\frac{\frac{a}{b}}{\frac{c}{d}}$$", 40);
    assert!(lines.len() >= 3);
    let area = Rect::new(2, 2, 40, 1);
    let mut buffer = Buffer::with_lines(vec!["guard".repeat(10); 5]);
    let guard = buffer.clone();
    HyperlinkParagraph::new(&lines, Style::default())
        .scroll(1)
        .render(area, &mut buffer);
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            if !area.contains((x, y).into()) {
                assert_eq!(buffer[(x, y)], guard[(x, y)]);
            }
        }
    }
    let visible = &lines[1].images[0];
    let symbol = buffer[(area.x + visible.columns.start as u16, area.y)].symbol();
    assert_eq!(symbol, visible.symbol(visible.columns.start));
}

#[test]
fn raster_uses_cell_metrics_and_preserves_transparency() {
    for cell in [
        CellSize {
            width: 8,
            height: 16,
        },
        CellSize {
            width: 14,
            height: 28,
        },
    ] {
        let image =
            rasterize(r"\int_0^1 x^2\,dx=\frac{1}{3}", true, cell, (240, 220, 200)).unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&image.payload)
            .unwrap();
        let pixels = image::load_from_memory(&bytes).unwrap().to_rgba8();
        assert_eq!(pixels.width(), image.columns as u32 * u32::from(cell.width));
        assert_eq!(pixels.height(), image.rows as u32 * u32::from(cell.height));
        assert!(pixels.pixels().any(|pixel| pixel[3] == 0));
        assert!(pixels.pixels().any(|pixel| pixel[3] > 0));
        assert!(image.baseline_row < image.rows);
    }
}

#[test]
fn lists_and_links_keep_their_positions_around_inline_math() {
    let _graphics = Graphics::enable();
    let lines = render(r"- Before $x^2$ [after](https://example.com) end.", 80);
    let baseline = lines
        .iter()
        .find(|line| text(line).contains("Before"))
        .unwrap();
    assert_eq!(baseline.images.len(), 1);
    assert!(!baseline.hyperlinks.is_empty());
    assert!(baseline.hyperlinks[0].columns.start >= baseline.images[0].columns.end);
}

#[test]
fn copying_math_restores_tex_instead_of_layout_cells() {
    let _graphics = Graphics::enable();
    for input in [
        r"Before $\frac{a}{b}$ after.",
        r"$$\frac{a}{b}$$",
        r"> Before $x^2$ after.",
    ] {
        let lines = render(input, 80);
        let mut selected = Vec::new();
        for line in &lines {
            if let Some(source) = &line.source {
                crate::markdown_copy::SelectedLine::append(
                    &mut selected,
                    source.clone(),
                    source.range.clone(),
                    "\n",
                );
            }
        }
        let (copied, _) = crate::markdown_copy::selection(&selected, "unused");
        assert!(
            copied.contains(if input.contains("frac") {
                r"\frac{a}{b}"
            } else {
                "x^2"
            }),
            "{copied}"
        );
        assert!(!copied.contains(SLOT));
        assert_eq!(
            copied
                .matches(if input.contains("frac") {
                    r"\frac{a}{b}"
                } else {
                    "x^2"
                })
                .count(),
            1
        );
        let plain = crate::markdown_copy::literal_selection(&selected, "unused");
        assert!(!plain.contains(SLOT));
    }
}

#[test]
fn adjacent_equal_equations_remain_distinct_when_copied() {
    let _graphics = Graphics::enable();
    let input = r"\(x^2\)\(x^2\)";
    let mut selected = Vec::new();
    for line in render(input, 80) {
        if let Some(source) = line.source {
            crate::markdown_copy::SelectedLine::append(
                &mut selected,
                source.clone(),
                source.range.clone(),
                "\n",
            );
        }
    }
    assert_eq!(
        crate::markdown_copy::selection(&selected, "unused").0,
        input
    );
}

#[test]
fn streaming_and_completed_math_have_identical_layout() {
    let _graphics = Graphics::enable();
    let input = "Before $x^2$ after.\n\n$$\\frac{a}{b}$$\n\nFollowing paragraph.";
    let completed = render(input, 60);
    let streamed = crate::markdown_render::render_streaming_markdown_lines_with_width_and_cwd(
        input,
        Some(60),
        None,
        &|_| false,
        crate::markdown_render::ListSpacing::default(),
    );
    assert_eq!(completed, streamed.lines);
    let partial = crate::markdown_render::render_streaming_markdown_lines_with_width_and_cwd(
        "Before.\n\n$$\\frac{a}",
        Some(60),
        None,
        &|_| false,
        crate::markdown_render::ListSpacing::default(),
    );
    assert!(partial.pending_math_start.is_some());
    assert!(partial.lines.iter().all(|line| line.images.is_empty()));
}
