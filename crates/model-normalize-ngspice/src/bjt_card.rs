//! Source-neutral classic Gummel–Poon model identities shared with consumers.
//!
//! The thermal law is immutable and independent of transistor polarity,
//! model parameters, nominal temperature, and instance temperature.

/// Historical thermal convention, NPN model spelling.
pub const GUMMEL_POON_LEGACY_NPN: &str = "gummel_poon_legacy_npn";
/// Historical thermal convention, PNP model spelling.
pub const GUMMEL_POON_LEGACY_PNP: &str = "gummel_poon_legacy_pnp";

/// Classic bipolar transistor polarity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BjtPolarity {
    Npn,
    Pnp,
}

/// Which thermal-voltage convention the source selected for this model.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BjtThermalConvention {
    /// Native ngspice convention.
    Ngspice,
    /// Historical SPICE convention qualified for the classic source family.
    Legacy,
}

/// Explicit native model spelling, without inferring the source's dialect.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClassicBjtFamily {
    pub polarity: BjtPolarity,
    pub thermal: BjtThermalConvention,
}

impl ClassicBjtFamily {
    /// Recognize classic polarity and explicit thermal-law aliases.
    /// LEVEL remains a separate physical implementation selector.
    #[must_use = "the recognized family identifies polarity and thermal law"]
    pub const fn parse(name: &str) -> Option<Self> {
        use BjtPolarity::{Npn, Pnp};
        use BjtThermalConvention::{Legacy, Ngspice};
        let (polarity, thermal) = if name.eq_ignore_ascii_case("NPN") {
            (Npn, Ngspice)
        } else if name.eq_ignore_ascii_case("PNP") {
            (Pnp, Ngspice)
        } else if name.eq_ignore_ascii_case(GUMMEL_POON_LEGACY_NPN) {
            (Npn, Legacy)
        } else if name.eq_ignore_ascii_case(GUMMEL_POON_LEGACY_PNP) {
            (Pnp, Legacy)
        } else {
            return None;
        };
        Some(Self { polarity, thermal })
    }

    /// Canonical source-neutral model spelling for the selected law.
    #[must_use = "the emitted model family selects the physical thermal convention"]
    pub const fn name(self) -> &'static str {
        use BjtPolarity::{Npn, Pnp};
        use BjtThermalConvention::{Legacy, Ngspice};
        match (self.polarity, self.thermal) {
            (Npn, Ngspice) => "NPN",
            (Pnp, Ngspice) => "PNP",
            (Npn, Legacy) => GUMMEL_POON_LEGACY_NPN,
            (Pnp, Legacy) => GUMMEL_POON_LEGACY_PNP,
        }
    }
}
