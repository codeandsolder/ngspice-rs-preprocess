//! Static ngspice-derived model/instance parameter catalogs.

macro_rules! spec {
    ($name:literal, $slot:literal, $kind:ident) => {
        crate::ParameterSpec {
            name: $name,
            slot: $slot,
            kind: crate::ParameterType::$kind,
        }
    };
}

mod bsim3_soi_fd;
mod bsim3_soi_pd;
mod bsim_soi;
mod hisim2;
mod hisim_hv1;
mod hisim_hv2;
mod soi3;

use crate::{ModelVariant, ParameterScope, ParameterSpec};

pub fn table(variant: ModelVariant, scope: ParameterScope) -> &'static [ParameterSpec] {
    use ModelVariant::{
        Bsim3SoiDd, Bsim3SoiFd, Bsim3SoiPd, BsimSoi, HiSim2, HiSimHv1, HiSimHv2, Soi3,
    };
    use ParameterScope::{Instance, Model};
    match (variant, scope) {
        (HiSim2, Model) => hisim2::MODEL,
        (HiSim2, Instance) => hisim2::INSTANCE,
        (HiSimHv1, Model) => hisim_hv1::MODEL,
        (HiSimHv1 | HiSimHv2, Instance) => hisim_hv1::INSTANCE,
        (HiSimHv2, Model) => hisim_hv2::MODEL,
        (BsimSoi, Model) => bsim_soi::MODEL,
        (BsimSoi, Instance) => bsim_soi::INSTANCE,
        (Bsim3SoiFd | Bsim3SoiDd, Model) => bsim3_soi_fd::MODEL,
        (Bsim3SoiFd | Bsim3SoiDd, Instance) => bsim3_soi_fd::INSTANCE,
        (Bsim3SoiPd, Model) => bsim3_soi_pd::MODEL,
        (Bsim3SoiPd, Instance) => bsim3_soi_pd::INSTANCE,
        (Soi3, Model) => soi3::MODEL,
        (Soi3, Instance) => soi3::INSTANCE,
    }
}
