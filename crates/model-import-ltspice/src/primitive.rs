//! Electrical emission rules for `LTspice` primitive prefixes and terminal slots.

use crate::{
    parse::{Attributes, Placement, Symbol},
    topology::Nets,
};
use anyhow::{Context, Result, ensure};

/// The built-in component families for which `LTspice` exports standard models
/// and a reference to its installation library. Library bytes are supplied by
/// the caller, never embedded in the importer.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Standard {
    Diode,
    Bipolar,
    Jfet,
    Mosfet,
}

impl Standard {
    pub const fn library(self) -> &'static str {
        match self {
            Self::Diode => "standard.dio",
            Self::Bipolar => "standard.bjt",
            Self::Jfet => "standard.jft",
            Self::Mosfet => "standard.mos",
        }
    }

    pub const fn defaults(self) -> &'static [&'static str] {
        match self {
            Self::Diode => &[".model D D"],
            Self::Bipolar => &[".model NPN NPN", ".model PNP PNP"],
            Self::Jfet => &[".model NJF NJF", ".model PJF PJF"],
            Self::Mosfet => &[".model NMOS NMOS", ".model PMOS PMOS"],
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Prefix(u8);

impl Prefix {
    pub fn parse(prefix: &str) -> Result<Self> {
        let upper = prefix.to_ascii_uppercase();
        let letter = match upper.as_str() {
            "QN" | "QP" => b'Q',
            "MN" | "MP" => b'M',
            "JN" | "JP" => b'J',
            _ => {
                let [letter] = upper.as_bytes() else {
                    anyhow::bail!("unsupported LTspice symbol prefix {prefix:?}");
                };
                ensure!(
                    letter.is_ascii_alphabetic(),
                    "invalid SPICE prefix {prefix:?}"
                );
                *letter
            }
        };
        Ok(Self(letter))
    }

    pub const fn letter(self) -> u8 {
        self.0
    }

    pub const fn standard(self) -> Option<Standard> {
        match self.0 {
            b'D' => Some(Standard::Diode),
            b'Q' => Some(Standard::Bipolar),
            b'J' => Some(Standard::Jfet),
            b'M' => Some(Standard::Mosfet),
            _ => None,
        }
    }

    pub const fn is_subcircuit(self) -> bool {
        self.0 == b'X'
    }

    pub fn terminals<'a>(
        self,
        placement: &Placement,
        symbol: &Symbol,
        nets: &'a Nets,
    ) -> Result<Vec<&'a str>> {
        if self.0 == b'A' {
            let mut slots = [None; 8];
            for pin in &symbol.pins {
                let order = pin.order.context("A-device pin lacks SpiceOrder")?;
                let slot = slots
                    .get_mut(order.checked_sub(1).context("zero A-device SpiceOrder")?)
                    .context("A-device SpiceOrder exceeds eight terminals")?;
                *slot =
                    nets.attached_name(placement.orientation.place(placement.origin, pin.point))?;
            }
            // An unconnected common terminal is ground. Every unused terminal
            // (including physically present but untouched pins) is tied to it.
            let common = slots[7].unwrap_or("0");
            return Ok(slots
                .into_iter()
                .map(|node| node.unwrap_or(common))
                .collect());
        }
        contiguous(symbol)?;
        let mut nodes = symbol
            .pins
            .iter()
            .map(|pin| nets.name(placement.orientation.place(placement.origin, pin.point)))
            .collect::<Result<Vec<_>>>()?;
        match self.0 {
            b'Q' | b'M' => ensure!(
                (3..=4).contains(&nodes.len()),
                "transistor requires three or four source pins"
            ),
            b'D' => ensure!(nodes.len() == 2, "diode requires two pins"),
            b'J' => ensure!(nodes.len() == 3, "JFET requires three pins"),
            _ => {}
        }
        match self.0 {
            b'Q' if nodes.len() == 3 => nodes.push("0"),
            b'M' if nodes.len() == 3 => nodes.push(nodes[2]),
            _ => {}
        }
        Ok(nodes)
    }
}

/// Block symbols may omit all `SpiceOrder` attributes; the declared order is
/// then their port order. Other ordinary symbols must have contiguous slots.
pub fn contiguous(symbol: &Symbol) -> Result<()> {
    for (index, pin) in symbol.pins.iter().enumerate() {
        ensure!(
            (symbol.block && pin.order.is_none()) || pin.order == Some(index + 1),
            "noncontiguous SpiceOrder for ordinary symbol"
        );
    }
    Ok(())
}

pub struct Binding<'a> {
    pub library: Option<&'a str>,
    pub fields: Vec<&'a str>,
}

fn get<'a>(attrs: &'a Attributes, key: &str) -> Option<&'a str> {
    attrs
        .get(key)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
}

impl<'a> Binding<'a> {
    pub fn new(prefix: Prefix, attrs: &'a Attributes) -> Self {
        let model = get(attrs, "spicemodel");
        let value = get(attrs, "value");
        let value2 = get(attrs, "value2");
        // ModelFile is the explicit library selector for X primitives.
        // Without it, nonempty Value2 denotes a fixed call: SpiceModel is
        // the library path, Value is a display label, and SpiceLine fields
        // do not participate. For other primitives all fields are additive.
        let library = prefix
            .is_subcircuit()
            .then(|| get(attrs, "modelfile"))
            .flatten();
        if prefix.is_subcircuit()
            && library.is_none()
            && let Some(tail) = value2
        {
            return Self {
                library: model,
                fields: vec![tail],
            };
        }
        Self {
            library,
            fields: [
                model,
                value,
                value2,
                get(attrs, "spiceline"),
                get(attrs, "spiceline2"),
            ]
            .into_iter()
            .flatten()
            .collect(),
        }
    }
}
