use alacritty_terminal::Term;
use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::selection::SelectionRange;
use alacritty_terminal::term::TermDamage;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor};
use gpui_kit::*;

use super::colors::Palette;

#[cfg(target_os = "windows")]
const FALLBACK_FONTS: &[&str] = &["Consolas", "Segoe UI Symbol", "Segoe UI Emoji"];
#[cfg(target_os = "macos")]
const FALLBACK_FONTS: &[&str] = &["Monaco", "Apple Color Emoji"];
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const FALLBACK_FONTS: &[&str] = &["Liberation Mono", "Noto Color Emoji"];

/// TUIs and shell prompts draw icons from Nerd Fonts' Private Use Area glyphs; whichever of these
/// are installed fill in what the main font lacks.
const NERD_FONTS: &[&str] = &[
    "Symbols Nerd Font Mono",
    "Symbols Nerd Font",
    "CaskaydiaMono Nerd Font",
    "CaskaydiaCove Nerd Font",
    "JetBrainsMono Nerd Font",
    "FiraCode Nerd Font",
    "Hack Nerd Font",
    "MesloLGS Nerd Font",
    "3270 Nerd Font",
];

/// How the cursor should be drawn this frame.
pub struct CursorState {
    pub focused: bool,
    /// False during the "off" half of a blink.
    pub visible: bool,
    /// IME composition in progress, drawn in place of the cursor.
    pub marked_text: String,
}

/// The font and cell size every row of the grid is laid out with.
#[derive(Clone)]
pub struct CellMetrics {
    pub font: Font,
    pub font_size: Pixels,
    pub cell_width: Pixels,
    pub cell_height: Pixels,
}

impl CellMetrics {
    pub fn measure(family: &str, font_size: Pixels, line_height: f32, window: &Window) -> Self {
        let font = Font {
            family: family.to_owned().into(),
            features: FontFeatures::disable_ligatures(),
            fallbacks: Some(FontFallbacks::from_fonts(
                FALLBACK_FONTS.iter().chain(NERD_FONTS).map(|font| font.to_string()).collect(),
            )),
            weight: FontWeight::NORMAL,
            style: FontStyle::Normal,
        };
        let text_system = window.text_system();
        let font_id = text_system.resolve_font(&font);
        let cell_width = text_system
            .em_advance(font_id, font_size)
            .unwrap_or(font_size * 0.6);

        Self {
            font,
            font_size,
            cell_width,
            cell_height: (font_size * line_height).round(),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
struct CellStyle {
    color: Hsla,
    bold: bool,
    italic: bool,
    underline: bool,
    strikethrough: bool,
}

struct TextBatch {
    column: usize,
    columns: usize,
    text: String,
    style: CellStyle,
    wide: bool,
}

struct BackgroundRect {
    column: usize,
    columns: usize,
    color: Hsla,
}

#[derive(Default)]
struct CachedRow {
    backgrounds: Vec<BackgroundRect>,
    text: Vec<(usize, ShapedLine)>,
}

/// Shaped rows from previous frames. Only rows alacritty reports as damaged are laid out again;
/// the cursor and selection are drawn on top, so moving them never invalidates a row.
#[derive(Default)]
pub struct GridCache {
    columns: usize,
    rows: Vec<CachedRow>,
}

impl GridCache {
    fn update<T: EventListener>(
        &mut self,
        term: &mut Term<T>,
        metrics: &CellMetrics,
        palette: &Palette,
        window: &mut Window,
    ) {
        let lines = term.screen_lines();
        let columns = term.columns();
        let resized = self.rows.len() != lines || self.columns != columns;
        let damaged: Vec<usize> = match term.damage() {
            TermDamage::Partial(damage) if !resized => damage.map(|bounds| bounds.line).collect(),
            _ => (0..lines).collect(),
        };
        term.reset_damage();

        if resized {
            self.columns = columns;
            self.rows.clear();
            self.rows.resize_with(lines, CachedRow::default);
        }
        for row in damaged.into_iter().filter(|row| *row < lines) {
            self.rows[row] = layout_row(term, row, metrics, palette, window);
        }
    }
}

fn layout_row<T>(
    term: &Term<T>,
    row: usize,
    metrics: &CellMetrics,
    palette: &Palette,
    window: &mut Window,
) -> CachedRow {
    let grid = term.grid();
    let overrides = term.colors();
    let default_background = palette.named(NamedColor::Background, overrides);
    let line = &grid[Line(row as i32 - grid.display_offset() as i32)];

    let mut backgrounds: Vec<BackgroundRect> = Vec::new();
    let mut batches: Vec<TextBatch> = Vec::new();

    for column in 0..grid.columns() {
        let cell = &line[Column(column)];
        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue;
        }
        let wide = cell.flags.contains(Flags::WIDE_CHAR);
        let columns = if wide { 2 } else { 1 };

        let (mut foreground, background) = cell_colors(cell, palette, overrides);
        if cell.flags.contains(Flags::HIDDEN) {
            foreground = background;
        }

        if background != default_background {
            match backgrounds.last_mut() {
                Some(rect) if rect.color == background && rect.column + rect.columns == column => {
                    rect.columns += columns;
                }
                _ => backgrounds.push(BackgroundRect {
                    column,
                    columns,
                    color: background,
                }),
            }
        }

        let style = cell_style(cell, foreground);
        let blank = cell.c == ' ' || cell.c == '\0';
        let decorated = style.underline || style.strikethrough;

        let extends_last = batches.last().is_some_and(|batch| {
            !batch.wide
                && !wide
                && batch.column + batch.columns == column
                && (batch.style == style || (blank && !decorated))
        });
        if extends_last {
            let batch = batches.last_mut().unwrap();
            batch.columns += 1;
            batch.text.push(if blank { ' ' } else { cell.c });
            batch.text.extend(cell.zerowidth().into_iter().flatten());
        } else if !blank || decorated {
            let mut text = String::from(if blank { ' ' } else { cell.c });
            text.extend(cell.zerowidth().into_iter().flatten());
            batches.push(TextBatch {
                column,
                columns,
                text,
                style,
                wide,
            });
        }
    }

    let text = batches
        .into_iter()
        .filter_map(|mut batch| {
            if !batch.style.underline && !batch.style.strikethrough {
                batch.text.truncate(batch.text.trim_end_matches(' ').len());
            }
            (!batch.text.is_empty())
                .then(|| (batch.column, shape(batch.text, batch.style, batch.wide, metrics, window)))
        })
        .collect();

    CachedRow { backgrounds, text }
}

pub fn paint_grid<T: EventListener>(
    term: &mut Term<T>,
    cache: &mut GridCache,
    origin: Point<Pixels>,
    metrics: &CellMetrics,
    palette: &Palette,
    cursor_state: &CursorState,
    window: &mut Window,
    cx: &mut App,
) {
    cache.update(term, metrics, palette, window);

    let content = term.renderable_content();
    let overrides = content.colors;
    let display_offset = content.display_offset as i32;
    let default_background = palette.named(NamedColor::Background, overrides);
    let cursor_color = palette.named(NamedColor::Cursor, overrides);
    let selection_color = Hsla { a: 0.35, ..cursor_color };

    let cell_origin = |row: usize, column: usize| {
        point(
            origin.x + metrics.cell_width * column as f32,
            origin.y + metrics.cell_height * row as f32,
        )
    };
    let cell_bounds = |row: usize, column: usize, columns: usize| {
        Bounds::new(
            cell_origin(row, column),
            size(metrics.cell_width * columns as f32, metrics.cell_height),
        )
    };

    for (row, cached) in cache.rows.iter().enumerate() {
        for rect in &cached.backgrounds {
            window.paint_quad(fill(cell_bounds(row, rect.column, rect.columns), rect.color));
        }
    }

    if let Some(selection) = content.selection {
        for row in 0..cache.rows.len() {
            let line = Line(row as i32 - display_offset);
            if let Some((start, end)) = selection_columns(&selection, line, cache.columns) {
                window.paint_quad(fill(cell_bounds(row, start, end - start), selection_color));
            }
        }
    }

    for (row, cached) in cache.rows.iter().enumerate() {
        for (column, line) in &cached.text {
            let _ = line.paint(
                cell_origin(row, *column),
                metrics.cell_height,
                TextAlign::Left,
                None,
                window,
                cx,
            );
        }
    }

    let cursor = content.cursor;
    let cursor_row = cursor.point.line.0 + display_offset;
    if cursor_row < 0 || cursor_row as usize >= cache.rows.len() {
        return;
    }
    let cursor_row = cursor_row as usize;
    let cursor_column = cursor.point.column.0;
    let cursor_cell = &term.grid()[cursor.point];
    let cursor_columns = if cursor_cell.flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
    let cursor_origin = cell_origin(cursor_row, cursor_column);

    let cursor_shape = match cursor.shape {
        CursorShape::Hidden => CursorShape::Hidden,
        _ if !cursor_state.marked_text.is_empty() => CursorShape::Hidden,
        _ if !cursor_state.focused => CursorShape::HollowBlock,
        _ if !cursor_state.visible => CursorShape::Hidden,
        shape => shape,
    };
    match cursor_shape {
        CursorShape::Block => {
            window.paint_quad(fill(
                cell_bounds(cursor_row, cursor_column, cursor_columns),
                cursor_color,
            ));
            if cursor_cell.c != ' ' && cursor_cell.c != '\0' {
                let mut text = String::from(cursor_cell.c);
                text.extend(cursor_cell.zerowidth().into_iter().flatten());
                let style = cell_style(cursor_cell, default_background);
                let line = shape(text, style, cursor_columns == 2, metrics, window);
                let _ = line.paint(cursor_origin, metrics.cell_height, TextAlign::Left, None, window, cx);
            }
        }
        CursorShape::Beam => window.paint_quad(fill(
            Bounds::new(cursor_origin, size(px(2.), metrics.cell_height)),
            cursor_color,
        )),
        CursorShape::Underline => window.paint_quad(fill(
            Bounds::new(
                point(cursor_origin.x, cursor_origin.y + metrics.cell_height - px(2.)),
                size(metrics.cell_width * cursor_columns as f32, px(2.)),
            ),
            cursor_color,
        )),
        CursorShape::HollowBlock => window.paint_quad(outline(
            cell_bounds(cursor_row, cursor_column, cursor_columns),
            cursor_color,
            BorderStyle::Solid,
        )),
        CursorShape::Hidden => {}
    }

    if !cursor_state.marked_text.is_empty() {
        let foreground = palette.named(NamedColor::Foreground, overrides);
        let run = TextRun {
            len: cursor_state.marked_text.len(),
            font: metrics.font.clone(),
            color: foreground,
            background_color: None,
            underline: Some(UnderlineStyle {
                thickness: px(1.),
                color: Some(foreground),
                wavy: false,
            }),
            strikethrough: None,
        };
        let line = window.text_system().shape_line(
            cursor_state.marked_text.clone().into(),
            metrics.font_size,
            &[run],
            None,
        );
        window.paint_quad(fill(
            Bounds::new(cursor_origin, size(line.width, metrics.cell_height)),
            default_background,
        ));
        let _ = line.paint(cursor_origin, metrics.cell_height, TextAlign::Left, None, window, cx);
    }
}

fn shape(
    text: String,
    style: CellStyle,
    wide: bool,
    metrics: &CellMetrics,
    window: &mut Window,
) -> ShapedLine {
    let run = TextRun {
        len: text.len(),
        font: Font {
            weight: if style.bold {
                FontWeight::BOLD
            } else {
                FontWeight::NORMAL
            },
            style: if style.italic {
                FontStyle::Italic
            } else {
                FontStyle::Normal
            },
            ..metrics.font.clone()
        },
        color: style.color,
        background_color: None,
        underline: style.underline.then_some(UnderlineStyle {
            thickness: px(1.),
            color: Some(style.color),
            wavy: false,
        }),
        strikethrough: style.strikethrough.then_some(StrikethroughStyle {
            thickness: px(1.),
            color: Some(style.color),
        }),
    };
    let force_width = if wide {
        metrics.cell_width * 2.
    } else {
        metrics.cell_width
    };
    window
        .text_system()
        .shape_line(text.into(), metrics.font_size, &[run], Some(force_width))
}

/// The selected columns of one grid line, as a half-open range.
fn selection_columns(selection: &SelectionRange, line: Line, columns: usize) -> Option<(usize, usize)> {
    if line < selection.start.line || line > selection.end.line {
        return None;
    }
    let last = columns.saturating_sub(1);
    let start = if selection.is_block || line == selection.start.line {
        selection.start.column.0.min(last)
    } else {
        0
    };
    let end = if selection.is_block || line == selection.end.line {
        selection.end.column.0.min(last)
    } else {
        last
    };
    (start <= end).then_some((start, end + 1))
}

fn cell_style(cell: &Cell, color: Hsla) -> CellStyle {
    CellStyle {
        color,
        bold: cell.flags.contains(Flags::BOLD),
        italic: cell.flags.contains(Flags::ITALIC),
        underline: cell.flags.intersects(Flags::ALL_UNDERLINES),
        strikethrough: cell.flags.contains(Flags::STRIKEOUT),
    }
}

fn cell_colors(cell: &Cell, palette: &Palette, overrides: &Colors) -> (Hsla, Hsla) {
    let mut foreground = cell.fg;
    if let Color::Named(named) = foreground {
        if cell.flags.contains(Flags::DIM) {
            foreground = Color::Named(named.to_dim());
        } else if cell.flags.contains(Flags::BOLD) && (named as usize) < 8 {
            foreground = Color::Named(named.to_bright());
        }
    }

    let foreground = palette.resolve(foreground, overrides);
    let background = palette.resolve(cell.bg, overrides);
    if cell.flags.contains(Flags::INVERSE) {
        (background, foreground)
    } else {
        (foreground, background)
    }
}
