//! Text styles: colors, sizes, and fonts, as Docs keeps them, and as they're
//! read from the CSS in other programs' HTML.
//!
//! Everything here is checked before it's kept, since the file writes it
//! back out inside `style` attributes. A color is six hex digits, a size is
//! a number, and a font's name has only letters, digits, spaces, and a few
//! marks, so nothing read can end the attribute or add a rule of its own.

use crate::fonts;

/// A color, as red, green, and blue.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Color(pub [u8; 3]);

impl Color {
    /// The color text has when nothing's set. Picking it means no color, so
    /// text pasted from Google Docs, which names it everywhere, stays plain.
    pub const BLACK: Color = Color([0, 0, 0]);

    /// As CSS writes it, such as `#1a5fb4`.
    pub fn css(self) -> String {
        format!("#{}", self.hex())
    }

    /// Six hex digits, as Word writes them, such as `1A5FB4`.
    pub fn hex_upper(self) -> String {
        self.hex().to_ascii_uppercase()
    }

    fn hex(self) -> String {
        let [r, g, b] = self.0;
        format!("{r:02x}{g:02x}{b:02x}")
    }

    /// A color as the editor writes it, `#rrggbb`.
    pub fn from_hex(text: &str) -> Option<Color> {
        let digits = text.trim().strip_prefix('#')?;
        if digits.len() != 6 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let part = |at: usize| u8::from_str_radix(&digits[at..at + 2], 16).ok();
        Some(Color([part(0)?, part(2)?, part(4)?]))
    }

    /// A color as CSS may write it: `#rgb`, `#rrggbb`, `#rrggbbaa`, `rgb()`,
    /// `rgba()`, or one of the common names. One that can't be seen, such
    /// as `transparent`, isn't a color.
    pub fn from_css(text: &str) -> Option<Color> {
        let text = text.trim().to_ascii_lowercase();
        if let Some(digits) = text.strip_prefix('#') {
            if !digits.chars().all(|c| c.is_ascii_hexdigit()) {
                return None;
            }
            let nibble = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
            let bytes = digits.as_bytes();
            return match bytes.len() {
                3 | 4 => {
                    if bytes.len() == 4 && nibble(bytes[3])? == 0 {
                        return None;
                    }
                    let one = |at: usize| nibble(bytes[at]).map(|d| d * 17);
                    Some(Color([one(0)?, one(1)?, one(2)?]))
                }
                6 | 8 => {
                    let two = |at: usize| u8::from_str_radix(&digits[at..at + 2], 16).ok();
                    if bytes.len() == 8 && two(6)? == 0 {
                        return None;
                    }
                    Some(Color([two(0)?, two(2)?, two(4)?]))
                }
                _ => None,
            };
        }
        if let Some(inside) = text
            .strip_prefix("rgba(")
            .or_else(|| text.strip_prefix("rgb("))
            .and_then(|rest| rest.strip_suffix(')'))
        {
            let parts: Vec<&str> = inside
                .split([',', ' ', '/'])
                .filter(|part| !part.is_empty())
                .collect();
            if parts.len() != 3 && parts.len() != 4 {
                return None;
            }
            if let Some(alpha) = parts.get(3) {
                let alpha: f64 = match alpha.strip_suffix('%') {
                    Some(percent) => percent.parse::<f64>().ok()? / 100.0,
                    None => alpha.parse().ok()?,
                };
                if alpha <= 0.0 {
                    return None;
                }
            }
            let channel = |part: &str| -> Option<u8> {
                let value: f64 = match part.strip_suffix('%') {
                    Some(percent) => percent.parse::<f64>().ok()? * 2.55,
                    None => part.parse().ok()?,
                };
                value
                    .is_finite()
                    .then(|| value.round().clamp(0.0, 255.0) as u8)
            };
            return Some(Color([
                channel(parts[0])?,
                channel(parts[1])?,
                channel(parts[2])?,
            ]));
        }
        NAMED
            .iter()
            .find(|(name, _)| *name == text)
            .map(|(_, rgb)| Color(*rgb))
    }
}

/// The color names people use most, and the 16 Word's highlights use.
const NAMED: &[(&str, [u8; 3])] = &[
    ("black", [0, 0, 0]),
    ("white", [255, 255, 255]),
    ("red", [255, 0, 0]),
    ("green", [0, 128, 0]),
    ("blue", [0, 0, 255]),
    ("yellow", [255, 255, 0]),
    ("cyan", [0, 255, 255]),
    ("aqua", [0, 255, 255]),
    ("magenta", [255, 0, 255]),
    ("fuchsia", [255, 0, 255]),
    ("gray", [128, 128, 128]),
    ("grey", [128, 128, 128]),
    ("silver", [192, 192, 192]),
    ("maroon", [128, 0, 0]),
    ("olive", [128, 128, 0]),
    ("lime", [0, 255, 0]),
    ("navy", [0, 0, 128]),
    ("purple", [128, 0, 128]),
    ("teal", [0, 128, 128]),
    ("orange", [255, 165, 0]),
    ("pink", [255, 192, 203]),
    ("brown", [165, 42, 42]),
    ("darkblue", [0, 0, 139]),
    ("darkcyan", [0, 139, 139]),
    ("darkgray", [169, 169, 169]),
    ("darkgreen", [0, 100, 0]),
    ("darkmagenta", [139, 0, 139]),
    ("darkred", [139, 0, 0]),
    ("darkyellow", [128, 128, 0]),
    ("lightgray", [211, 211, 211]),
];

/// A font size in half points, as Word keeps them, so 10.5 points is 21.
pub type HalfPoints = u16;

/// The smallest and largest sizes kept, in points, as Google Docs allows.
pub const MIN_SIZE: f64 = 1.0;
pub const MAX_SIZE: f64 = 400.0;

/// A size in points as half points, rounded to the nearest half point, if
/// it's one Docs keeps.
pub fn half_points(points: f64) -> Option<HalfPoints> {
    (points.is_finite() && (MIN_SIZE..=MAX_SIZE).contains(&points))
        .then(|| (points * 2.0).round() as HalfPoints)
}

/// Half points as points, such as `10.5`.
pub fn points(size: HalfPoints) -> f64 {
    f64::from(size) / 2.0
}

/// Points as CSS writes them, without a needless `.0`.
pub fn points_text(size: HalfPoints) -> String {
    if size.is_multiple_of(2) {
        (size / 2).to_string()
    } else {
        format!("{}.5", size / 2)
    }
}

/// A CSS font size in points: `12pt`, `16px`, `1.5em` of `parent`, and
/// the keywords. A size relative to an unknown parent assumes 11 points.
pub fn size_from_css(text: &str, parent: Option<HalfPoints>) -> Option<HalfPoints> {
    let text = text.trim().to_ascii_lowercase();
    let parent = parent.map_or(11.0, points);
    let keyword = match text.as_str() {
        "xx-small" => Some(7.0),
        "x-small" => Some(7.5),
        "small" => Some(10.0),
        "medium" => Some(12.0),
        "large" => Some(13.5),
        "x-large" => Some(18.0),
        "xx-large" => Some(24.0),
        "xxx-large" => Some(36.0),
        _ => None,
    };
    if let Some(points) = keyword {
        return half_points(points);
    }
    let split = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let number: f64 = number.parse().ok()?;
    let points = match unit.trim() {
        "pt" => number,
        "px" => number * 0.75,
        "em" | "rem" => number * parent,
        "%" => number / 100.0 * parent,
        "in" => number * 72.0,
        "cm" => number * 72.0 / 2.54,
        "mm" => number * 72.0 / 25.4,
        "pc" => number * 12.0,
        _ => return None,
    };
    half_points(points)
}

/// A CSS length in points: `36pt`, `48px`, `0.5in`, `1.27cm`, `2em` (of
/// 11 points), or `0`. Lengths below zero stay, for hanging indents.
pub fn length_from_css(text: &str) -> Option<f64> {
    let text = text.trim().to_ascii_lowercase();
    let split = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+'))
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let number: f64 = number.parse().ok()?;
    let points = match unit.trim() {
        "pt" => number,
        "px" => number * 0.75,
        "em" | "rem" => number * 11.0,
        "in" => number * 72.0,
        "cm" => number * 72.0 / 2.54,
        "mm" => number * 72.0 / 25.4,
        "pc" => number * 12.0,
        "" if number == 0.0 => 0.0,
        _ => return None,
    };
    points.is_finite().then_some(points)
}

/// Line spacing from a CSS `line-height`, in hundredths of single spacing.
/// Programs write it three ways: Google Docs as a number, 1.2 times its
/// spacing; Word as a percentage of its spacing, such as `115%`; and some
/// as a length, here taken as one of 11-point text.
pub fn line_from_css(text: &str) -> Option<u16> {
    let text = text.trim().to_ascii_lowercase();
    let line = if let Some(percent) = text.strip_suffix('%') {
        percent.trim().parse::<f64>().ok()? / 100.0
    } else if let Ok(number) = text.parse::<f64>() {
        number / 1.2
    } else {
        length_from_css(&text)? / (11.0 * crate::model::SINGLE)
    };
    (line.is_finite() && line > 0.0).then(|| {
        let hundredths = (line * 100.0).round().clamp(1.0, 10_000.0) as u16;
        hundredths.clamp(
            *crate::model::LINE_SPACING.start(),
            *crate::model::LINE_SPACING.end(),
        )
    })
}

/// The `size` of an old-style `<font size>`, 1 to 7, in points as browsers
/// show them.
pub fn size_from_font_tag(text: &str) -> Option<HalfPoints> {
    let points = match text.trim() {
        "1" => 7.5,
        "2" => 10.0,
        "3" => 12.0,
        "4" => 13.5,
        "5" => 18.0,
        "6" => 24.0,
        "7" => 36.0,
        _ => return None,
    };
    half_points(points)
}

/// The longest font name kept.
const MAX_FONT_NAME: usize = 64;

/// A font's name if it's one Docs can write safely: letters, digits,
/// spaces, and `-`, `_`, `.`, and `&`, at most 64 characters.
pub fn font_name(name: &str) -> Option<String> {
    let name = name.trim();
    let allowed = |c: char| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.' | '&');
    (!name.is_empty() && name.chars().count() <= MAX_FONT_NAME && name.chars().all(allowed))
        .then(|| name.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// The font Docs uses for a CSS `font-family` ([`fonts::resolve`]), if
/// it names a font, or a generic family such as `serif`. One that names
/// neither, such as `url(x)`, sets nothing.
pub fn font_from_css(text: &str) -> Option<fonts::Resolved> {
    const GENERIC: &[&str] = &[
        "serif",
        "sans-serif",
        "monospace",
        "cursive",
        "fantasy",
        "system-ui",
        "ui-serif",
        "ui-sans-serif",
        "ui-monospace",
        "ui-rounded",
        "math",
        "emoji",
        "fangsong",
        "-apple-system",
        "blinkmacsystemfont",
        "-webkit-standard",
    ];
    let readable = text.split(',').any(|name| {
        let name = name.trim().trim_matches(['"', '\'']).trim();
        GENERIC.contains(&name.to_ascii_lowercase().as_str()) || font_name(name).is_some()
    });
    readable.then(|| fonts::resolve(text))
}

/// The declarations in a `style` attribute, each name in lowercase, with
/// `!important` dropped.
pub fn declarations(style: &str) -> Vec<(String, String)> {
    style
        .split(';')
        .filter_map(|declaration| {
            let (name, value) = declaration.split_once(':')?;
            let value = value.trim();
            let value = value
                .strip_suffix("!important")
                .map_or(value, str::trim_end);
            Some((name.trim().to_ascii_lowercase(), value.to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_lengths_and_line_spacing() {
        assert_eq!(length_from_css("36pt"), Some(36.0));
        assert_eq!(length_from_css("0.5in"), Some(36.0));
        assert_eq!(length_from_css("-18pt"), Some(-18.0));
        assert_eq!(length_from_css("48px"), Some(36.0));
        assert_eq!(length_from_css("0"), Some(0.0));
        assert_eq!(length_from_css("auto"), None);
        // Google Docs' 1.15, Word's 1.15, and double spacing.
        assert_eq!(line_from_css("1.38"), Some(115));
        assert_eq!(line_from_css("115%"), Some(115));
        assert_eq!(line_from_css("2.4"), Some(200));
        assert_eq!(line_from_css("normal"), None);
    }

    #[test]
    fn reads_colors_as_css_writes_them() {
        assert_eq!(Color::from_css("#1A5FB4"), Some(Color([0x1a, 0x5f, 0xb4])));
        assert_eq!(Color::from_css("#f00"), Some(Color([255, 0, 0])));
        assert_eq!(Color::from_css("rgb(255, 0, 0)"), Some(Color([255, 0, 0])));
        assert_eq!(
            Color::from_css("rgb(255 0 0 / 50%)"),
            Some(Color([255, 0, 0]))
        );
        assert_eq!(Color::from_css("rgba(0,0,0,0)"), None);
        assert_eq!(Color::from_css("transparent"), None);
        assert_eq!(Color::from_css("#ffff0000"), None);
        assert_eq!(Color::from_css("Yellow"), Some(Color([255, 255, 0])));
        assert_eq!(Color::from_css("#12345"), None);
        assert_eq!(Color::from_css("url(x)"), None);
        assert_eq!(Color([0x1a, 0x5f, 0xb4]).css(), "#1a5fb4");
        assert_eq!(Color::from_hex("#1a5fb4"), Some(Color([0x1a, 0x5f, 0xb4])));
        assert_eq!(Color::from_hex("red"), None);
    }

    #[test]
    fn reads_sizes_in_points() {
        assert_eq!(size_from_css("12pt", None), Some(24));
        assert_eq!(size_from_css("16px", None), Some(24));
        assert_eq!(size_from_css("10.5pt", None), Some(21));
        assert_eq!(size_from_css("2em", Some(24)), Some(48));
        assert_eq!(size_from_css("150%", None), Some(33));
        assert_eq!(size_from_css("large", None), Some(27));
        assert_eq!(size_from_css("0pt", None), None);
        assert_eq!(size_from_css("9999pt", None), None);
        assert_eq!(size_from_css("calc(1px)", None), None);
        assert_eq!(size_from_font_tag("5"), Some(36));
        assert_eq!(points_text(21), "10.5");
        assert_eq!(points_text(22), "11");
    }

    #[test]
    fn font_names_stay_safe() {
        let font = |text: &str| font_from_css(text).map(|resolved| resolved.font.name);
        assert_eq!(font("\"Times New Roman\", serif"), Some("Tinos"));
        assert_eq!(font("Arial"), Some("Arimo"));
        assert_eq!(font("sans-serif"), Some("Arimo"));
        assert_eq!(font("url(x)"), None);
        assert_eq!(font("'a';color:red"), None);
        assert_eq!(font_name("x\"}body{"), None);
        assert_eq!(font_name("  Open   Sans "), Some("Open Sans".into()));
    }

    #[test]
    fn reads_declarations() {
        assert_eq!(
            declarations("color: red ; font-size:12pt !important;bad"),
            vec![
                ("color".to_owned(), "red".to_owned()),
                ("font-size".to_owned(), "12pt".to_owned()),
            ]
        );
    }
}
