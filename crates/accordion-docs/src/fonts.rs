//! The fonts Docs has, and the ones it uses in place of fonts it doesn't.
//!
//! Docs always names the font it shows, so a document never says Arial
//! while showing another font. A font Docs doesn't have, named in a paste,
//! an opened file, or an outside change to a document's file, becomes one
//! Docs has as it's read ([`resolve`]): one made to the same letter widths,
//! such as Arimo for Arial, else one that looks like it, else one of its
//! kind. The web app keeps the same list, stand-ins, and weights in
//! `src/docs/fonts.ts`, and its editor says which fonts it replaced.

/// A font Docs has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Font {
    /// The font's own name.
    pub name: &'static str,
    pub kind: Kind,
    /// The weights it has, from 100 to 900.
    pub weights: &'static [u16],
}

/// What a font is like, for the families Word and OpenDocument name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    SansSerif,
    Serif,
    Monospace,
    Display,
    Handwriting,
}

const ALL: &[u16] = &[100, 200, 300, 400, 500, 600, 700, 800, 900];
const REGULAR: &[u16] = &[400];
const REGULAR_BOLD: &[u16] = &[400, 700];

const fn font(name: &'static str, kind: Kind, weights: &'static [u16]) -> Font {
    Font {
        name,
        kind,
        weights,
    }
}

use Kind::{Display, Handwriting, Monospace, SansSerif, Serif};

/// Docs' fonts, as the font menu lists them.
pub const FONTS: &[Font] = &[
    font("Alegreya", Serif, &[400, 500, 600, 700, 800, 900]),
    font("Amatic SC", Handwriting, REGULAR_BOLD),
    font("Arimo", SansSerif, &[400, 500, 600, 700]),
    font("Bree Serif", Serif, REGULAR),
    font("Caladea", Serif, REGULAR_BOLD),
    font("Carlito", SansSerif, REGULAR_BOLD),
    font("Caveat", Handwriting, &[400, 500, 600, 700]),
    font("Comfortaa", Display, &[300, 400, 500, 600, 700]),
    font("Comic Neue", Handwriting, &[300, 400, 700]),
    font("Comic Relief", Handwriting, REGULAR_BOLD),
    font("Cousine", Monospace, REGULAR_BOLD),
    font("EB Garamond", Serif, &[400, 500, 600, 700, 800]),
    font("Gelasio", Serif, &[400, 500, 600, 700]),
    font("Inter", SansSerif, ALL),
    font("Lato", SansSerif, &[100, 300, 400, 700, 900]),
    font("Lexend", SansSerif, ALL),
    font("Libre Baskerville", Serif, &[400, 500, 600, 700]),
    font("Lobster", Display, REGULAR),
    font("Lora", Serif, &[400, 500, 600, 700]),
    font("Merriweather", Serif, &[300, 400, 500, 600, 700, 800, 900]),
    font("Montserrat", SansSerif, ALL),
    font("Noto Sans", SansSerif, ALL),
    font("Noto Serif", Serif, ALL),
    font(
        "Nunito",
        SansSerif,
        &[200, 300, 400, 500, 600, 700, 800, 900],
    ),
    font("Open Sans", SansSerif, &[300, 400, 500, 600, 700, 800]),
    font("Oswald", SansSerif, &[200, 300, 400, 500, 600, 700]),
    font("Pacifico", Handwriting, REGULAR),
    font("Permanent Marker", Handwriting, REGULAR),
    font("Pinyon Script", Handwriting, REGULAR),
    font("Playfair Display", Serif, &[400, 500, 600, 700, 800, 900]),
    font("Raleway", SansSerif, ALL),
    font("Roboto", SansSerif, ALL),
    font(
        "Roboto Mono",
        Monospace,
        &[100, 200, 300, 400, 500, 600, 700],
    ),
    font("Roboto Serif", Serif, ALL),
    font(
        "Source Code Pro",
        Monospace,
        &[200, 300, 400, 500, 600, 700, 800, 900],
    ),
    font(
        "Source Sans 3",
        SansSerif,
        &[200, 300, 400, 500, 600, 700, 800, 900],
    ),
    font("Spectral", Serif, &[200, 300, 400, 500, 600, 700, 800]),
    font("Tinos", Serif, REGULAR_BOLD),
    font("Ultra", Serif, REGULAR),
    font("Varela Round", SansSerif, REGULAR),
];

/// Text with no font of its own: Arimo, as Google Docs uses Arial, whose
/// letter widths Arimo has.
pub const DEFAULT: &str = "Arimo";

/// The font for code: Cousine, with Courier New's letter widths.
pub const CODE: &str = "Cousine";

/// The font Docs has by this name, ignoring case.
pub fn named(name: &str) -> Option<&'static Font> {
    FONTS
        .iter()
        .find(|font| font.name.eq_ignore_ascii_case(name))
}

/// Fonts Docs doesn't have, in lowercase, the font Docs uses for each, and
/// whether the two have the same letter widths. The rest only look alike.
const STAND_INS: &[(&str, &str, bool)] = &[
    ("arial", "Arimo", true),
    ("helvetica", "Arimo", true),
    ("liberation sans", "Arimo", true),
    ("calibri", "Carlito", true),
    ("cambria", "Caladea", true),
    ("courier new", "Cousine", true),
    ("courier", "Cousine", true),
    ("liberation mono", "Cousine", true),
    ("georgia", "Gelasio", true),
    ("times new roman", "Tinos", true),
    ("times", "Tinos", true),
    ("liberation serif", "Tinos", true),
    ("comic sans ms", "Comic Relief", true),
    ("comic sans", "Comic Relief", true),
    ("helvetica neue", "Arimo", false),
    ("verdana", "Noto Sans", false),
    ("tahoma", "Noto Sans", false),
    ("segoe ui", "Noto Sans", false),
    ("dejavu sans", "Noto Sans", false),
    ("lucida grande", "Noto Sans", false),
    ("lucida sans unicode", "Noto Sans", false),
    ("trebuchet ms", "Source Sans 3", false),
    ("aptos", "Source Sans 3", false),
    ("impact", "Oswald", false),
    ("proxima nova", "Montserrat", false),
    ("century gothic", "Montserrat", false),
    ("futura", "Montserrat", false),
    ("garamond", "EB Garamond", false),
    ("baskerville", "Libre Baskerville", false),
    ("baskerville old face", "Libre Baskerville", false),
    ("consolas", "Source Code Pro", false),
    ("menlo", "Source Code Pro", false),
    ("monaco", "Source Code Pro", false),
    ("lucida console", "Source Code Pro", false),
    ("dejavu sans mono", "Source Code Pro", false),
];

fn stand_in(name: &str) -> Option<(&'static Font, bool)> {
    let lower = name.to_lowercase();
    STAND_INS
        .iter()
        .find(|(from, _, _)| *from == lower)
        .and_then(|(_, to, same)| Some((named(to)?, *same)))
}

/// Fonts made to the same letter widths as a font Word uses, and that font.
const TWINS: &[(&str, &str)] = &[
    ("Arimo", "Arial"),
    ("Caladea", "Cambria"),
    ("Carlito", "Calibri"),
    ("Comic Relief", "Comic Sans MS"),
    ("Cousine", "Courier New"),
    ("Gelasio", "Georgia"),
    ("Tinos", "Times New Roman"),
];

/// The font Word uses whose letter widths `name` has, such as Calibri for
/// Carlito. Files name it after the font, for programs that don't have the
/// font but have its twin, so lines still break in the same places.
pub fn twin(name: &str) -> Option<&'static str> {
    TWINS
        .iter()
        .find(|(font, _)| font.eq_ignore_ascii_case(name))
        .map(|(_, twin)| *twin)
}

/// CSS's generic families, and the font Docs uses for each, if any.
const GENERIC: &[(&str, Option<&str>)] = &[
    ("serif", Some("Tinos")),
    ("sans-serif", Some("Arimo")),
    ("monospace", Some("Cousine")),
    ("cursive", Some("Caveat")),
    ("fantasy", None),
    ("system-ui", None),
    ("ui-serif", Some("Tinos")),
    ("ui-sans-serif", Some("Arimo")),
    ("ui-monospace", Some("Cousine")),
    ("ui-rounded", None),
    ("math", None),
    ("emoji", None),
    ("fangsong", None),
    ("-apple-system", None),
    ("blinkmacsystemfont", None),
    ("-webkit-standard", None),
    ("inherit", None),
    ("initial", None),
    ("unset", None),
];

fn generic(name: &str) -> Option<Option<&'static str>> {
    let lower = name.to_lowercase();
    GENERIC
        .iter()
        .find(|(each, _)| *each == lower)
        .map(|(_, font)| *font)
}

/// Weights that fonts put in their names, such as "Raleway Light", as Word
/// and older programs name them, longest first so "Extra Bold" isn't read
/// as "Bold".
const WEIGHT_WORDS: &[(&str, u16)] = &[
    ("extra light", 200),
    ("extralight", 200),
    ("extra-light", 200),
    ("ultra light", 200),
    ("ultralight", 200),
    ("ultra-light", 200),
    ("semi bold", 600),
    ("semibold", 600),
    ("semi-bold", 600),
    ("demi bold", 600),
    ("demibold", 600),
    ("demi-bold", 600),
    ("extra bold", 800),
    ("extrabold", 800),
    ("extra-bold", 800),
    ("ultra bold", 800),
    ("ultrabold", 800),
    ("ultra-bold", 800),
    ("hairline", 100),
    ("thin", 100),
    ("light", 300),
    ("regular", 400),
    ("normal", 400),
    ("book", 400),
    ("medium", 500),
    ("black", 900),
    ("heavy", 900),
    ("bold", 700),
];

/// Each weight's word in a font's name, as Word names a font's weights,
/// such as "Raleway Light".
pub fn weight_word(weight: u16) -> Option<&'static str> {
    Some(match weight {
        100 => "Thin",
        200 => "ExtraLight",
        300 => "Light",
        500 => "Medium",
        600 => "SemiBold",
        800 => "ExtraBold",
        900 => "Black",
        _ => return None,
    })
}

/// A name without the weight it ends with, and that weight.
fn split_weight(name: &str) -> (&str, Option<u16>) {
    if named(name).is_some() || stand_in(name).is_some() {
        return (name, None);
    }
    // ASCII only, so the lowercase name's lengths are the name's.
    let lower = name.to_ascii_lowercase();
    for (word, weight) in WEIGHT_WORDS {
        if let Some(base) = lower.strip_suffix(word)
            && base.ends_with(' ')
        {
            return (name[..base.len()].trim_end(), Some(*weight));
        }
    }
    (name, None)
}

/// The weight `font` has closest to `weight`, the lighter of two as close.
pub fn nearest_weight(font: &Font, weight: u16) -> u16 {
    let mut best = font.weights[0];
    for &have in font.weights {
        if have.abs_diff(weight) < best.abs_diff(weight) {
            best = have;
        }
    }
    best
}

/// The weight text shows: its own weight, made bolder by bold, as Google
/// Docs does. Bold makes weights under 400 regular, weights under 700
/// bold, and leaves heavier ones as they are.
pub fn shown_weight(weight: Option<u16>, bold: bool) -> u16 {
    let own = weight.unwrap_or(400);
    match (bold, own) {
        (false, _) | (true, 700..) => own,
        (true, ..400) => 400,
        (true, _) => 700,
    }
}

/// The font Docs uses for a name or a CSS `font-family`, and a weight the
/// name gave, such as 300 for "Raleway Light".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub font: &'static Font,
    pub weight: Option<u16>,
}

/// The font Docs uses for a CSS `font-family`, such as `"Segoe UI",
/// Roboto, sans-serif`, as the web app's `resolveFont` picks it: the first
/// font Docs has, or one made to the same widths as it, then one that
/// looks like the first font named, then one for its generic family, then
/// Arimo. A name with a weight in it is the font in its nearest weight.
pub fn resolve(family: &str) -> Resolved {
    let names: Vec<String> = family
        .split(',')
        .map(|name| {
            let name = name.trim().trim_matches(['"', '\'']);
            name.split_whitespace().collect::<Vec<_>>().join(" ")
        })
        .filter(|name| !name.is_empty())
        .collect();
    let weighted: Vec<(&str, Option<u16>)> = names
        .iter()
        .filter(|name| generic(name).is_none())
        .map(|name| split_weight(name))
        .collect();
    let with = |font: &'static Font, weight: Option<u16>| Resolved {
        font,
        weight: weight
            .map(|weight| nearest_weight(font, weight))
            .filter(|weight| *weight != 400),
    };
    for (base, weight) in &weighted {
        if let Some(font) = named(base) {
            return with(font, *weight);
        }
        if let Some((font, true)) = stand_in(base) {
            return with(font, *weight);
        }
    }
    for (base, weight) in &weighted {
        if let Some((font, _)) = stand_in(base) {
            return with(font, *weight);
        }
    }
    for name in &names {
        if let Some(Some(font)) = generic(name).map(|font| font.and_then(named)) {
            return with(font, None);
        }
    }
    with(named(DEFAULT).expect("Docs has its default font"), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(family: &str) -> (&'static str, Option<u16>) {
        let resolved = resolve(family);
        (resolved.font.name, resolved.weight)
    }

    #[test]
    fn fonts_docs_lacks_get_one_it_has() {
        assert_eq!(name("Arial, sans-serif"), ("Arimo", None));
        assert_eq!(name("\"Times New Roman\", serif"), ("Tinos", None));
        assert_eq!(name("Calibri"), ("Carlito", None));
        assert_eq!(name("'Comic Sans MS'"), ("Comic Relief", None));
        assert_eq!(name("Verdana"), ("Noto Sans", None));
        assert_eq!(name("\"Segoe UI\", Roboto, sans-serif"), ("Roboto", None));
        assert_eq!(name("Wingdings 9, serif"), ("Tinos", None));
        assert_eq!(name("Mystery"), ("Arimo", None));
        assert_eq!(name("monospace"), ("Cousine", None));
        assert_eq!(name("raleway"), ("Raleway", None));
        assert_eq!(name("Source Sans 3"), ("Source Sans 3", None));
    }

    #[test]
    fn weights_in_names_are_weights() {
        assert_eq!(name("Raleway Light"), ("Raleway", Some(300)));
        assert_eq!(name("Montserrat SemiBold"), ("Montserrat", Some(600)));
        assert_eq!(name("Roboto Extra Bold"), ("Roboto", Some(800)));
        assert_eq!(name("Arial Bold"), ("Arimo", Some(700)));
        assert_eq!(name("Calibri Light"), ("Carlito", None));
        assert_eq!(name("Lato ExtraLight"), ("Lato", Some(100)));
    }

    #[test]
    fn bold_makes_weights_bolder() {
        assert_eq!(shown_weight(Some(300), true), 400);
        assert_eq!(shown_weight(Some(500), true), 700);
        assert_eq!(shown_weight(Some(800), true), 800);
        assert_eq!(shown_weight(Some(300), false), 300);
        assert_eq!(shown_weight(None, true), 700);
        assert_eq!(shown_weight(None, false), 400);
    }

    #[test]
    fn stand_ins_are_fonts_docs_has() {
        for (from, to, _) in STAND_INS {
            assert!(named(to).is_some(), "{to}");
            assert!(named(from).is_none(), "{from}");
            assert_eq!(*from, from.to_lowercase());
        }
        for (font, twin) in TWINS {
            assert_eq!(resolve(twin).font.name, *font);
        }
        for font in FONTS {
            assert!(font.weights.contains(&400), "{}", font.name);
            assert!(crate::style::font_name(font.name).as_deref() == Some(font.name));
        }
        let names: Vec<_> = FONTS.iter().map(|font| font.name).collect();
        let mut sorted = names.clone();
        sorted.sort_by_key(|name| name.to_lowercase());
        assert_eq!(names, sorted);
    }
}
