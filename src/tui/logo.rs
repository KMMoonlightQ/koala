use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

// Two square pixels per terminal cell: round ears, pale cheeks, a dark nose.
const PIXELS: [&str; 14] = [
    "  gggg        gggg  ",
    " gllllg      gllllg ",
    "glppplgggggggglppplg",
    "glppgllllllllllgpplg",
    "gllgllllllllllllgllg",
    " ggglldlllllldllggg ",
    "   glldlllllldllg   ",
    "   gllllnnnnllllg   ",
    "   glllhnnnnhlllg   ",
    "    gllhnnnnhllg    ",
    "    gllhhhhhhllg    ",
    "     gllhhhhllg     ",
    "      gggggggg      ",
    "                    ",
];

fn color(pixel: u8) -> Color {
    match pixel {
        b'g' => Color::Rgb(125, 149, 160),
        b'l' => Color::Rgb(185, 203, 207),
        b'h' => Color::Rgb(224, 231, 224),
        b'p' => Color::Rgb(187, 151, 151),
        b'd' | b'n' => Color::Rgb(44, 52, 60),
        _ => Color::Reset,
    }
}

pub(super) fn lines() -> Vec<Line<'static>> {
    render_pixels(&PIXELS)
}

fn render_pixels(pixels: &[&str]) -> Vec<Line<'static>> {
    pixels
        .as_chunks::<2>()
        .0
        .iter()
        .map(|rows| {
            Line::from(
                rows[0]
                    .bytes()
                    .zip(rows[1].bytes())
                    .map(|(top, bottom)| {
                        if top == b' ' {
                            Span::styled(
                                if bottom == b' ' { " " } else { "▄" },
                                Style::default().fg(color(bottom)),
                            )
                        } else {
                            Span::styled("▀", Style::default().fg(color(top)).bg(color(bottom)))
                        }
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

// Four consecutive squares travel around an empty center. Half-blocks make
// a true 3x3 pixel grid in three columns and two terminal rows.
pub(super) fn running(elapsed_ms: u128, paused: bool) -> Vec<Line<'static>> {
    let ring = [
        (0, 0),
        (1, 0),
        (2, 0),
        (2, 1),
        (2, 2),
        (1, 2),
        (0, 2),
        (0, 1),
    ];
    let phase = if paused {
        0
    } else {
        (elapsed_ms / 120 % 8) as usize
    };
    let mut pixels = [[false; 3]; 4];
    for offset in 0..4 {
        let (x, y) = ring[(phase + offset) % 8];
        pixels[y][x] = true;
    }
    (0..2)
        .map(|row| {
            Line::from(
                (0..3)
                    .map(|x| {
                        let symbol = match (pixels[row * 2][x], pixels[row * 2 + 1][x]) {
                            (true, true) => "█",
                            (true, false) => "▀",
                            (false, true) => "▄",
                            (false, false) => " ",
                        };
                        Span::styled(symbol, super::theme::accent())
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}
