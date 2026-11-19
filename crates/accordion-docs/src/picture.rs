//! Pictures in documents (decision 30). Each is stored once, in the file's
//! body as a `data:` address, and the editor's document refers to it by
//! the SHA-256 hash of its contents.
//!
//! Only JPEG, PNG, GIF, and WebP pictures are kept, the kinds every
//! browser shows. Their kind and size come from their first bytes, never
//! from what a file claims, and nothing here decodes them: pictures people
//! add are checked and shrunk by core's sandboxed worker, and pictures in a
//! file written elsewhere only ever reach a browser, as an image.

use std::collections::HashMap;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use sha2::{Digest, Sha256};

/// The largest picture a file's body may hold, decoded. Pictures people add
/// are far smaller once shrunk.
pub const MAX_PICTURE: usize = 25 * 1024 * 1024;

/// A picture's contents and what they are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture {
    /// The SHA-256 hash of the contents, in lowercase hex.
    pub hash: String,
    /// `image/jpeg`, `image/png`, `image/gif`, or `image/webp`.
    pub content_type: &'static str,
    pub contents: Arc<[u8]>,
    /// Pixels across and down.
    pub width: u32,
    pub height: u32,
}

/// Pictures by their hash.
pub type Pictures = HashMap<String, Picture>;

impl Picture {
    /// The picture in `contents`, if it's one Docs keeps.
    pub fn read(contents: impl Into<Arc<[u8]>>) -> Option<Picture> {
        let contents: Arc<[u8]> = contents.into();
        if contents.len() > MAX_PICTURE {
            return None;
        }
        let (content_type, (width, height)) = kind_and_size(&contents)?;
        if width == 0 || height == 0 {
            return None;
        }
        Some(Picture {
            hash: hash(&contents),
            content_type,
            contents,
            width,
            height,
        })
    }

    /// The picture in a `data:` address, such as a page's `<img src>`.
    pub fn from_data_address(address: &str) -> Option<Picture> {
        let rest = address.trim().strip_prefix("data:")?;
        let (meta, data) = rest.split_once(',')?;
        if !meta.to_ascii_lowercase().ends_with(";base64") {
            return None;
        }
        // Too long to hold a picture small enough to keep.
        if data.len() > MAX_PICTURE / 3 * 4 + 1024 * 1024 {
            return None;
        }
        let data: String = data.chars().filter(|c| !c.is_ascii_whitespace()).collect();
        Picture::read(STANDARD.decode(data).ok()?)
    }

    /// The picture as a `data:` address, for a page's `<img src>`.
    pub fn data_address(&self) -> String {
        format!(
            "data:{};base64,{}",
            self.content_type,
            STANDARD.encode(&self.contents)
        )
    }

    /// The file name extension for its kind.
    pub fn extension(&self) -> &'static str {
        match self.content_type {
            "image/png" => "png",
            "image/gif" => "gif",
            "image/webp" => "webp",
            _ => "jpg",
        }
    }
}

/// The SHA-256 hash of `contents`, in lowercase hex.
pub fn hash(contents: &[u8]) -> String {
    Sha256::digest(contents)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Whether `text` is a hash as [`hash`] writes it.
pub fn is_hash(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The kind of picture and its size, from the header of each format.
fn kind_and_size(contents: &[u8]) -> Option<(&'static str, (u32, u32))> {
    let be16 = |at: usize| {
        Some(u32::from(u16::from_be_bytes(
            contents.get(at..at + 2)?.try_into().ok()?,
        )))
    };
    let le16 = |at: usize| {
        Some(u32::from(u16::from_le_bytes(
            contents.get(at..at + 2)?.try_into().ok()?,
        )))
    };
    let be32 = |at: usize| {
        Some(u32::from_be_bytes(
            contents.get(at..at + 4)?.try_into().ok()?,
        ))
    };
    let le24 = |at: usize| {
        let bytes = contents.get(at..at + 3)?;
        Some(u32::from(bytes[0]) | u32::from(bytes[1]) << 8 | u32::from(bytes[2]) << 16)
    };
    match contents {
        [
            0x89,
            b'P',
            b'N',
            b'G',
            0x0d,
            0x0a,
            0x1a,
            0x0a,
            _,
            _,
            _,
            _,
            b'I',
            b'H',
            b'D',
            b'R',
            ..,
        ] => Some(("image/png", (be32(16)?, be32(20)?))),
        [b'G', b'I', b'F', b'8', b'7' | b'9', b'a', ..] => {
            Some(("image/gif", (le16(6)?, le16(8)?)))
        }
        [
            b'R',
            b'I',
            b'F',
            b'F',
            _,
            _,
            _,
            _,
            b'W',
            b'E',
            b'B',
            b'P',
            b'V',
            b'P',
            b'8',
            kind,
            ..,
        ] => {
            let size = match kind {
                // Lossy: 14 bits each, after the frame's start code.
                b' ' => (le16(26)? & 0x3fff, le16(28)? & 0x3fff),
                // Lossless: 14 bits each, less one.
                b'L' => {
                    let bits = u32::from_le_bytes(contents.get(21..25)?.try_into().ok()?);
                    ((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1)
                }
                // Extended: 24 bits each, less one.
                b'X' => (le24(24)? + 1, le24(27)? + 1),
                _ => return None,
            };
            Some(("image/webp", size))
        }
        [0xff, 0xd8, 0xff, ..] => {
            // The size is in the first start-of-frame segment.
            let mut at = 2;
            loop {
                if *contents.get(at)? != 0xff {
                    return None;
                }
                let marker = *contents.get(at + 1)?;
                match marker {
                    0xff => at += 1,
                    0xd8 | 0x01 | 0xd0..=0xd7 => at += 2,
                    0xc0..=0xcf if !matches!(marker, 0xc4 | 0xc8 | 0xcc) => {
                        return Some(("image/jpeg", (be16(at + 7)?, be16(at + 5)?)));
                    }
                    0xd9 | 0xda => return None,
                    _ => at += 2 + usize::try_from(be16(at + 2)?).ok()?,
                }
            }
        }
        _ => None,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A PNG's signature and header, enough to read its size.
    pub(crate) fn png(width: u32, height: u32) -> Vec<u8> {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        png.extend(width.to_be_bytes());
        png.extend(height.to_be_bytes());
        png.extend([8, 6, 0, 0, 0]);
        png
    }

    #[test]
    fn knows_each_kind_and_its_size() {
        let picture = Picture::read(png(640, 480)).unwrap();
        assert_eq!(picture.content_type, "image/png");
        assert_eq!((picture.width, picture.height), (640, 480));
        assert!(is_hash(&picture.hash));

        let gif = b"GIF89a\x40\x01\xf0\x00".to_vec();
        assert_eq!(kind_and_size(&gif), Some(("image/gif", (320, 240))));

        // An APP0 segment, then a baseline frame 200 down and 300 across.
        let jpeg = [
            &[0xff, 0xd8, 0xff, 0xe0, 0x00, 0x04, 0x00, 0x00][..],
            &[0xff, 0xc0, 0x00, 0x11, 0x08, 0x00, 0xc8, 0x01, 0x2c][..],
        ]
        .concat();
        assert_eq!(kind_and_size(&jpeg), Some(("image/jpeg", (300, 200))));

        let mut webp = b"RIFF\0\0\0\0WEBPVP8X\x0a\0\0\0\0\0\0\0".to_vec();
        webp.extend([0x3f, 0x01, 0x00, 0xef, 0x00, 0x00]);
        assert_eq!(kind_and_size(&webp), Some(("image/webp", (320, 240))));

        assert_eq!(
            kind_and_size(b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>"),
            None
        );
        assert_eq!(kind_and_size(&[0xff, 0xd8, 0xff]), None);
    }

    #[test]
    fn reads_data_addresses_by_their_contents() {
        let picture = Picture::read(png(2, 1)).unwrap();
        let address = picture.data_address();
        assert!(address.starts_with("data:image/png;base64,"));
        assert_eq!(Picture::from_data_address(&address), Some(picture.clone()));
        // What the address claims doesn't matter: the bytes say PNG.
        let claimed = address.replace("image/png", "image/svg+xml");
        assert_eq!(
            Picture::from_data_address(&claimed).unwrap().content_type,
            "image/png"
        );
        assert_eq!(Picture::from_data_address("data:text/html,<b>hi</b>"), None);
        assert_eq!(
            Picture::from_data_address("https://example.com/a.png"),
            None
        );
    }
}
