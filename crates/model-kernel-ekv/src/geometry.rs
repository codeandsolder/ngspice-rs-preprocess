//! Documented EKV compact-device geometry defaults in the `PSpice` dialect.
//! Explicit zero overrides an inferred value, including diffusion geometry.

use model_kernel_api::{Error, Result};

use crate::Geometry;

/// Optional instance values. The parser must preserve supplied-vs-default
/// provenance instead of substituting zero before geometry preparation.
#[derive(Clone, Copy, Debug, Default)]
pub struct Overrides {
    pub drain_area: Option<f64>,
    pub source_area: Option<f64>,
    pub drain_perimeter: Option<f64>,
    pub source_perimeter: Option<f64>,
    /// Drain, gate, source, bulk sheet-resistance squares.
    pub squares: [Option<f64>; 4],
}

/// Default values supplied by the circuit and model declarations.
#[derive(Clone, Copy, Debug, Default)]
pub struct Defaults {
    /// Circuit area defaults, drain then source.
    pub areas: [f64; 2],
    /// Absent HDIF is different from explicitly supplying zero.
    pub hdif: Option<f64>,
    pub gate_sheet_specified: bool,
}

/// Effective per-finger diffusion geometry. Device-level currents and
/// charges scale by M after the per-finger geometry has been resolved.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Diffusions {
    pub areas: [f64; 2],
    pub perimeters: [f64; 2],
    /// Drain, gate, source, bulk squares.
    pub squares: [f64; 4],
}

impl Defaults {
    /// Resolve the documented defaults (`PSpice` Reference Guide, pp. 175,
    /// 180). The drain/source area and perimeter for HDIF depend on the
    /// parity of the number of fingers, not the series multiplier.
    ///
    /// # Errors
    /// Invalid geometry or a non-integral finger count when an HDIF-derived
    /// parity-dependent default is needed.
    pub fn resolve(self, g: Geometry, supplied: Overrides) -> Result<Diffusions> {
        let positive = [g.length, g.width, g.parallel, g.series];
        if positive.iter().any(|x| !x.is_finite() || *x <= 0.) {
            return Err(Error::Domain("invalid EKV drawn geometry".into()));
        }
        let mut area = self.areas;
        let mut perimeter = [0.; 2];
        let mut squares = [0.; 4];
        if let Some(h) = self.hdif {
            if !h.is_finite() || h < 0. {
                return Err(Error::Domain("invalid EKV diffusion length".into()));
            }
            squares[0] = h / g.width;
            squares[2] = squares[0];
            // Fractional M is valid as a circuit multiplier without an
            // inferred physical finger layout. It has no parity.
            let need_layout = [
                supplied.drain_area,
                supplied.source_area,
                supplied.drain_perimeter,
                supplied.source_perimeter,
            ]
            .iter()
            .any(Option::is_none);
            if need_layout {
                if g.parallel < 1. || g.parallel.fract() != 0. {
                    return Err(Error::Domain(
                        "HDIF geometry requires an integral finger count".into(),
                    ));
                }
                let m = g.parallel;
                let w = g.width;
                let edge = 2.0_f64.mul_add(h, w);
                if m < 2. {
                    area = [2. * h * w; 2];
                    perimeter = [2. * edge, 4.0_f64.mul_add(h, w)];
                } else if m % 2. == 0. {
                    area = [h * w, (2. * h / m + h) * w];
                    perimeter = [edge, 2.0_f64.mul_add(edge / m, edge)];
                } else {
                    area = [(h / m + h) * w; 2];
                    perimeter = [edge / m + edge; 2];
                }
            }
        }
        if self.gate_sheet_specified {
            squares[1] = 0.5 * g.width / g.length;
        }
        let resolved = Diffusions {
            areas: [
                supplied.drain_area.unwrap_or(area[0]),
                supplied.source_area.unwrap_or(area[1]),
            ],
            perimeters: [
                supplied.drain_perimeter.unwrap_or(perimeter[0]),
                supplied.source_perimeter.unwrap_or(perimeter[1]),
            ],
            squares: std::array::from_fn(|i| supplied.squares[i].unwrap_or(squares[i])),
        };
        if resolved
            .areas
            .into_iter()
            .chain(resolved.perimeters)
            .chain(resolved.squares)
            .any(|x| !x.is_finite() || x < 0.)
        {
            return Err(Error::Domain("invalid EKV diffusion geometry".into()));
        }
        Ok(resolved)
    }
}
