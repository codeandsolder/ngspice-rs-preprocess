//! BSIMSOI-specific five-coordinate differential and exponent limits.
use super::super::BsimSoiThermalScalar;
use crate::differential::Differential;

/// Scalar result and analytic gradient in [Vgs, Vds, Vbs, Ves, kelvin] order.
pub type BsimSoiChannelScalar = Differential<5>;
pub(super) type Jet = BsimSoiChannelScalar;

impl Differential<5> {
    pub(super) const fn thermal(value: BsimSoiThermalScalar) -> Self {
        Self::coordinate(value.value, 4, value.d_temperature)
    }
    pub(super) fn bounded_exp(self) -> Self {
        if self.value > 100. {
            Self::constant(2.688_117_142e43)
        } else {
            self.floor_exp()
        }
    }
    pub(super) fn floor_exp(self) -> Self {
        if self.value <= -100. {
            Self::constant(3.720_075_976e-44)
        } else {
            self.exp()
        }
    }
    pub(super) fn limited_exp(self) -> Self {
        if self.value > 100. {
            (self - 99.) * 2.688_117_142e43
        } else {
            self.floor_exp()
        }
    }
}
