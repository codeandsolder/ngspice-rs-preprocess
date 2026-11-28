//! Electrical records only; graphical primitives never become wires.

use anyhow::{Context, Result, bail, ensure};
use model_preprocess_command::{Resource, WorkBudget};
use std::collections::BTreeMap;

pub type Attributes = BTreeMap<String, String>;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Point(pub i64, pub i64);

impl Point {
    fn parse(fields: &mut std::str::SplitAsciiWhitespace<'_>) -> Result<Self> {
        // LTspice's signed 32-bit file coordinates leave room for rotation
        // and placement without overflow in the wider internal space.
        let mut next = || -> Result<i64> {
            Ok(i64::from(
                fields
                    .next()
                    .context("missing coordinate")?
                    .parse::<i32>()?,
            ))
        };
        Ok(Self(next()?, next()?))
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Orientation {
    R0,
    R90,
    R180,
    R270,
    M0,
    M90,
    M180,
    M270,
}

impl Orientation {
    fn parse(value: &str) -> Result<Self> {
        Ok(match value {
            "R0" => Self::R0,
            "R90" => Self::R90,
            "R180" => Self::R180,
            "R270" => Self::R270,
            "M0" => Self::M0,
            "M90" => Self::M90,
            "M180" => Self::M180,
            "M270" => Self::M270,
            _ => bail!("invalid symbol orientation {value}"),
        })
    }

    pub const fn place(self, origin: Point, pin: Point) -> Point {
        let Point(x, y) = pin;
        let (x, y) = match self {
            Self::R0 => (x, y),
            Self::R90 => (-y, x),
            Self::R180 => (-x, -y),
            Self::R270 => (y, -x),
            Self::M0 => (-x, y),
            Self::M90 => (y, x),
            Self::M180 => (x, -y),
            Self::M270 => (-y, -x),
        };
        Point(origin.0 + x, origin.1 + y)
    }
}

#[derive(Debug)]
pub struct Placement {
    pub name: String,
    pub origin: Point,
    pub orientation: Orientation,
    pub attributes: Attributes,
    pub line: usize,
}

#[derive(Debug)]
pub struct Label {
    pub point: Point,
    pub name: String,
    pub line: usize,
}

#[derive(Default, Debug)]
pub struct Sheet {
    pub wires: Vec<(Point, Point)>,
    pub labels: Vec<Label>,
    pub ports: BTreeMap<Point, usize>,
    pub placements: Vec<Placement>,
    pub directives: Vec<(usize, String)>,
}

#[derive(Debug)]
pub struct Pin {
    pub point: Point,
    pub name: Option<String>,
    pub order: Option<usize>,
}

#[derive(Debug)]
pub struct Symbol {
    pub block: bool,
    pub attributes: Attributes,
    pub pins: Vec<Pin>,
}

// Legacy Windows symbol libraries use Windows-1252, not ISO-8859-1.
const C1: [char; 32] = [
    '€', '\u{0081}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{008d}', 'Ž',
    '\u{008f}', '\u{0090}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{009d}',
    'ž', 'Ÿ',
];

pub fn decode(bytes: &[u8]) -> Result<String> {
    if let Some(bytes) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        return Ok(std::str::from_utf8(bytes)?.to_owned());
    }
    let endian = if bytes.starts_with(&[0xff, 0xfe]) {
        Some(u16::from_le_bytes as fn([u8; 2]) -> u16)
    } else if bytes.starts_with(&[0xfe, 0xff]) {
        Some(u16::from_be_bytes as fn([u8; 2]) -> u16)
    } else {
        None
    };
    if let Some(decode) = endian {
        let (words, tail) = bytes[2..].as_chunks::<2>();
        ensure!(tail.is_empty(), "truncated UTF-16 schematic");
        return Ok(String::from_utf16(
            &words.iter().copied().map(decode).collect::<Vec<_>>(),
        )?);
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        return Ok(text.to_owned());
    }
    Ok(bytes
        .iter()
        .map(|&byte| {
            if (0x80..=0x9f).contains(&byte) {
                C1[usize::from(byte - 0x80)]
            } else {
                char::from(byte)
            }
        })
        .collect())
}

fn attribute(line: &str) -> Result<(String, String)> {
    let (_, rest) = line
        .split_once(char::is_whitespace)
        .context("missing attribute")?;
    let rest = rest.trim_start();
    let (key, value) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    ensure!(!key.is_empty(), "empty attribute key");
    let key = key.to_ascii_lowercase();
    let value = value.trim();
    let value = if value == "\"\"" { "" } else { value };
    Ok((key, value.to_owned()))
}

fn set_attribute(attributes: &mut Attributes, line: &str) -> Result<()> {
    let (key, value) = attribute(line)?;
    ensure!(
        attributes.insert(key.clone(), value).is_none(),
        "duplicate attribute {key}"
    );
    Ok(())
}

fn no_more(fields: &mut std::str::SplitAsciiWhitespace<'_>) -> Result<()> {
    ensure!(fields.next().is_none(), "unexpected trailing fields");
    Ok(())
}

fn header(fields: &mut std::str::SplitAsciiWhitespace<'_>) -> Result<()> {
    ensure!(
        matches!(fields.next(), Some("4" | "4.1")),
        "unsupported schematic version"
    );
    no_more(fields)
}

fn text_directive(line: &str) -> Result<Option<String>> {
    let mut rest = line;
    for _ in 0..5 {
        rest = rest
            .split_once(char::is_whitespace)
            .context("malformed TEXT record")?
            .1
            .trim_start();
    }
    if let Some(text) = rest.strip_prefix('!') {
        ensure!(!text.trim().is_empty(), "empty SPICE directive");
        return Ok(Some(text.to_owned()));
    }
    Ok(None)
}

fn parse_placement(line: &str, line_number: usize) -> Result<Placement> {
    let rest = line
        .strip_prefix("SYMBOL")
        .context("missing symbol")?
        .trim();
    // The last three fields are always coordinates and orientation;
    // library paths may contain spaces.
    let (rest, angle) = rest
        .rsplit_once(char::is_whitespace)
        .context("missing orientation")?;
    let (rest, y) = rest
        .trim_end()
        .rsplit_once(char::is_whitespace)
        .context("missing y coordinate")?;
    let (name, x) = rest
        .trim_end()
        .rsplit_once(char::is_whitespace)
        .context("missing x coordinate")?;
    let name = name.trim().to_owned();
    ensure!(!name.is_empty(), "missing symbol name");
    let origin = Point(i64::from(x.parse::<i32>()?), i64::from(y.parse::<i32>()?));
    let orientation = Orientation::parse(angle)?;
    Ok(Placement {
        name,
        origin,
        orientation,
        attributes: Attributes::new(),
        line: line_number,
    })
}

impl Sheet {
    pub fn parse(text: &str, budget: &WorkBudget) -> Result<Self> {
        let mut sheet = Self::default();
        let mut version = false;
        let mut sheet_header = false;
        let mut current = None;
        for (index, line) in text.lines().enumerate() {
            budget.charge(Resource::Statements, 1)?;
            let line = line.trim();
            let mut fields = line.split_ascii_whitespace();
            let Some(tag) = fields.next() else { continue };
            let result = (|| -> Result<()> {
                match tag {
                    "Version" => {
                        ensure!(!version, "duplicate Version");
                        header(&mut fields)?;
                        version = true;
                    }
                    "SHEET" => {
                        ensure!(
                            version && !sheet_header,
                            "duplicate or missing sheet header"
                        );
                        ensure!(fields.next() == Some("1"), "unsupported sheet index");
                        Point::parse(&mut fields)?;
                        no_more(&mut fields)?;
                        sheet_header = true;
                    }
                    "WIRE" => {
                        let a = Point::parse(&mut fields)?;
                        let b = Point::parse(&mut fields)?;
                        no_more(&mut fields)?;
                        ensure!(a != b, "zero length wire");
                        sheet.wires.push((a, b));
                        current = None;
                    }
                    "FLAG" => {
                        let point = Point::parse(&mut fields)?;
                        let name = fields.next().context("missing flag name")?.to_owned();
                        no_more(&mut fields)?;
                        sheet.labels.push(Label {
                            point,
                            name,
                            line: index + 1,
                        });
                        current = None;
                    }
                    "IOPIN" => {
                        let point = Point::parse(&mut fields)?;
                        ensure!(
                            matches!(fields.next(), Some("In" | "Out" | "BiDir")),
                            "invalid port direction"
                        );
                        no_more(&mut fields)?;
                        ensure!(
                            sheet.ports.insert(point, index + 1).is_none(),
                            "duplicate port"
                        );
                        current = None;
                    }
                    "SYMBOL" => {
                        let placement = parse_placement(line, index + 1)?;
                        current = Some(sheet.placements.len());
                        sheet.placements.push(placement);
                    }
                    "SYMATTR" => {
                        let placement = sheet
                            .placements
                            .get_mut(current.context("attribute without symbol")?)
                            .context("missing symbol")?;
                        set_attribute(&mut placement.attributes, line)?;
                    }
                    "WINDOW" => {
                        ensure!(current.is_some(), "window without symbol");
                    }
                    "TEXT" => {
                        if let Some(text) = text_directive(line)? {
                            sheet.directives.push((index + 1, text));
                        }
                        current = None;
                    }
                    "LINE" | "RECTANGLE" | "CIRCLE" | "ARC" => {
                        current = None;
                    }
                    _ => bail!("unsupported schematic record {tag}"),
                }
                Ok(())
            })();
            result.with_context(|| format!("schematic line {}", index + 1))?;
        }
        ensure!(
            version && sheet_header,
            "missing schematic Version/SHEET header"
        );
        Ok(sheet)
    }
}

impl Symbol {
    pub fn parse(text: &str, budget: &WorkBudget) -> Result<Self> {
        let mut version = false;
        let mut block = None;
        let mut attributes = Attributes::new();
        let mut pins: Vec<Pin> = Vec::new();
        for (index, line) in text.lines().enumerate() {
            budget.charge(Resource::Statements, 1)?;
            let line = line.trim();
            let mut fields = line.split_ascii_whitespace();
            let Some(tag) = fields.next() else { continue };
            let result = (|| -> Result<()> {
                match tag {
                    "Version" => {
                        ensure!(!version, "duplicate Version");
                        header(&mut fields)?;
                        version = true;
                    }
                    "SymbolType" => {
                        ensure!(block.is_none(), "duplicate SymbolType");
                        block = Some(match fields.next() {
                            Some("CELL") => false,
                            Some("BLOCK") => true,
                            _ => bail!("unsupported symbol type"),
                        });
                        no_more(&mut fields)?;
                    }
                    "SYMATTR" => set_attribute(&mut attributes, line)?,
                    "PIN" => {
                        let point = Point::parse(&mut fields)?;
                        ensure!(
                            fields.next().is_some() && fields.next().is_some(),
                            "malformed PIN record"
                        );
                        no_more(&mut fields)?;
                        pins.push(Pin {
                            point,
                            name: None,
                            order: None,
                        });
                    }
                    "PINATTR" => {
                        let pin = pins.last_mut().context("pin attribute without pin")?;
                        let (key, value) = attribute(line)?;
                        match key.as_str() {
                            "pinname" => {
                                ensure!(pin.name.replace(value).is_none(), "duplicate pin name");
                            }
                            "spiceorder" => {
                                let order = value.parse::<usize>()?;
                                ensure!(
                                    order > 0 && pin.order.replace(order).is_none(),
                                    "invalid pin order"
                                );
                            }
                            _ => bail!("unsupported pin attribute {key}"),
                        }
                    }
                    "WINDOW" | "LINE" | "RECTANGLE" | "CIRCLE" | "ARC" | "TEXT" => {}
                    _ => bail!("unsupported symbol record {tag}"),
                }
                Ok(())
            })();
            result.with_context(|| format!("symbol line {}", index + 1))?;
        }
        ensure!(version, "missing symbol Version header");
        let block = block.context("missing SymbolType")?;
        ensure!(!pins.is_empty(), "symbol has no electrical pins");
        if !block || pins.iter().any(|pin| pin.order.is_some()) {
            pins.sort_unstable_by_key(|pin| pin.order);
            for (index, pin) in pins.iter().enumerate() {
                ensure!(
                    pin.order == Some(index + 1),
                    "missing, duplicate or noncontiguous SpiceOrder"
                );
            }
        }
        Ok(Self {
            block,
            attributes,
            pins,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_eight_orientation_transforms() -> Result<()> {
        for (code, expected) in [
            ("R0", Point(3, 5)),
            ("R90", Point(-5, 3)),
            ("R180", Point(-3, -5)),
            ("R270", Point(5, -3)),
            ("M0", Point(-3, 5)),
            ("M90", Point(5, 3)),
            ("M180", Point(3, -5)),
            ("M270", Point(-5, -3)),
        ] {
            assert_eq!(
                Orientation::parse(code)?.place(Point(100, -100), Point(3, 5)),
                Point(100 + expected.0, -100 + expected.1)
            );
        }
        assert!(Orientation::parse("R45").is_err());
        Ok(())
    }

    #[test]
    fn pin_order_and_legacy_encodings() -> Result<()> {
        let text = "Version 4\nSymbolType CELL\nSYMATTR Prefix R\nSYMATTR Value 1k\nPIN 0 16 NONE 0\nPINATTR SpiceOrder 2\nPIN 0 0 NONE 0\nPINATTR SpiceOrder 1\n";
        let symbol = Symbol::parse(text, &WorkBudget::default())?;
        assert_eq!(symbol.pins[0].point, Point(0, 0));
        assert_eq!(symbol.pins[1].point, Point(0, 16));
        assert!(
            Symbol::parse(
                &text.replace("SpiceOrder 2", "SpiceOrder 1"),
                &WorkBudget::default()
            )
            .is_err()
        );
        assert_eq!(decode(&[0x80, 0xb5])?, "€µ");
        let mut wide = vec![0xff, 0xfe];
        wide.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(decode(&wide)?, text);
        Ok(())
    }
}
