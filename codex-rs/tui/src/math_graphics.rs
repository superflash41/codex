//! Typeset math occupies ordinary text cells; Kitty placeholders supply only their pixels.

use std::collections::HashMap;
use std::collections::HashSet;
use std::io::Write;
use std::ops::Range;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering;

use base64::Engine;
use image::ImageEncoder;
use latex_rust::Dim;
use ratatui::buffer::Buffer;
use ratatui::buffer::CellDiffOption;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::text::Line;
use ratatui::text::Span;
use unicode_segmentation::UnicodeSegmentation;

use crate::terminal_hyperlinks::HyperlinkLine;
use crate::terminal_hyperlinks::LogicalLineSource;
use crate::width::display_width;
use crate::wrapping::RtOptions;
use crate::wrapping::WrappedLine;

mod coordinates;
#[cfg(test)]
pub(crate) mod tests;

// Inert layout cells, never terminal control sequences or Kitty placeholders in source text.
pub(crate) const SLOT: &str = "\u{e000}";
const MAX_COLUMNS: usize = 256;
const MAX_ROWS: usize = 32;
const CACHE_LIMIT: usize = 128;

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub(crate) struct CellSize {
    pub(crate) width: u16,
    pub(crate) height: u16,
}

#[cfg(test)]
thread_local! {
    pub(crate) static TEST_CELL_SIZE: std::cell::Cell<Option<CellSize>> = const { std::cell::Cell::new(None) };
}

pub(crate) fn cell_size() -> Option<CellSize> {
    #[cfg(test)]
    {
        TEST_CELL_SIZE.get()
    }
    #[cfg(not(test))]
    {
        use codex_terminal_detection::TerminalName;
        let info = codex_terminal_detection::terminal_info();
        if info.multiplexer.is_some()
            || !matches!(info.name, TerminalName::Ghostty | TerminalName::Kitty)
            || crossterm::style::Colored::ansi_color_disabled_memoized()
        {
            return None;
        }
        let size = crossterm::terminal::window_size().ok()?;
        let width = size.width.checked_div(size.columns)?;
        let height = size.height.checked_div(size.rows)?;
        (width > 0 && height > 0 && width <= 256 && height <= 256)
            .then_some(CellSize { width, height })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct MathImage {
    pub(crate) id: u32,
    pub(crate) display: bool,
    pub(crate) columns: usize,
    pub(crate) rows: usize,
    pub(crate) baseline_row: usize,
    payload: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ImageRow {
    pub(crate) image: Arc<MathImage>,
    pub(crate) columns: Range<usize>,
    pub(crate) row: usize,
    pub(crate) image_column: usize,
}

impl ImageRow {
    pub(crate) fn project(&self, range: Range<usize>, prefix: usize) -> Option<Self> {
        let start = self.columns.start.max(range.start);
        let end = self.columns.end.min(range.end);
        (start < end).then(|| Self {
            image: Arc::clone(&self.image),
            columns: prefix + start - range.start..prefix + end - range.start,
            row: self.row,
            image_column: self.image_column + start - self.columns.start,
        })
    }

    pub(crate) fn shift(&mut self, columns: usize) {
        self.columns = self.columns.start + columns..self.columns.end + columns;
    }

    fn symbol(&self, column: usize) -> String {
        format!(
            "\u{10eeee}{}{}",
            coordinates::DIACRITICS[self.row],
            coordinates::DIACRITICS[self.image_column + column - self.columns.start]
        )
    }

    fn color(&self) -> Color {
        let id = self.image.id;
        // These RGB bytes encode a protocol ID, not a theme color.
        crate::terminal_palette::rgb_color(((id >> 16) as u8, (id >> 8) as u8, id as u8))
    }
}

type CacheKey = (String, bool, CellSize, (u8, u8, u8));
static CACHE: LazyLock<Mutex<HashMap<CacheKey, Option<Arc<MathImage>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static NEXT_ID: AtomicU32 = AtomicU32::new(0x800000);

pub(crate) fn typeset(formula: &str, display: bool) -> Option<Arc<MathImage>> {
    let size = cell_size()?;
    if formula.len() > 4096
        || formula
            .chars()
            .any(|ch| ch.is_control() && !ch.is_whitespace())
    {
        return None;
    }
    let color = crate::terminal_palette::default_fg().unwrap_or((208, 208, 208));
    let key = (formula.to_owned(), display, size, color);
    let mut cache = CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(image) = cache.get(&key) {
        return image.clone();
    }
    let image = rasterize(formula, display, size, color).map(Arc::new);
    if cache.len() >= CACHE_LIMIT {
        cache.clear();
    }
    cache.insert(key, image.clone());
    image
}

fn rasterize(
    formula: &str,
    display: bool,
    cell: CellSize,
    color: (u8, u8, u8),
) -> Option<MathImage> {
    static FONT: std::sync::OnceLock<Option<latex_rust::MathFont>> = std::sync::OnceLock::new();
    let font = FONT
        .get_or_init(|| latex_rust::MathFont::stix_two_math().ok())
        .as_ref()?;
    let (ast, _) = latex_rust::parse_with_options(
        formula,
        &latex_rust::ParseOptions::new().with_max_depth(32),
    )
    .ok()?;
    let tree = latex_rust::layout_with_max_depth(
        &ast,
        font,
        if display {
            latex_rust::MathStyle::Display
        } else {
            latex_rust::MathStyle::Text
        },
        32,
    )
    .ok()?;
    let em = Dim::ratio(i64::from(cell.height) * 3, 4);
    let width = (&tree.width * &em).ceil_to_u32().ok()?.max(1);
    let ascent = (&tree.height * &em).ceil_to_u32().ok()?;
    let depth = (&tree.depth * &em).ceil_to_u32().ok()?;
    if width > u32::from(cell.width) * MAX_COLUMNS as u32
        || ascent > u32::from(cell.height) * MAX_ROWS as u32
        || depth > u32::from(cell.height) * MAX_ROWS as u32
    {
        return None;
    }
    let columns = width.div_ceil(u32::from(cell.width)) as usize;
    // shortcut: use a 3/4-cell baseline until terminals expose their font ascent.
    let text_baseline = u32::from(cell.height) * 3 / 4;
    let baseline_row = ascent
        .saturating_sub(text_baseline)
        .div_ceil(u32::from(cell.height)) as usize;
    let top = baseline_row as u32 * u32::from(cell.height) + text_baseline - ascent;
    let rows = (top + ascent + depth)
        .div_ceil(u32::from(cell.height))
        .max(1) as usize;
    if columns > MAX_COLUMNS
        || rows > MAX_ROWS
        || columns == 0
        || columns * rows * usize::from(cell.width) * usize::from(cell.height) > 262_144
    {
        return None;
    }
    let options = latex_rust::PngOptions {
        font_size_pt: em,
        dpi: Dim::from_i64(72),
        color: latex_rust::Color::rgb(color.0, color.1, color.2),
        ..Default::default()
    };
    let png = latex_rust::render_png(&tree, font, &options).ok()?;
    let pixels = image::load_from_memory(&png).ok()?.to_rgba8();
    let mut canvas = image::RgbaImage::new(
        columns as u32 * u32::from(cell.width),
        rows as u32 * u32::from(cell.height),
    );
    image::imageops::overlay(&mut canvas, &pixels, 0, i64::from(top));
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(
            canvas.as_raw(),
            canvas.width(),
            canvas.height(),
            image::ExtendedColorType::Rgba8,
        )
        .ok()?;
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    // Do not wrap IDs: an old scrollback placeholder must never display another equation.
    if id > 0xffffff {
        return None;
    }
    Some(MathImage {
        id,
        display,
        columns,
        rows,
        baseline_row,
        payload: base64::engine::general_purpose::STANDARD.encode(png),
    })
}

#[derive(Default)]
struct Transfers {
    pending: HashMap<u32, Arc<MathImage>>,
    sent: HashSet<u32>,
}
fn with_transfers<T>(f: impl FnOnce(&mut Transfers) -> T) -> T {
    #[cfg(test)]
    {
        thread_local! {
            static TRANSFERS: std::cell::RefCell<Transfers> = std::cell::RefCell::new(Transfers::default());
        }
        TRANSFERS.with(|state| f(&mut state.borrow_mut()))
    }
    #[cfg(not(test))]
    {
        static TRANSFERS: LazyLock<Mutex<Transfers>> =
            LazyLock::new(|| Mutex::new(Transfers::default()));
        f(&mut TRANSFERS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner))
    }
}

fn enqueue(image: &Arc<MathImage>) {
    with_transfers(|state| {
        state.pending.insert(image.id, Arc::clone(image));
    });
}

pub(crate) fn reset_transfers() {
    with_transfers(|state| state.sent.clear());
}

pub(crate) fn write_pending(writer: &mut impl Write) -> std::io::Result<()> {
    with_transfers(|state| {
        // Re-send visible images periodically rather than relying on unlimited terminal image storage.
        if state.sent.len() >= CACHE_LIMIT {
            state.sent.clear();
        }
        for (id, image) in std::mem::take(&mut state.pending) {
            if state.sent.contains(&id) {
                continue;
            }
            for (index, chunk) in image.payload.as_bytes().chunks(4096).enumerate() {
                let more = usize::from((index + 1) * 4096 < image.payload.len());
                if index == 0 {
                    write!(
                        writer,
                        "\x1b_Ga=T,U=1,f=100,q=2,i={id},c={},r={},m={more};",
                        image.columns, image.rows
                    )?;
                } else {
                    write!(writer, "\x1b_Gm={more};")?;
                }
                writer.write_all(chunk)?;
                writer.write_all(b"\x1b\\")?;
            }
            state.sent.insert(id);
        }
        Ok(())
    })
}

pub(crate) fn mark_buffer(buf: &mut Buffer, area: Rect, images: &[ImageRow]) {
    if area.is_empty() {
        return;
    }
    for image in images {
        enqueue(&image.image);
        for column in image.columns.start..image.columns.end.min(usize::from(area.width)) {
            let cell = &mut buf[(area.x + column as u16, area.y)];
            cell.set_symbol(&image.symbol(column))
                .set_fg(image.color())
                .set_diff_option(CellDiffOption::ForcedWidth(std::num::NonZeroU16::MIN));
        }
    }
}

pub(crate) fn decorate(line: &HyperlinkLine) -> Vec<Span<'static>> {
    let mut result = Vec::new();
    let mut column = 0;
    for span in &line.line.spans {
        for grapheme in span.content.graphemes(true) {
            if let Some(image) = line
                .images
                .iter()
                .find(|image| image.columns.contains(&column))
            {
                enqueue(&image.image);
                result.push(Span::styled(
                    image.symbol(column),
                    span.style.fg(image.color()),
                ));
            } else {
                result.push(Span::styled(grapheme.to_owned(), span.style));
            }
            column += display_width(grapheme);
        }
    }
    result
}

/// Reserve each equation as an indivisible token before wrapping the surrounding text.
pub(crate) fn wrap<'a>(line: &'a HyperlinkLine, options: RtOptions<'a>) -> Vec<WrappedLine<'a>> {
    let logical = LogicalLineSource::from_line(&line.line);
    let text = logical.text.as_ref();
    let mut tokens = Vec::new();
    let mut offset = 0;
    let mut column = 0;
    while offset < text.len() {
        let rest = &text[offset..];
        let length = if let Some(image) = line
            .images
            .iter()
            .find(|image| image.columns.start == column)
        {
            (image.columns.end - image.columns.start) * SLOT.len()
        } else {
            let next_image = line
                .images
                .iter()
                .filter(|image| image.columns.start > column)
                .map(|image| image.columns.start - column)
                .min()
                .unwrap_or(usize::MAX);
            let whitespace = rest.starts_with(char::is_whitespace);
            let mut width = 0;
            rest.grapheme_indices(true)
                .take_while(|(_, part)| {
                    let accept =
                        part.chars().all(char::is_whitespace) == whitespace && width < next_image;
                    width += display_width(part);
                    accept
                })
                .last()
                .map_or(0, |(start, part)| start + part.len())
        };
        if length == 0 {
            break;
        }
        tokens.push(offset..offset + length);
        column += display_width(&text[offset..offset + length]);
        offset += length;
    }
    let mut ranges = Vec::new();
    let mut start = 0;
    let mut end = 0;
    let mut used = 0;
    for token in tokens {
        let token_text = &text[token.clone()];
        let width = display_width(token_text);
        let indent = if ranges.is_empty() {
            &options.initial_indent
        } else {
            &options.subsequent_indent
        };
        let available = options.width.saturating_sub(indent.width()).max(1);
        if used > 0 && used + width > available {
            ranges.push(start..end);
            start = token.start;
            used = 0;
        }
        if used == 0 && token_text.chars().all(char::is_whitespace) {
            start = token.end;
            end = token.end;
            continue;
        }
        let indent = if ranges.is_empty() {
            &options.initial_indent
        } else {
            &options.subsequent_indent
        };
        let available = options.width.saturating_sub(indent.width()).max(1);
        if width > available && !token_text.starts_with(SLOT) {
            for (index, part) in token_text.grapheme_indices(true) {
                let w = display_width(part);
                if used > 0 && used + w > available {
                    ranges.push(start..token.start + index);
                    start = token.start + index;
                    used = 0;
                }
                used += w;
            }
        } else {
            used += width;
        }
        end = token.end;
    }
    if start < end {
        ranges.push(start..end);
    }
    ranges
        .into_iter()
        .enumerate()
        .map(|(index, range)| {
            let mut output = if index == 0 {
                options.initial_indent.clone()
            } else {
                options.subsequent_indent.clone()
            };
            let prefix_bytes = output.spans.iter().map(|span| span.content.len()).sum();
            output
                .spans
                .extend(logical.styled_range(range.clone()).spans);
            WrappedLine {
                line: output,
                range,
                prefix_bytes,
            }
        })
        .collect()
}

/// Expand a wrapped text row to its maximum math ascent and descent, retaining one prose baseline.
pub(crate) fn expand(line: HyperlinkLine) -> Vec<HyperlinkLine> {
    if line.images.is_empty() {
        return vec![line];
    }
    let above = line
        .images
        .iter()
        .map(|image| image.image.baseline_row)
        .max()
        .unwrap_or(0);
    let below = line
        .images
        .iter()
        .map(|image| image.image.rows - image.image.baseline_row - 1)
        .max()
        .unwrap_or(0);
    (0..above + below + 1)
        .map(|row| {
            let mut output = if row == above {
                line.clone()
            } else {
                HyperlinkLine::new(Line::default())
            };
            output.images = line
                .images
                .iter()
                .filter_map(|image| {
                    let top = above - image.image.baseline_row;
                    (top <= row && row < top + image.image.rows).then(|| ImageRow {
                        row: row - top,
                        ..image.clone()
                    })
                })
                .collect();
            if row != above {
                let mut spans = Vec::new();
                let mut end = 0;
                for image in &output.images {
                    spans.push(Span::raw(" ".repeat(image.columns.start - end)));
                    spans.push(Span::raw(SLOT.repeat(image.columns.len())));
                    end = image.columns.end;
                }
                output.line.spans = spans;
                // Each padding row has independent provenance so transcript reflow cannot merge it.
                let mut source = LogicalLineSource::from_line(&output.line);
                let mut copy = crate::markdown_copy::CopyLine::default();
                copy.omit = true;
                source.copy = Some(Arc::new(copy));
                output.source = Some(source);
            }
            output
        })
        .collect()
}
