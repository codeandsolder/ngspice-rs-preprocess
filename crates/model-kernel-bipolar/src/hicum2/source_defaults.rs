//! Complete, typed nominal HICUM/L2 2.4.0 Verilog-A source defaults.
//!
//! Reference: VA-Models hicum2/vacode/hicumL2V2p4p0.va, parameter
//! declarations (136 source coefficients). This factory populates the
//! physical model once, without a string-key map at Newton evaluation.
//! Effective-temperature and physical-capacitor partitioning are performed
//! by the normal nominal-to-prepared path.

use super::{Parameters as Diodes, Polarity, junction, noise, temperature, transport};

impl temperature::Nominal {
    /// Exact HICUM/L2 2.4.0 Verilog-A nominal defaults. Source FLcomp=0
    /// intentionally selects its historical 2.1-compatible equations.
    /// Polarity is initially NPN; callers may select PNP before binding.
    #[must_use = "use the initialized parameter set when binding model overrides"]
    pub const fn source_defaults() -> Self {
        Self {
            polarity: Polarity::Npn,
            transport: source_transport(),
            diodes: source_diodes(),
            coefficients: source_coefficients(),
            auxiliary: source_auxiliary(),
            noise: source_noise(),
        }
    }
}

impl Default for temperature::Nominal {
    fn default() -> Self {
        Self::source_defaults()
    }
}

const fn source_transport() -> transport::Parameters {
    transport::Parameters {
        junction: source_junction(),
        c10: 2e-30,
        qp0: 2e-14,
        ich: 0.0,
        mcf: 1.0,
        hjei: 1.0,
        ahjei_nominal: 0.0,
        ahjei: 0.0,
        hjei_zero_bias: 1.0,
        rhjei: 1.0,
        hjci: 1.0,
        hf0: 1.0,
        hfe: 1.0,
        hfc: 1.0,
        t0: 0.0,
        dt0h: 0.0,
        tbvl: 0.0,
        tef0: 0.0,
        gtfe: 1.0,
        thcs: 0.0,
        ahc: 0.1,
        fthc: 0.0,
        rci0: 150.0,
        vlim: 0.5,
        vces: 0.1,
        vpt: 100.0,
        aick: 1e-3,
        delck: 2.0,
        tr: 0.0,
        tbhrec: 0.0,
        vcbar: 0.0,
        icbar: 0.0,
        acbar: 0.01,
        latb: 0.0,
        latl: 0.0,
        flcomp: 0.0,
    }
}

const fn source_diodes() -> Diodes {
    Diodes {
        ibeis: 1e-18,
        mbei: 1.0,
        ireis: 0.0,
        mrei: 2.0,
        ibeps: 0.0,
        mbep: 1.0,
        ireps: 0.0,
        mrep: 2.0,
        ibcis: 1e-16,
        mbci: 1.0,
        ibcxs: 0.0,
        mbcx: 1.0,
        iscs: 0.0,
        msc: 1.0,
    }
}

const fn source_coefficients() -> temperature::ThermalCoefficients {
    temperature::ThermalCoefficients {
        tnom_celsius: 27.0,
        dt: 0.0,
        vgb: 1.17,
        vge: 1.17,
        vgc: 1.17,
        vgs: 1.17,
        f1vg: -1.02377e-4,
        f2vg: 4.3215e-4,
        zetact: 3.0,
        zetabet: 3.5,
        zetaci: 0.0,
        zetacx: 1.0,
        zetarbi: 0.0,
        zetarbx: 0.0,
        zetarcx: 0.0,
        zetare: 0.0,
        alb: 0.0,
        alvs: 0.0,
        alces: 0.0,
        alt0: 0.0,
        kt0: 0.0,
        zetahjei: 1.0,
        zetavgbe: 1.0,
        dvgbe: 0.0,
        alfav: 0.0,
        alqav: 0.0,
        alkav: 0.0,
        zetarth: 0.0,
        alrth: 0.0,
    }
}

const fn source_auxiliary() -> temperature::AuxiliaryCoefficients {
    temperature::AuxiliaryCoefficients {
        itss: 0.0,
        msf: 1.0,
        tsf: 0.0,
        rbi0: 0.0,
        fdqr0: 0.0,
        fgeo: 0.6557,
        fqi: 1.0,
        fcrbi: 0.0,
        flnqs: false,
        rbx: 0.0,
        rcx: 0.0,
        re: 0.0,
        rsu: 0.0,
        csu: 0.0,
        cbepar: 0.0,
        fbepar: 1.0,
        cbcpar: 0.0,
        fbcpar: 0.0,
        favl: 0.0,
        qavl: 0.0,
        kavl: 0.0,
        ibets: 0.0,
        abet: 40.0,
        tunode: 1,
        rth: 0.0,
        flsh: 0,
        cth: 0.0,
    }
}

const fn source_noise() -> noise::Parameters {
    noise::Parameters {
        kf: 0.0,
        af: 2.0,
        flicker_location: noise::FlickerLocation::Intrinsic,
        kfre: 0.0,
        afre: 2.0,
        correlated: false,
        alit: 0.333,
        alqf: 0.167,
    }
}

const fn source_junction() -> junction::Parameters {
    junction::Parameters {
        intrinsic_emitter: junction::Forward {
            zero_bias_capacitance: 1e-20,
            potential: 0.9,
            grading: 0.5,
            max_cap_ratio: 2.5,
        },
        peripheral_emitter: junction::Forward {
            zero_bias_capacitance: 1e-20,
            potential: 0.9,
            grading: 0.5,
            max_cap_ratio: 2.5,
        },
        intrinsic_collector: junction::Collector {
            zero_bias_capacitance: 1e-20,
            potential: 0.7,
            grading: 0.4,
            punch_through: 100.0,
        },
        external_collector: junction::ExternalCollector {
            // Two nominal fields contribute their sum as
            // unsplit CJcx0. FBCpar/CBCpar source preparation
            // performs the authoritative split.
            external_zero_bias_capacitance: 1e-20,
            peripheral_zero_bias_capacitance: 0.0,
            potential: 0.7,
            grading: 0.4,
            punch_through: 100.0,
        },
        substrate: junction::Collector {
            zero_bias_capacitance: 0.0,
            potential: 0.6,
            grading: 0.5,
            punch_through: 100.0,
        },
        peripheral_substrate: junction::PeripheralSubstrate {
            zero_bias_capacitance: 0.0,
            potential: 0.6,
            grading: 0.5,
            punch_through: 100.0,
        },
    }
}
