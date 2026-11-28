//! Model-local independent noise sources. A source can drive several
//! coordinates coherently; separate sources add incoherently.

use crate::{Descriptor, EquationKind, Error, Result};

/// Dimensionless or unit-converting complex transfer coefficient into a local
/// residual row. The source PSD times its squared magnitude gives row PSD.
#[derive(Clone, Copy, Debug)]
pub struct Injection {
    pub row: usize,
    pub re: f64,
    pub im: f64,
}

/// One independent source at the requested frequency, with nonnegative PSD.
pub struct Source<'a> {
    pub name: &'a str,
    pub density: f64,
    pub injections: &'a [Injection],
}

/// Consumer-owned destination for borrowed generators.
pub trait Sink {
    /// # Errors
    /// Invalid or unrepresentable source, consumer resource limits.
    fn push(&mut self, source: Source<'_>) -> Result<()>;
}

pub(crate) struct CheckedSink<'a> {
    descriptor: &'a Descriptor,
    sink: &'a mut dyn Sink,
}

impl<'a> CheckedSink<'a> {
    pub(crate) fn new(descriptor: &'a Descriptor, sink: &'a mut dyn Sink) -> Self {
        Self { descriptor, sink }
    }
}

impl Sink for CheckedSink<'_> {
    fn push(&mut self, source: Source<'_>) -> Result<()> {
        if source.name.is_empty()
            || !source.density.is_finite()
            || source.density < 0.
            || source.injections.is_empty()
            || source.injections.iter().any(|term| {
                term.row >= self.descriptor.equations.len()
                    || !term.re.is_finite()
                    || !term.im.is_finite()
            })
        {
            return Err(Error::Domain("invalid noise source".into()));
        }
        let electrical = source
            .injections
            .iter()
            .filter(|term| self.descriptor.equations[term.row].kind == EquationKind::Electrical);
        if !crate::balanced(electrical.clone().map(|term| term.re))
            || !crate::balanced(electrical.map(|term| term.im))
        {
            return Err(Error::Conservation);
        }
        self.sink.push(source)
    }
}
