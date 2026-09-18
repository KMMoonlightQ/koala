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
        .chunks_exact(2)
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

// A single text-row koala with a fixed-width moving trail. Terminal font
// rendering keeps the character the same height as the adjacent status text.
pub(super) fn running(elapsed_ms: u128, paused: bool) -> Line<'static> {
    let trail = if paused {
        "  "
    } else {
        ["· ", "˙ ", " ·", " ˙"][(elapsed_ms / 150 % 4) as usize]
    };
    Line::from(vec![
        Span::styled(trail, super::theme::accent()),
        Span::raw("🐨 "),
    ])
}
