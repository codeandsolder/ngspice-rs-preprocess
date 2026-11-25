//! HICUM/L2 2.4.0 source-validated preparation-time parameter schema.
//!
//! All 136 published Verilog-A names and bounds are represented here.
//! Parameter lookup and validation happen at model-card ingestion, never
//! during physical Newton evaluation. The parser owns SPICE lexical
//! syntax and engineering suffix interpretation; this layer only owns
//! source identity, numeric semantic ranges and typed preparation.
//! Generated directly from the upstream HICUM/L2 V2.4.0 public VA
//! model parameter declarations; no vendor implementation is embedded.

use super::{Polarity, noise, temperature};
use crate::ModelError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Real,
    Integer,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Specification {
    pub name: &'static str,
    pub kind: Kind,
    pub default: f64,
    pub minimum: f64,
    pub maximum: f64,
    pub include_minimum: bool,
    pub include_maximum: bool,
    pub exclude_zero: bool,
}

impl Specification {
    #[must_use = "the boolean determines whether the source parameter is valid"]
    pub fn accepts(self, value: f64) -> bool {
        value.is_finite()
            && (if self.include_minimum {
                value >= self.minimum
            } else {
                value > self.minimum
            })
            && (if self.include_maximum {
                value <= self.maximum
            } else {
                value < self.maximum
            })
            && (!self.exclude_zero || value != 0.0)
            && (self.kind == Kind::Real || value.fract() == 0.0)
    }
}

/// Dense, immutable model-card parameter identifier. Names are resolved
/// once, then ID/value pairs can be applied directly without name lookups.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Id(u8);

impl Id {
    #[must_use = "the lookup distinguishes unknown names from registered parameter identities"]
    pub fn from_name(name: &str) -> Option<Self> {
        SPECIFICATIONS
            .iter()
            .enumerate()
            .find_map(|(position, spec)| {
                if spec.name.eq_ignore_ascii_case(name) {
                    u8::try_from(position).ok().map(Self)
                } else {
                    None
                }
            })
    }

    /// Stable dense source parameter identity for one-time artifact hashing.
    #[must_use = "the lookup distinguishes invalid indices from registered parameter identities"]
    pub fn at(index: usize) -> Option<Self> {
        u8::try_from(index)
            .ok()
            .filter(|slot| usize::from(*slot) < SPECIFICATIONS.len())
            .map(Self)
    }

    #[must_use = "the returned specification defines parameter validation"]
    pub const fn specification(self) -> Specification {
        SPECIFICATIONS[self.0 as usize]
    }
}

#[derive(Clone, Copy)]
enum Bounds {
    Closed,
    ClosedOpen,
    OpenClosed,
    Open,
}

impl Specification {
    const fn bounded(
        name: &'static str,
        kind: Kind,
        default: f64,
        minimum: f64,
        maximum: f64,
        bounds: Bounds,
    ) -> Self {
        Self {
            name,
            kind,
            default,
            minimum,
            maximum,
            include_minimum: matches!(bounds, Bounds::Closed | Bounds::ClosedOpen),
            include_maximum: matches!(bounds, Bounds::Closed | Bounds::OpenClosed),
            exclude_zero: false,
        }
    }
}

// The catalog is source-ordered data, not repeated validation logic.
// Interval endpoint inclusion remains explicit on every row.
macro_rules! spec {
    ($name:literal, $kind:ident, $default:expr, $min:expr, $max:expr, $bounds:ident) => {
        Specification::bounded($name, Kind::$kind, $default, $min, $max, Bounds::$bounds)
    };
}

pub const SPECIFICATIONS: [Specification; 136] = [
    spec!("c10", Real, 2.0E-30, 0.0, 1.0, Closed),
    spec!("qp0", Real, 2.0E-14, 0.0, 1.0, OpenClosed),
    spec!("ich", Real, 0.0, 0.0, f64::INFINITY, ClosedOpen),
    spec!("hf0", Real, 1.0, 0.0, f64::INFINITY, ClosedOpen),
    spec!("hfe", Real, 1.0, 0.0, f64::INFINITY, ClosedOpen),
    spec!("hfc", Real, 1.0, 0.0, f64::INFINITY, ClosedOpen),
    spec!("hjei", Real, 1.0, 0.0, 100.0, Closed),
    spec!("ahjei", Real, 0.0, 0.0, 100.0, Closed),
    spec!("rhjei", Real, 1.0, 0.0, 10.0, OpenClosed),
    spec!("hjci", Real, 1.0, 0.0, 100.0, Closed),
    spec!("ibeis", Real, 1.0E-18, 0.0, 1.0, OpenClosed),
    spec!("mbei", Real, 1.0, 0.0, 10.0, OpenClosed),
    spec!("ireis", Real, 0.0, 0.0, 1.0, Closed),
    spec!("mrei", Real, 2.0, 0.0, 10.0, OpenClosed),
    spec!("ibeps", Real, 0.0, 0.0, 1.0, Closed),
    spec!("mbep", Real, 1.0, 0.0, 10.0, OpenClosed),
    spec!("ireps", Real, 0.0, 0.0, 1.0, Closed),
    spec!("mrep", Real, 2.0, 0.0, 10.0, OpenClosed),
    spec!("mcf", Real, 1.0, 0.0, 10.0, OpenClosed),
    spec!("tbhrec", Real, 0.0, 0.0, f64::INFINITY, ClosedOpen),
    spec!("ibcis", Real, 1.0E-16, 0.0, 1.0, OpenClosed),
    spec!("mbci", Real, 1.0, 0.0, 10.0, OpenClosed),
    spec!("ibcxs", Real, 0.0, 0.0, 1.0, Closed),
    spec!("mbcx", Real, 1.0, 0.0, 10.0, OpenClosed),
    spec!("ibets", Real, 0.0, 0.0, 50.0, Closed),
    spec!("abet", Real, 40.0, 0.0, f64::INFINITY, ClosedOpen),
    spec!("tunode", Integer, 1.0, 0.0, 1.0, Closed),
    spec!("favl", Real, 0.0, 0.0, f64::INFINITY, ClosedOpen),
    spec!("qavl", Real, 0.0, 0.0, f64::INFINITY, ClosedOpen),
    spec!("kavl", Real, 0.0, 0.0, 3.0, Closed),
    spec!("alfav", Real, 0.0, f64::NEG_INFINITY, f64::INFINITY, Closed),
    spec!("alqav", Real, 0.0, f64::NEG_INFINITY, f64::INFINITY, Closed),
    spec!("alkav", Real, 0.0, f64::NEG_INFINITY, f64::INFINITY, Closed),
    spec!("rbi0", Real, 0.0, 0.0, f64::INFINITY, ClosedOpen),
    spec!("rbx", Real, 0.0, 0.0, f64::INFINITY, ClosedOpen),
    spec!("fgeo", Real, 0.6557, 0.0, f64::INFINITY, ClosedOpen),
    spec!("fdqr0", Real, 0.0, -0.5, 100.0, Closed),
    spec!("fcrbi", Real, 0.0, 0.0, 1.0, Closed),
    spec!("fqi", Real, 1.0, 0.0, 1.0, Closed),
    spec!("re", Real, 0.0, 0.0, f64::INFINITY, ClosedOpen),
    spec!("rcx", Real, 0.0, 0.0, f64::INFINITY, ClosedOpen),
    spec!("itss", Real, 0.0, 0.0, 1.0, Closed),
    spec!("msf", Real, 1.0, 0.0, 10.0, OpenClosed),
    spec!("iscs", Real, 0.0, 0.0, 1.0, Closed),
    spec!("msc", Real, 1.0, 0.0, 10.0, OpenClosed),
    spec!("tsf", Real, 0.0, 0.0, f64::INFINITY, ClosedOpen),
    spec!("rsu", Real, 0.0, 0.0, f64::INFINITY, ClosedOpen),
    spec!("csu", Real, 0.0, 0.0, f64::INFINITY, ClosedOpen),
    spec!("cjei0", Real, 1.0E-20, 0.0, f64::INFINITY, ClosedOpen),
    spec!("vdei", Real, 0.9, 0.0, 10.0, OpenClosed),
    spec!("zei", Real, 0.5, 0.0, 1.0, Open),
    spec!("ajei", Real, 2.5, 1.0, f64::INFINITY, ClosedOpen),
    spec!("cjep0", Real, 1.0E-20, 0.0, f64::INFINITY, ClosedOpen),
    spec!("vdep", Real, 0.9, 0.0, 10.0, OpenClosed),
    spec!("zep", Real, 0.5, 0.0, 1.0, Open),
    spec!("ajep", Real, 2.5, 1.0, f64::INFINITY, ClosedOpen),
    spec!("cjci0", Real, 1.0E-20, 0.0, f64::INFINITY, ClosedOpen),
    spec!("vdci", Real, 0.7, 0.0, 10.0, OpenClosed),
    spec!("zci", Real, 0.4, 0.0, 1.0, Open),
    spec!("vptci", Real, 100.0, 0.0, 100.0, OpenClosed),
    spec!("cjcx0", Real, 1.0E-20, 0.0, f64::INFINITY, ClosedOpen),
    spec!("vdcx", Real, 0.7, 0.0, 10.0, OpenClosed),
    spec!("zcx", Real, 0.4, 0.0, 1.0, Open),
    spec!("vptcx", Real, 100.0, 0.0, 100.0, OpenClosed),
    spec!("fbcpar", Real, 0.0, 0.0, 1.0, Closed),
    spec!("fbepar", Real, 1.0, 0.0, 1.0, Closed),
    spec!("cjs0", Real, 0.0, 0.0, f64::INFINITY, ClosedOpen),
    spec!("vds", Real, 0.6, 0.0, 10.0, OpenClosed),
    spec!("zs", Real, 0.5, 0.0, 1.0, Open),
    spec!("vpts", Real, 100.0, 0.0, 100.0, OpenClosed),
    spec!("cscp0", Real, 0.0, 0.0, f64::INFINITY, ClosedOpen),
    spec!("vdsp", Real, 0.6, 0.0, 10.0, Closed),
    spec!("zsp", Real, 0.5, 0.0, 1.0, Open),
    spec!("vptsp", Real, 100.0, 0.0, 100.0, OpenClosed),
    spec!("t0", Real, 0.0, 0.0, f64::INFINITY, ClosedOpen),
    spec!("dt0h", Real, 0.0, f64::NEG_INFINITY, f64::INFINITY, Open),
    spec!("tbvl", Real, 0.0, f64::NEG_INFINITY, f64::INFINITY, Open),
    spec!("tef0", Real, 0.0, 0.0, f64::INFINITY, ClosedOpen),
    spec!("gtfe", Real, 1.0, 0.0, 10.0, OpenClosed),
    spec!("thcs", Real, 0.0, 0.0, f64::INFINITY, ClosedOpen),
    spec!("ahc", Real, 0.1, 0.0, 50.0, OpenClosed),
    spec!("fthc", Real, 0.0, 0.0, 1.0, Closed),
    spec!("rci0", Real, 150.0, 0.0, f64::INFINITY, Open),
    spec!("vlim", Real, 0.5, 0.0, 10.0, OpenClosed),
    spec!("vces", Real, 0.1, 0.0, 1.0, Closed),
    spec!("vpt", Real, 100.0, 0.0, f64::INFINITY, Open),
    spec!("aick", Real, 1e-3, 0.0, 10.0, OpenClosed),
    spec!("delck", Real, 2.0, 0.0, 10.0, OpenClosed),
    spec!("tr", Real, 0.0, 0.0, f64::INFINITY, ClosedOpen),
    spec!("vcbar", Real, 0.0, 0.0, 1.0, Closed),
    spec!("icbar", Real, 0.0, 0.0, 1.0, Closed),
    spec!("acbar", Real, 0.01, 0.0, 10.0, OpenClosed),
    spec!("cbepar", Real, 0.0, 0.0, f64::INFINITY, ClosedOpen),
    spec!("cbcpar", Real, 0.0, 0.0, f64::INFINITY, ClosedOpen),
    spec!("alqf", Real, 0.167, 0.0, 1.0, Closed),
    spec!("alit", Real, 0.333, 0.0, 1.0, Closed),
    spec!("flnqs", Integer, 0.0, 0.0, 1.0, Closed),
    spec!("kf", Real, 0.0, 0.0, f64::INFINITY, ClosedOpen),
    spec!("af", Real, 2.0, 0.0, 10.0, OpenClosed),
    spec!("cfbe", Integer, -1.0, -2.0, -1.0, Closed),
    spec!("flcono", Integer, 0.0, 0.0, 1.0, Closed),
    spec!("kfre", Real, 0.0, 0.0, f64::INFINITY, ClosedOpen),
    spec!("afre", Real, 2.0, 0.0, 10.0, OpenClosed),
    spec!("latb", Real, 0.0, 0.0, f64::INFINITY, ClosedOpen),
    spec!("latl", Real, 0.0, 0.0, f64::INFINITY, ClosedOpen),
    spec!("vgb", Real, 1.17, 0.0, 10.0, OpenClosed),
    spec!("alt0", Real, 0.0, f64::NEG_INFINITY, f64::INFINITY, Closed),
    spec!("kt0", Real, 0.0, f64::NEG_INFINITY, f64::INFINITY, Closed),
    spec!("zetaci", Real, 0.0, -10.0, 10.0, Closed),
    spec!("alvs", Real, 0.0, f64::NEG_INFINITY, f64::INFINITY, Closed),
    spec!("alces", Real, 0.0, f64::NEG_INFINITY, f64::INFINITY, Closed),
    spec!("zetarbi", Real, 0.0, -10.0, 10.0, Closed),
    spec!("zetarbx", Real, 0.0, -10.0, 10.0, Closed),
    spec!("zetarcx", Real, 0.0, -10.0, 10.0, Closed),
    spec!("zetare", Real, 0.0, -10.0, 10.0, Closed),
    spec!("zetacx", Real, 1.0, -10.0, 10.0, Closed),
    spec!("vge", Real, 1.17, 0.0, 10.0, OpenClosed),
    spec!("vgc", Real, 1.17, 0.0, 10.0, OpenClosed),
    spec!("vgs", Real, 1.17, 0.0, 10.0, OpenClosed),
    spec!(
        "f1vg",
        Real,
        -1.02377e-4,
        f64::NEG_INFINITY,
        f64::INFINITY,
        Closed
    ),
    spec!(
        "f2vg",
        Real,
        4.3215e-4,
        f64::NEG_INFINITY,
        f64::INFINITY,
        Closed
    ),
    spec!("zetact", Real, 3.0, -10.0, 10.0, Closed),
    spec!("zetabet", Real, 3.5, -10.0, 10.0, Closed),
    spec!("alb", Real, 0.0, f64::NEG_INFINITY, f64::INFINITY, Closed),
    spec!("dvgbe", Real, 0.0, -10.0, 10.0, Closed),
    spec!("zetahjei", Real, 1.0, -10.0, 10.0, Closed),
    spec!("zetavgbe", Real, 1.0, -10.0, 10.0, Closed),
    spec!("flsh", Integer, 0.0, 0.0, 2.0, Closed),
    spec!("rth", Real, 0.0, 0.0, f64::INFINITY, ClosedOpen),
    spec!("zetarth", Real, 0.0, -10.0, 10.0, Closed),
    spec!("alrth", Real, 0.0, -10.0, 10.0, Closed),
    spec!("cth", Real, 0.0, 0.0, f64::INFINITY, ClosedOpen),
    spec!("flcomp", Real, 0.0, 0.0, f64::INFINITY, ClosedOpen),
    spec!("tnom", Real, 27.0, f64::NEG_INFINITY, f64::INFINITY, Closed),
    spec!("dt", Real, 0.0, f64::NEG_INFINITY, f64::INFINITY, Closed),
    Specification {
        exclude_zero: true,
        ..spec!("type", Integer, 1.0, -1.0, 1.0, Closed)
    },
];

impl temperature::Nominal {
    /// Apply a resolved source parameter, validating its original VA
    /// numeric domain before modifying the typed nominal family.
    ///
    /// # Errors
    /// Reports the named parameter for an invalid integer, range,
    /// excluded value, NaN or infinity.
    pub fn assign_parameter(&mut self, id: Id, value: f64) -> Result<(), ModelError> {
        let spec = id.specification();
        if !spec.accepts(value) {
            return Err(ModelError::InvalidParameter(spec.name));
        }
        match id.0 {
            0..=15 => self.assign_group_0(id.0, value),
            16..=31 => self.assign_group_1(id.0, value),
            32..=47 => self.assign_group_2(id.0, value),
            48..=63 => self.assign_group_3(id.0, value),
            64..=79 => self.assign_group_4(id.0, value),
            80..=95 => self.assign_group_5(id.0, value),
            96..=111 => self.assign_group_6(id.0, value),
            112..=127 => self.assign_group_7(id.0, value),
            128..=135 => self.assign_group_8(id.0, value),
            _ => Err(ModelError::InvalidParameter("parameter_slot")),
        }
    }

    const fn assign_group_0(&mut self, slot: u8, value: f64) -> Result<(), ModelError> {
        match slot {
            0 => {
                self.transport.c10 = value;
            }
            1 => {
                self.transport.qp0 = value;
            }
            2 => {
                self.transport.ich = value;
            }
            3 => {
                self.transport.hf0 = value;
            }
            4 => {
                self.transport.hfe = value;
            }
            5 => {
                self.transport.hfc = value;
            }
            6 => {
                self.transport.hjei = value;
                self.transport.hjei_zero_bias = value;
            }
            7 => {
                self.transport.ahjei_nominal = value;
                self.transport.ahjei = value;
            }
            8 => {
                self.transport.rhjei = value;
            }
            9 => {
                self.transport.hjci = value;
            }
            10 => {
                self.diodes.ibeis = value;
            }
            11 => {
                self.diodes.mbei = value;
            }
            12 => {
                self.diodes.ireis = value;
            }
            13 => {
                self.diodes.mrei = value;
            }
            14 => {
                self.diodes.ibeps = value;
            }
            15 => {
                self.diodes.mbep = value;
            }
            _ => return Err(ModelError::InvalidParameter("parameter_slot")),
        }
        Ok(())
    }

    fn assign_group_1(&mut self, slot: u8, value: f64) -> Result<(), ModelError> {
        match slot {
            16 => {
                self.diodes.ireps = value;
            }
            17 => {
                self.diodes.mrep = value;
            }
            18 => {
                self.transport.mcf = value;
            }
            19 => {
                self.transport.tbhrec = value;
            }
            20 => {
                self.diodes.ibcis = value;
            }
            21 => {
                self.diodes.mbci = value;
            }
            22 => {
                self.diodes.ibcxs = value;
            }
            23 => {
                self.diodes.mbcx = value;
            }
            24 => {
                self.auxiliary.ibets = value;
            }
            25 => {
                self.auxiliary.abet = value;
            }
            26 => {
                self.auxiliary.tunode = u8::from(value > 0.0);
            }
            27 => {
                self.auxiliary.favl = value;
            }
            28 => {
                self.auxiliary.qavl = value;
            }
            29 => {
                self.auxiliary.kavl = value;
            }
            30 => {
                self.coefficients.alfav = value;
            }
            31 => {
                self.coefficients.alqav = value;
            }
            _ => return Err(ModelError::InvalidParameter("parameter_slot")),
        }
        Ok(())
    }

    const fn assign_group_2(&mut self, slot: u8, value: f64) -> Result<(), ModelError> {
        match slot {
            32 => {
                self.coefficients.alkav = value;
            }
            33 => {
                self.auxiliary.rbi0 = value;
            }
            34 => {
                self.auxiliary.rbx = value;
            }
            35 => {
                self.auxiliary.fgeo = value;
            }
            36 => {
                self.auxiliary.fdqr0 = value;
            }
            37 => {
                self.auxiliary.fcrbi = value;
            }
            38 => {
                self.auxiliary.fqi = value;
            }
            39 => {
                self.auxiliary.re = value;
            }
            40 => {
                self.auxiliary.rcx = value;
            }
            41 => {
                self.auxiliary.itss = value;
            }
            42 => {
                self.auxiliary.msf = value;
            }
            43 => {
                self.diodes.iscs = value;
            }
            44 => {
                self.diodes.msc = value;
            }
            45 => {
                self.auxiliary.tsf = value;
            }
            46 => {
                self.auxiliary.rsu = value;
            }
            47 => {
                self.auxiliary.csu = value;
            }
            _ => return Err(ModelError::InvalidParameter("parameter_slot")),
        }
        Ok(())
    }

    const fn assign_group_3(&mut self, slot: u8, value: f64) -> Result<(), ModelError> {
        match slot {
            48 => {
                self.transport
                    .junction
                    .intrinsic_emitter
                    .zero_bias_capacitance = value;
            }
            49 => {
                self.transport.junction.intrinsic_emitter.potential = value;
            }
            50 => {
                self.transport.junction.intrinsic_emitter.grading = value;
            }
            51 => {
                self.transport.junction.intrinsic_emitter.max_cap_ratio = value;
            }
            52 => {
                self.transport
                    .junction
                    .peripheral_emitter
                    .zero_bias_capacitance = value;
            }
            53 => {
                self.transport.junction.peripheral_emitter.potential = value;
            }
            54 => {
                self.transport.junction.peripheral_emitter.grading = value;
            }
            55 => {
                self.transport.junction.peripheral_emitter.max_cap_ratio = value;
            }
            56 => {
                self.transport
                    .junction
                    .intrinsic_collector
                    .zero_bias_capacitance = value;
            }
            57 => {
                self.transport.junction.intrinsic_collector.potential = value;
            }
            58 => {
                self.transport.junction.intrinsic_collector.grading = value;
            }
            59 => {
                self.transport.junction.intrinsic_collector.punch_through = value;
            }
            60 => {
                self.transport
                    .junction
                    .external_collector
                    .external_zero_bias_capacitance = value;
                self.transport
                    .junction
                    .external_collector
                    .peripheral_zero_bias_capacitance = 0.0;
            }
            61 => {
                self.transport.junction.external_collector.potential = value;
            }
            62 => {
                self.transport.junction.external_collector.grading = value;
            }
            63 => {
                self.transport.junction.external_collector.punch_through = value;
            }
            _ => return Err(ModelError::InvalidParameter("parameter_slot")),
        }
        Ok(())
    }

    const fn assign_group_4(&mut self, slot: u8, value: f64) -> Result<(), ModelError> {
        match slot {
            64 => {
                self.auxiliary.fbcpar = value;
            }
            65 => {
                self.auxiliary.fbepar = value;
            }
            66 => {
                self.transport.junction.substrate.zero_bias_capacitance = value;
            }
            67 => {
                self.transport.junction.substrate.potential = value;
            }
            68 => {
                self.transport.junction.substrate.grading = value;
            }
            69 => {
                self.transport.junction.substrate.punch_through = value;
            }
            70 => {
                self.transport
                    .junction
                    .peripheral_substrate
                    .zero_bias_capacitance = value;
            }
            71 => {
                self.transport.junction.peripheral_substrate.potential = value;
            }
            72 => {
                self.transport.junction.peripheral_substrate.grading = value;
            }
            73 => {
                self.transport.junction.peripheral_substrate.punch_through = value;
            }
            74 => {
                self.transport.t0 = value;
            }
            75 => {
                self.transport.dt0h = value;
            }
            76 => {
                self.transport.tbvl = value;
            }
            77 => {
                self.transport.tef0 = value;
            }
            78 => {
                self.transport.gtfe = value;
            }
            79 => {
                self.transport.thcs = value;
            }
            _ => return Err(ModelError::InvalidParameter("parameter_slot")),
        }
        Ok(())
    }

    const fn assign_group_5(&mut self, slot: u8, value: f64) -> Result<(), ModelError> {
        match slot {
            80 => {
                self.transport.ahc = value;
            }
            81 => {
                self.transport.fthc = value;
            }
            82 => {
                self.transport.rci0 = value;
            }
            83 => {
                self.transport.vlim = value;
            }
            84 => {
                self.transport.vces = value;
            }
            85 => {
                self.transport.vpt = value;
            }
            86 => {
                self.transport.aick = value;
            }
            87 => {
                self.transport.delck = value;
            }
            88 => {
                self.transport.tr = value;
            }
            89 => {
                self.transport.vcbar = value;
            }
            90 => {
                self.transport.icbar = value;
            }
            91 => {
                self.transport.acbar = value;
            }
            92 => {
                self.auxiliary.cbepar = value;
            }
            93 => {
                self.auxiliary.cbcpar = value;
            }
            94 => {
                self.noise.alqf = value;
            }
            95 => {
                self.noise.alit = value;
            }
            _ => return Err(ModelError::InvalidParameter("parameter_slot")),
        }
        Ok(())
    }

    fn assign_group_6(&mut self, slot: u8, value: f64) -> Result<(), ModelError> {
        match slot {
            96 => {
                self.auxiliary.flnqs = value > 0.0;
            }
            97 => {
                self.noise.kf = value;
            }
            98 => {
                self.noise.af = value;
            }
            99 => {
                self.noise.flicker_location = if value > -1.5 {
                    noise::FlickerLocation::Intrinsic
                } else {
                    noise::FlickerLocation::Peripheral
                };
            }
            100 => {
                self.noise.correlated = value > 0.0;
            }
            101 => {
                self.noise.kfre = value;
            }
            102 => {
                self.noise.afre = value;
            }
            103 => {
                self.transport.latb = value;
            }
            104 => {
                self.transport.latl = value;
            }
            105 => {
                self.coefficients.vgb = value;
            }
            106 => {
                self.coefficients.alt0 = value;
            }
            107 => {
                self.coefficients.kt0 = value;
            }
            108 => {
                self.coefficients.zetaci = value;
            }
            109 => {
                self.coefficients.alvs = value;
            }
            110 => {
                self.coefficients.alces = value;
            }
            111 => {
                self.coefficients.zetarbi = value;
            }
            _ => return Err(ModelError::InvalidParameter("parameter_slot")),
        }
        Ok(())
    }

    fn assign_group_7(&mut self, slot: u8, value: f64) -> Result<(), ModelError> {
        match slot {
            112 => {
                self.coefficients.zetarbx = value;
            }
            113 => {
                self.coefficients.zetarcx = value;
            }
            114 => {
                self.coefficients.zetare = value;
            }
            115 => {
                self.coefficients.zetacx = value;
            }
            116 => {
                self.coefficients.vge = value;
            }
            117 => {
                self.coefficients.vgc = value;
            }
            118 => {
                self.coefficients.vgs = value;
            }
            119 => {
                self.coefficients.f1vg = value;
            }
            120 => {
                self.coefficients.f2vg = value;
            }
            121 => {
                self.coefficients.zetact = value;
            }
            122 => {
                self.coefficients.zetabet = value;
            }
            123 => {
                self.coefficients.alb = value;
            }
            124 => {
                self.coefficients.dvgbe = value;
            }
            125 => {
                self.coefficients.zetahjei = value;
            }
            126 => {
                self.coefficients.zetavgbe = value;
            }
            127 => {
                self.auxiliary.flsh = match value {
                    0.0 => 0,
                    1.0 => 1,
                    _ => 2,
                };
            }
            _ => return Err(ModelError::InvalidParameter("parameter_slot")),
        }
        Ok(())
    }

    fn assign_group_8(&mut self, slot: u8, value: f64) -> Result<(), ModelError> {
        match slot {
            128 => {
                self.auxiliary.rth = value;
            }
            129 => {
                self.coefficients.zetarth = value;
            }
            130 => {
                self.coefficients.alrth = value;
            }
            131 => {
                self.auxiliary.cth = value;
            }
            132 => {
                self.transport.flcomp = value;
            }
            133 => {
                self.coefficients.tnom_celsius = value;
            }
            134 => {
                self.coefficients.dt = value;
            }
            135 => {
                self.polarity = if value > 0.0 {
                    Polarity::Npn
                } else {
                    Polarity::Pnp
                };
            }
            _ => return Err(ModelError::InvalidParameter("parameter_slot")),
        }
        Ok(())
    }

    /// Transactional application: a rejected multi-parameter model card
    /// leaves the original nominal model entirely unchanged.
    ///
    /// # Errors
    /// Returns the first invalid source value without any partial update.
    pub fn assign_parameters(&mut self, values: &[(Id, f64)]) -> Result<(), ModelError> {
        let mut proposed = *self;
        for &(id, value) in values {
            proposed.assign_parameter(id, value)?;
        }
        *self = proposed;
        Ok(())
    }

    /// Read the nominal source model parameter, never its temperature-
    /// adjusted value, with the source's own binary/enum encoding.
    #[must_use = "the returned nominal coefficient is required for source model preparation"]
    pub fn parameter(&self, id: Id) -> f64 {
        match id.0 {
            0..=31 => self.read_group_0(id.0),
            32..=63 => self.read_group_1(id.0),
            64..=95 => self.read_group_2(id.0),
            96..=127 => self.read_group_3(id.0),
            128..=135 => self.read_group_4(id.0),
            _ => f64::NAN,
        }
    }
    fn read_group_0(&self, slot: u8) -> f64 {
        match slot {
            0 => self.transport.c10,
            1 => self.transport.qp0,
            2 => self.transport.ich,
            3 => self.transport.hf0,
            4 => self.transport.hfe,
            5 => self.transport.hfc,
            6 => self.transport.hjei,
            7 => self.transport.ahjei_nominal,
            8 => self.transport.rhjei,
            9 => self.transport.hjci,
            10 => self.diodes.ibeis,
            11 => self.diodes.mbei,
            12 => self.diodes.ireis,
            13 => self.diodes.mrei,
            14 => self.diodes.ibeps,
            15 => self.diodes.mbep,
            16 => self.diodes.ireps,
            17 => self.diodes.mrep,
            18 => self.transport.mcf,
            19 => self.transport.tbhrec,
            20 => self.diodes.ibcis,
            21 => self.diodes.mbci,
            22 => self.diodes.ibcxs,
            23 => self.diodes.mbcx,
            24 => self.auxiliary.ibets,
            25 => self.auxiliary.abet,
            26 => f64::from(self.auxiliary.tunode),
            27 => self.auxiliary.favl,
            28 => self.auxiliary.qavl,
            29 => self.auxiliary.kavl,
            30 => self.coefficients.alfav,
            31 => self.coefficients.alqav,
            _ => f64::NAN,
        }
    }

    fn read_group_1(&self, slot: u8) -> f64 {
        match slot {
            32 => self.coefficients.alkav,
            33 => self.auxiliary.rbi0,
            34 => self.auxiliary.rbx,
            35 => self.auxiliary.fgeo,
            36 => self.auxiliary.fdqr0,
            37 => self.auxiliary.fcrbi,
            38 => self.auxiliary.fqi,
            39 => self.auxiliary.re,
            40 => self.auxiliary.rcx,
            41 => self.auxiliary.itss,
            42 => self.auxiliary.msf,
            43 => self.diodes.iscs,
            44 => self.diodes.msc,
            45 => self.auxiliary.tsf,
            46 => self.auxiliary.rsu,
            47 => self.auxiliary.csu,
            48 => {
                self.transport
                    .junction
                    .intrinsic_emitter
                    .zero_bias_capacitance
            }
            49 => self.transport.junction.intrinsic_emitter.potential,
            50 => self.transport.junction.intrinsic_emitter.grading,
            51 => self.transport.junction.intrinsic_emitter.max_cap_ratio,
            52 => {
                self.transport
                    .junction
                    .peripheral_emitter
                    .zero_bias_capacitance
            }
            53 => self.transport.junction.peripheral_emitter.potential,
            54 => self.transport.junction.peripheral_emitter.grading,
            55 => self.transport.junction.peripheral_emitter.max_cap_ratio,
            56 => {
                self.transport
                    .junction
                    .intrinsic_collector
                    .zero_bias_capacitance
            }
            57 => self.transport.junction.intrinsic_collector.potential,
            58 => self.transport.junction.intrinsic_collector.grading,
            59 => self.transport.junction.intrinsic_collector.punch_through,
            60 => {
                self.transport
                    .junction
                    .external_collector
                    .external_zero_bias_capacitance
                    + self
                        .transport
                        .junction
                        .external_collector
                        .peripheral_zero_bias_capacitance
            }
            61 => self.transport.junction.external_collector.potential,
            62 => self.transport.junction.external_collector.grading,
            63 => self.transport.junction.external_collector.punch_through,
            _ => f64::NAN,
        }
    }

    const fn read_group_2(&self, slot: u8) -> f64 {
        match slot {
            64 => self.auxiliary.fbcpar,
            65 => self.auxiliary.fbepar,
            66 => self.transport.junction.substrate.zero_bias_capacitance,
            67 => self.transport.junction.substrate.potential,
            68 => self.transport.junction.substrate.grading,
            69 => self.transport.junction.substrate.punch_through,
            70 => {
                self.transport
                    .junction
                    .peripheral_substrate
                    .zero_bias_capacitance
            }
            71 => self.transport.junction.peripheral_substrate.potential,
            72 => self.transport.junction.peripheral_substrate.grading,
            73 => self.transport.junction.peripheral_substrate.punch_through,
            74 => self.transport.t0,
            75 => self.transport.dt0h,
            76 => self.transport.tbvl,
            77 => self.transport.tef0,
            78 => self.transport.gtfe,
            79 => self.transport.thcs,
            80 => self.transport.ahc,
            81 => self.transport.fthc,
            82 => self.transport.rci0,
            83 => self.transport.vlim,
            84 => self.transport.vces,
            85 => self.transport.vpt,
            86 => self.transport.aick,
            87 => self.transport.delck,
            88 => self.transport.tr,
            89 => self.transport.vcbar,
            90 => self.transport.icbar,
            91 => self.transport.acbar,
            92 => self.auxiliary.cbepar,
            93 => self.auxiliary.cbcpar,
            94 => self.noise.alqf,
            95 => self.noise.alit,
            _ => f64::NAN,
        }
    }

    fn read_group_3(&self, slot: u8) -> f64 {
        match slot {
            96 => {
                if self.auxiliary.flnqs {
                    1.0
                } else {
                    0.0
                }
            }
            97 => self.noise.kf,
            98 => self.noise.af,
            99 => match self.noise.flicker_location {
                noise::FlickerLocation::Intrinsic => -1.0,
                noise::FlickerLocation::Peripheral => -2.0,
            },
            100 => {
                if self.noise.correlated {
                    1.0
                } else {
                    0.0
                }
            }
            101 => self.noise.kfre,
            102 => self.noise.afre,
            103 => self.transport.latb,
            104 => self.transport.latl,
            105 => self.coefficients.vgb,
            106 => self.coefficients.alt0,
            107 => self.coefficients.kt0,
            108 => self.coefficients.zetaci,
            109 => self.coefficients.alvs,
            110 => self.coefficients.alces,
            111 => self.coefficients.zetarbi,
            112 => self.coefficients.zetarbx,
            113 => self.coefficients.zetarcx,
            114 => self.coefficients.zetare,
            115 => self.coefficients.zetacx,
            116 => self.coefficients.vge,
            117 => self.coefficients.vgc,
            118 => self.coefficients.vgs,
            119 => self.coefficients.f1vg,
            120 => self.coefficients.f2vg,
            121 => self.coefficients.zetact,
            122 => self.coefficients.zetabet,
            123 => self.coefficients.alb,
            124 => self.coefficients.dvgbe,
            125 => self.coefficients.zetahjei,
            126 => self.coefficients.zetavgbe,
            127 => f64::from(self.auxiliary.flsh),
            _ => f64::NAN,
        }
    }

    const fn read_group_4(&self, slot: u8) -> f64 {
        match slot {
            128 => self.auxiliary.rth,
            129 => self.coefficients.zetarth,
            130 => self.coefficients.alrth,
            131 => self.auxiliary.cth,
            132 => self.transport.flcomp,
            133 => self.coefficients.tnom_celsius,
            134 => self.coefficients.dt,
            135 => match self.polarity {
                Polarity::Npn => 1.0,
                Polarity::Pnp => -1.0,
            },
            _ => f64::NAN,
        }
    }
}
