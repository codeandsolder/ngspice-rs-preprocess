//! Bounded, typed Touchstone 1.0/1.1/2.0/2.1 ASCII codec.
//!
//! Matrices are row-major in their declared single-ended or mixed-mode basis.
//! Network realization, interpolation and circuit stamping belong to the caller.
//! Format reference: <https://ibis.org/touchstone_ver2.1/touchstone_ver2_1.pdf>.
//! Adapted from the BSD-3-Clause solver codec, revision ed05876.
//! Copyright (c) 2026 Jan Równicki / codeandsolder.

/// One rectangular complex coefficient.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Complex {
    /// Real component.
    pub re: f64,
    /// Imaginary component.
    pub im: f64,
}
impl Complex {
    /// The exact zero coefficient.
    pub const ZERO: Self = Self::new(0., 0.);
    /// Construct from rectangular components.
    #[must_use = "constructs a coefficient"]
    pub const fn new(re: f64, im: f64) -> Self {
        Self { re, im }
    }
    /// Construct a purely real coefficient.
    #[must_use = "constructs a coefficient"]
    pub const fn real(re: f64) -> Self {
        Self::new(re, 0.)
    }
    /// Whether both components are finite.
    #[must_use = "reports coefficient validity"]
    pub const fn is_finite(self) -> bool {
        self.re.is_finite() && self.im.is_finite()
    }
    /// Magnitude of the coefficient.
    #[must_use = "computes the magnitude"]
    pub fn magnitude(self) -> f64 {
        self.re.hypot(self.im)
    }
    /// Phase in radians.
    #[must_use = "computes the phase"]
    pub fn phase(self) -> f64 {
        self.im.atan2(self.re)
    }
}

/// A diagnostic from Touchstone import, validation or export.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TouchstoneError {
    /// Source name, or the stage of processing.
    pub path: String,
    /// One-based source line; zero when not tied to a physical line.
    pub line: usize,
    /// Diagnostic detail.
    pub message: String,
}
impl std::fmt::Display for TouchstoneError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Touchstone {}:{}: {}",
            self.path, self.line, self.message
        )
    }
}
impl std::error::Error for TouchstoneError {}
/// Result of a codec operation.
pub type TouchstoneResult<T> = Result<T, TouchstoneError>;

use std::fmt::Write as _;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// The supported official Touchstone file versions.
pub enum Version {
    /// Original single-reference, implicit-port file layout.
    V1_0,
    /// Single-ended format with per-port option-line references.
    V1_1,
    /// Keyworded specification with explicit port numbering.
    V2_0,
    /// Version 2.1 of the keyworded specification.
    V2_1,
}
impl Version {
    /// Whether bracketed v2 section headers are mandatory.
    #[must_use = "identifies the file grammar"]
    pub const fn modern(self) -> bool {
        matches!(self, Self::V2_0 | Self::V2_1)
    }
    const fn label(self) -> &'static str {
        match self {
            Self::V1_0 => "1.0",
            Self::V1_1 => "1.1",
            Self::V2_0 => "2.0",
            Self::V2_1 => "2.1",
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// The native N-port parameter matrix basis.
pub enum Parameter {
    /// Power-wave scattering coefficients.
    S,
    /// Admittance coefficients in siemens.
    Y,
    /// Impedance coefficients in ohms.
    Z,
    /// Two-port hybrid h parameters.
    H,
    /// Two-port inverse-hybrid g parameters.
    G,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Encoding of each complex pair on disk.
pub enum Encoding {
    /// Real followed by imaginary component.
    Ri,
    /// Linear magnitude followed by phase in degrees.
    Ma,
    /// Decibel magnitude followed by phase in degrees.
    Db,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Frequency unit used by the source file.
pub enum Unit {
    /// Hertz.
    Hz,
    /// Kilohertz.
    KHz,
    /// Megahertz.
    MHz,
    /// Gigahertz.
    GHz,
}
impl Unit {
    /// Multiplier converting this unit to hertz.
    #[must_use = "gives the frequency multiplier"]
    pub const fn scale(self) -> f64 {
        match self {
            Self::Hz => 1.,
            Self::KHz => 1e3,
            Self::MHz => 1e6,
            Self::GHz => 1e9,
        }
    }
    const fn label(self) -> &'static str {
        match self {
            Self::Hz => "Hz",
            Self::KHz => "kHz",
            Self::MHz => "MHz",
            Self::GHz => "GHz",
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Storage layout of matrix entries in the file.
pub enum MatrixFormat {
    /// Every matrix coefficient.
    Full,
    /// Lower triangle including diagonal, symmetric by definition.
    Lower,
    /// Upper triangle including diagonal, symmetric by definition.
    Upper,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Explicit order of S12 and S21 in a two-port record.
pub enum TwoPortOrder {
    /// Legacy 11,21,12,22 convention.
    N21BeforeN12,
    /// Row-major 11,12,21,22 convention.
    N12BeforeN21,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// One ordered axis in a mixed-mode Touchstone matrix.
pub enum MixedPort {
    /// Single-ended port index, zero-based in memory.
    Single(usize),
    /// Ordered positive and negative port indices for differential mode.
    Differential(usize, usize),
    /// Ordered positive and negative port indices for common mode.
    Common(usize, usize),
}
impl MixedPort {
    fn label(self) -> String {
        match self {
            Self::Single(p) => format!("S{}", p + 1),
            Self::Differential(a, b) => format!("D{},{}", a + 1, b + 1),
            Self::Common(a, b) => format!("C{},{}", a + 1, b + 1),
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
/// One frequency sample with all N² row-major complex coefficients.
pub struct NetworkPoint {
    /// Sample frequency in physical hertz.
    pub hz: f64,
    /// Response-row/stimulus-column matrix entries.
    pub matrix: Vec<Complex>,
}
#[derive(Clone, Debug, PartialEq)]
/// Two-port noise parameters; resistance is always physical ohms.
pub struct NoisePoint {
    /// Noise sample frequency in hertz.
    pub hz: f64,
    /// Minimum noise figure in decibels.
    pub min_figure_db: f64,
    /// Complex optimum source reflection coefficient.
    pub optimum_reflection: Complex,
    /// Effective noise resistance in ohms.
    pub resistance_ohms: f64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Independent limits for bounded file import and export.
pub struct TouchstoneLimits {
    /// Maximum ASCII input byte count.
    pub source_bytes: usize,
    /// Maximum declared network port count.
    pub ports: usize,
    /// Maximum network or noise frequency samples.
    pub frequencies: usize,
    /// Maximum network matrix and numeric row storage.
    pub numeric_bytes: usize,
}
impl Default for TouchstoneLimits {
    fn default() -> Self {
        Self {
            source_bytes: 256 * 1024 * 1024,
            ports: 256,
            frequencies: 100_000,
            numeric_bytes: 256 * 1024 * 1024,
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
/// Validated, versioned sampled N-port network with optional noise.
pub struct Touchstone {
    /// Source file language version.
    pub version: Version,
    /// Power-wave or voltage/current matrix parameter type.
    pub parameter: Parameter,
    /// Original complex-pair representation.
    pub encoding: Encoding,
    /// Original on-disk frequency unit.
    pub unit: Unit,
    /// Strictly positive real port references, in ohms.
    pub references: Vec<f64>,
    /// Full or symmetric-triangle layout for export.
    pub matrix_format: MatrixFormat,
    /// Recorded two-port permutation.
    pub two_port_order: TwoPortOrder,
    /// Mixed-mode descriptor list, if values are not single-ended.
    pub mixed_mode: Option<Vec<MixedPort>>,
    /// Monotonically increasing physical frequency samples.
    pub points: Vec<NetworkPoint>,
    /// Independent two-port noise frequency grid.
    pub noise: Vec<NoisePoint>,
    /// Preserved freeform v2 information-section lines.
    pub information: Vec<String>,
}
fn error(path: &str, line: usize, text: impl std::fmt::Display) -> TouchstoneError {
    TouchstoneError {
        path: path.to_owned(),
        line,
        message: text.to_string(),
    }
}
fn numeric(path: &str, line: usize, word: &str) -> TouchstoneResult<f64> {
    word.parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .ok_or_else(|| error(path, line, format!("invalid numeric token {word}")))
}
fn positive(path: &str, line: usize, word: &str) -> TouchstoneResult<f64> {
    let value = numeric(path, line, word)?;
    if value <= 0. {
        return Err(error(path, line, "reference must be positive"));
    }
    Ok(value)
}
fn count(path: &str, line: usize, word: &str, limit: usize) -> TouchstoneResult<usize> {
    word.parse::<usize>()
        .ok()
        .filter(|&n| n > 0 && n <= limit)
        .ok_or_else(|| error(path, line, format!("count must be in 1..={limit}")))
}
fn coordinates(n: usize, fmt: MatrixFormat, order: TwoPortOrder) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for row in 0..n {
        for col in 0..n {
            if fmt == MatrixFormat::Lower && col > row || fmt == MatrixFormat::Upper && col < row {
                continue;
            }
            if fmt == MatrixFormat::Full && n == 2 && order == TwoPortOrder::N21BeforeN12 {
                out.push((col, row));
            } else {
                out.push((row, col));
            }
        }
    }
    out
}
/// Version-1 normalization from dimensionless values to physical SI coefficients.
fn legacy_factor(parameter: Parameter, references: &[f64], row: usize, col: usize) -> f64 {
    let first = references[row].sqrt();
    let second = references[col].sqrt();
    match parameter {
        Parameter::S => 1.,
        Parameter::Y => 1. / first / second,
        Parameter::Z => first * second,
        Parameter::H => match (row, col) {
            (0, 0) => references[0],
            (1, 1) => 1. / references[1],
            _ => references[0].sqrt() / references[1].sqrt(),
        },
        Parameter::G => match (row, col) {
            (0, 0) => 1. / references[0],
            (1, 1) => references[1],
            _ => references[1].sqrt() / references[0].sqrt(),
        },
    }
}

fn complex_pair(
    path: &str,
    line: usize,
    a: f64,
    b: f64,
    encoding: Encoding,
) -> TouchstoneResult<Complex> {
    let result = match encoding {
        Encoding::Ri => Complex::new(a, b),
        Encoding::Ma | Encoding::Db => {
            let magnitude = if encoding == Encoding::Ma {
                a
            } else {
                10_f64.powf(a / 20.)
            };
            if magnitude < 0.
                || !magnitude.is_finite()
                || (encoding == Encoding::Db && magnitude == 0.)
            {
                return Err(error(path, line, "invalid magnitude"));
            }
            let angle = b.to_radians();
            Complex::new(magnitude * angle.cos(), magnitude * angle.sin())
        }
    };
    if !result.is_finite() {
        return Err(error(path, line, "non-finite complex coefficient"));
    }
    Ok(result)
}
fn mixed_axes(
    path: &str,
    line: usize,
    words: &[String],
    refs: &[f64],
) -> TouchstoneResult<Vec<MixedPort>> {
    let n = refs.len();
    if words.len() != n {
        return Err(error(
            path,
            line,
            "mixed-mode order must have N descriptors",
        ));
    }
    let mut descriptors = Vec::new();
    let mut singles = vec![false; n];
    let mut pairs = std::collections::BTreeMap::<(usize, usize), u8>::new();
    for word in words {
        let (letter, numbers) = word.split_at(1);
        let ids = numbers
            .split(',')
            .map(|x| count(path, line, x, n).map(|v| v - 1))
            .collect::<TouchstoneResult<Vec<_>>>()?;
        let descriptor = match (letter.to_ascii_uppercase().as_str(), ids.as_slice()) {
            ("S", &[a]) if !singles[a] => {
                singles[a] = true;
                MixedPort::Single(a)
            }
            ("D" | "C", &[a, b]) if a != b && refs[a].to_bits() == refs[b].to_bits() => {
                let bit = if letter.eq_ignore_ascii_case("d") {
                    1
                } else {
                    2
                };
                let status = pairs.entry((a, b)).or_default();
                if *status & bit != 0 {
                    return Err(error(path, line, "duplicate mixed-mode descriptor"));
                }
                *status |= bit;
                if bit == 1 {
                    MixedPort::Differential(a, b)
                } else {
                    MixedPort::Common(a, b)
                }
            }
            _ => {
                return Err(error(
                    path,
                    line,
                    format!("invalid mixed-mode descriptor {word}"),
                ));
            }
        };
        descriptors.push(descriptor);
    }
    if pairs.values().any(|&m| m != 3) {
        return Err(error(path, line, "each D descriptor needs matching C"));
    }
    for (port, &is_single) in singles.iter().enumerate() {
        let occurrences = pairs
            .keys()
            .filter(|&&(a, b)| a == port || b == port)
            .count();
        if usize::from(is_single) + occurrences != 1 {
            return Err(error(
                path,
                line,
                "port occurs in multiple or no mixed-mode groups",
            ));
        }
    }
    Ok(descriptors)
}

/// Bounded output sink. Unlike formatting to a `String` and checking its
/// length afterward, no temporary or final encoded source can ever grow
/// beyond the specified byte budget. All `fmt::Write` calls are fallible.
struct BoundedText {
    bytes: String,
    limit: usize,
    exhausted: bool,
}

impl BoundedText {
    const fn new(limit: usize) -> Self {
        Self {
            bytes: String::new(),
            limit,
            exhausted: false,
        }
    }

    fn push_str(&mut self, text: &str) {
        let _ = std::fmt::Write::write_str(self, text);
    }

    fn push(&mut self, character: char) {
        let _ = std::fmt::Write::write_char(self, character);
    }

    fn finish(self) -> TouchstoneResult<String> {
        if self.exhausted {
            Err(error(
                "output",
                0,
                "encoded Touchstone text exceeds source byte cap",
            ))
        } else {
            Ok(self.bytes)
        }
    }
}

impl std::fmt::Write for BoundedText {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        let remaining = self.limit.saturating_sub(self.bytes.len());
        if self.exhausted || text.len() > remaining {
            self.exhausted = true;
            return Err(std::fmt::Error);
        }
        self.bytes.push_str(text);
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Header,
    Network,
    Noise,
    Information,
    End,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Continuation {
    References,
    Mixed,
}

impl Touchstone {
    /// Parse a complete bounded Touchstone file. Version-1 files require a
    /// .sNp filename to infer port count; v2 declares it inside the file.
    /// # Errors
    /// Unsupported/invalid syntax, failed numeric conversion, violated
    /// resource caps or physically invalid reference impedances.
    #[expect(
        clippy::too_many_lines,
        reason = "single-pass grammar state machine retains ordered line-numbered keyword and record diagnostics"
    )]
    pub fn parse(source: &str, path: &str, limits: TouchstoneLimits) -> TouchstoneResult<Self> {
        if source.len() > limits.source_bytes
            || !source
                .as_bytes()
                .iter()
                .all(|&b| b == b'\t' || b == b'\r' || b == b'\n' || (32..127).contains(&b))
        {
            return Err(error(
                path,
                0,
                "file is too large or contains non-ASCII bytes",
            ));
        }
        let file_ports = std::path::Path::new(path)
            .extension()
            .and_then(|v| v.to_str())
            .and_then(|v| {
                let lower = v.to_ascii_lowercase();
                lower
                    .strip_prefix('s')
                    .and_then(|x| x.strip_suffix('p'))
                    .and_then(|x| x.parse::<usize>().ok())
            });
        let mut result = Self {
            version: Version::V1_0,
            parameter: Parameter::S,
            encoding: Encoding::Ma,
            unit: Unit::GHz,
            references: Vec::new(),
            matrix_format: MatrixFormat::Full,
            two_port_order: TwoPortOrder::N21BeforeN12,
            mixed_mode: None,
            points: Vec::new(),
            noise: Vec::new(),
            information: Vec::new(),
        };
        let mut ports: Option<usize> = None;
        let mut section = Section::Header;
        let mut continuation = None;
        let mut option_seen = false;
        let mut version_seen = false;
        let mut keywords = std::collections::BTreeSet::<String>::new();
        let mut ref_args = Vec::<String>::new();
        let mut option_refs = Vec::<String>::new();
        let mut mixed_args = Vec::<String>::new();
        let mut expected_points = None;
        let mut expected_noise = None;
        let mut pending = Vec::<f64>::new();
        let mut width = None;
        let mut ordering = Vec::new();
        for (index, physical) in source
            .split('\n')
            .flat_map(|line| line.strip_suffix('\r').unwrap_or(line).split('\r'))
            .enumerate()
        {
            let line = index + 1;
            let without_comment = physical
                .split_once('!')
                .map_or(physical, |(part, _)| part)
                .trim();
            if without_comment.is_empty() {
                continue;
            }
            if section == Section::End {
                return Err(error(path, line, "data appears after [End]"));
            }
            if section == Section::Information {
                if without_comment.eq_ignore_ascii_case("[End Information]") {
                    section = Section::Header;
                } else {
                    result.information.push(physical.to_owned());
                }
                continue;
            }
            if without_comment.starts_with('[') {
                if continuation.is_some() {
                    return Err(error(path, line, "incomplete reference or mixed-mode list"));
                }
                let close = without_comment
                    .find(']')
                    .ok_or_else(|| error(path, line, "unclosed keyword"))?;
                let keyword = without_comment[1..close].to_ascii_lowercase();
                let argument = without_comment[close + 1..].trim();
                if !keywords.insert(keyword.clone()) {
                    return Err(error(path, line, format!("duplicate [{keyword}]")));
                }
                match keyword.as_str() {
                    "version" if !option_seen && !version_seen && section == Section::Header => {
                        result.version = match argument {
                            "2.0" => Version::V2_0,
                            "2.1" => Version::V2_1,
                            _ => return Err(error(path, line, "unsupported version")),
                        };
                        version_seen = true;
                    }
                    "number of ports"
                        if result.version.modern() && option_seen && ports.is_none() =>
                    {
                        ports = Some(count(path, line, argument, limits.ports)?);
                    }
                    "number of frequencies"
                        if result.version.modern()
                            && ports.is_some()
                            && section == Section::Header =>
                    {
                        expected_points = Some(count(path, line, argument, limits.frequencies)?);
                    }
                    "number of noise frequencies"
                        if result.version.modern()
                            && ports == Some(2)
                            && section == Section::Header =>
                    {
                        expected_noise = Some(count(path, line, argument, limits.frequencies)?);
                    }
                    "reference"
                        if result.version.modern()
                            && ports.is_some()
                            && section == Section::Header =>
                    {
                        ref_args.extend(
                            argument
                                .split_ascii_whitespace()
                                .take(ports.unwrap_or_default().saturating_add(1))
                                .map(str::to_owned),
                        );
                        continuation = Some(Continuation::References);
                    }
                    "mixed-mode order"
                        if result.version.modern()
                            && ports.is_some()
                            && section == Section::Header =>
                    {
                        mixed_args.extend(
                            argument
                                .split_ascii_whitespace()
                                .take(ports.unwrap_or_default().saturating_add(1))
                                .map(str::to_owned),
                        );
                        continuation = Some(Continuation::Mixed);
                    }
                    "matrix format"
                        if result.version.modern()
                            && ports.is_some()
                            && section == Section::Header =>
                    {
                        result.matrix_format = match argument.to_ascii_lowercase().as_str() {
                            "full" => MatrixFormat::Full,
                            "lower" => MatrixFormat::Lower,
                            "upper" => MatrixFormat::Upper,
                            _ => return Err(error(path, line, "invalid matrix format")),
                        };
                    }
                    "two-port data order"
                        if result.version.modern()
                            && ports == Some(2)
                            && section == Section::Header =>
                    {
                        result.two_port_order = match argument {
                            "21_12" => TwoPortOrder::N21BeforeN12,
                            "12_21" => TwoPortOrder::N12BeforeN21,
                            _ => return Err(error(path, line, "invalid two-port data order")),
                        };
                    }
                    "begin information"
                        if result.version.modern()
                            && ports.is_some()
                            && section == Section::Header
                            && argument.is_empty() =>
                    {
                        section = Section::Information;
                    }
                    "network data"
                        if result.version.modern()
                            && ports.is_some()
                            && section == Section::Header
                            && expected_points.is_some()
                            && argument.is_empty() =>
                    {
                        section = Section::Network;
                    }
                    "noise data"
                        if result.version.modern()
                            && section == Section::Network
                            && pending.is_empty()
                            && expected_noise.is_some()
                            && argument.is_empty() =>
                    {
                        if result.points.len() != expected_points.unwrap_or_default() {
                            return Err(error(path, line, "incomplete network block"));
                        }
                        section = Section::Noise;
                    }
                    "end"
                        if result.version.modern()
                            && matches!(section, Section::Network | Section::Noise)
                            && pending.is_empty()
                            && argument.is_empty() =>
                    {
                        section = Section::End;
                    }
                    _ => {
                        return Err(error(
                            path,
                            line,
                            format!("misplaced or unsupported [{keyword}]"),
                        ));
                    }
                }
                if let Some(field) = continuation {
                    let len = if field == Continuation::References {
                        ref_args.len()
                    } else {
                        mixed_args.len()
                    };
                    if len > ports.unwrap_or_default() {
                        return Err(error(path, line, "too many header arguments"));
                    }
                    if len == ports.unwrap_or_default() {
                        continuation = None;
                    }
                }
                continue;
            }
            if let Some(field) = continuation {
                let target = if field == Continuation::References {
                    &mut ref_args
                } else {
                    &mut mixed_args
                };
                let n = ports.unwrap_or_default();
                target.extend(
                    without_comment
                        .split_ascii_whitespace()
                        .take(n.saturating_sub(target.len()).saturating_add(1))
                        .map(str::to_owned),
                );
                if target.len() > n {
                    return Err(error(path, line, "too many reference/mixed-mode arguments"));
                }
                if target.len() == n {
                    continuation = None;
                }
                continue;
            }
            if let Some(stripped) = without_comment.strip_prefix('#') {
                if option_seen {
                    continue;
                } // Subsequent option lines are ignored by IBIS.
                if section != Section::Header {
                    return Err(error(path, line, "option line after network data"));
                }
                option_seen = true;
                let mut args = stripped.split_ascii_whitespace().peekable();
                while let Some(word) = args.next() {
                    match word.to_ascii_uppercase().as_str() {
                        "HZ" => result.unit = Unit::Hz,
                        "KHZ" => result.unit = Unit::KHz,
                        "MHZ" => result.unit = Unit::MHz,
                        "GHZ" => result.unit = Unit::GHz,
                        "S" => result.parameter = Parameter::S,
                        "Y" => result.parameter = Parameter::Y,
                        "Z" => result.parameter = Parameter::Z,
                        "G" => result.parameter = Parameter::G,
                        "H" => result.parameter = Parameter::H,
                        "RI" => result.encoding = Encoding::Ri,
                        "MA" => result.encoding = Encoding::Ma,
                        "DB" => result.encoding = Encoding::Db,
                        "R" => {
                            if !option_refs.is_empty() {
                                return Err(error(path, line, "duplicate reference option"));
                            }
                            // v1.0 permits option tokens in arbitrary order:
                            // `# S R 100 GHz RI` is legal. Collect only numeric
                            // reference fields, then resume parsing options.
                            while args.peek().is_some_and(|word| word.parse::<f64>().is_ok()) {
                                if let Some(value) = args.next() {
                                    if option_refs.len() >= limits.ports {
                                        return Err(error(
                                            path,
                                            line,
                                            "too many option references",
                                        ));
                                    }
                                    option_refs.push(value.to_owned());
                                }
                            }
                            if option_refs.len() > 1 && args.peek().is_some() {
                                return Err(error(
                                    path,
                                    line,
                                    "per-port references must end the option line",
                                ));
                            }
                            if option_refs.is_empty() {
                                return Err(error(path, line, "R requires a reference impedance"));
                            }
                        }
                        _ => return Err(error(path, line, format!("bad option token {word}"))),
                    }
                }
                if !result.version.modern() {
                    ports = file_ports;
                }
                continue;
            }
            if !option_seen {
                return Err(error(path, line, "missing option line"));
            }
            if section == Section::Header {
                if result.version.modern() {
                    return Err(error(path, line, "missing [Network Data]"));
                }
                section = Section::Network;
            }
            let n =
                ports.ok_or_else(|| error(path, line, "cannot infer port count from filename"))?;
            if n == 0 || n > limits.ports {
                return Err(error(path, line, "port cap exceeded"));
            }
            if matches!(result.parameter, Parameter::H | Parameter::G) && n != 2 {
                return Err(error(path, line, "hybrid H/G parameters require two ports"));
            }
            if width.is_none() {
                let entries = n
                    .checked_mul(n)
                    .and_then(|square| match result.matrix_format {
                        MatrixFormat::Full => Some(square),
                        _ => n
                            .checked_add(1)
                            .and_then(|k| n.checked_mul(k))
                            .map(|v| v / 2),
                    })
                    .ok_or_else(|| error(path, line, "matrix dimensions overflow"))?;
                let record = entries
                    .checked_mul(2)
                    .and_then(|v| v.checked_add(1))
                    .ok_or_else(|| error(path, line, "row dimensions overflow"))?;
                let matrix_bytes = n
                    .checked_mul(n)
                    .and_then(|v| v.checked_mul(std::mem::size_of::<Complex>()))
                    .ok_or_else(|| error(path, line, "network storage overflow"))?;
                if matrix_bytes > limits.numeric_bytes
                    || record
                        .checked_mul(std::mem::size_of::<f64>())
                        .is_none_or(|v| v > limits.numeric_bytes)
                {
                    return Err(error(
                        path,
                        line,
                        "one network record exceeds the numeric budget",
                    ));
                }
                pending
                    .try_reserve(record)
                    .map_err(|_| error(path, line, "cannot reserve numeric row"))?;
                ordering = coordinates(n, result.matrix_format, result.two_port_order);
                width = Some((record, matrix_bytes));
            }
            let (record, matrix_bytes) = width.unwrap_or_default();
            if section == Section::Network {
                // Legacy noise starts at a lower frequency than the preceding
                // network records, and its five fields occupy one physical line.
                let words = without_comment.split_ascii_whitespace();
                if !result.version.modern()
                    && n == 2
                    && pending.is_empty()
                    && words.clone().take(6).count() == 5
                    && result.points.last().is_some_and(|p| {
                        words
                            .clone()
                            .next()
                            .unwrap_or_default()
                            .parse::<f64>()
                            .is_ok_and(|f| f * result.unit.scale() <= p.hz)
                    })
                {
                    section = Section::Noise;
                }
                if section == Section::Network {
                    if !result.version.modern() {
                        let fields = words.clone().take(10).count();
                        let expected = if n <= 2 {
                            record
                        } else if pending.is_empty() {
                            1 + 2 * n.min(4)
                        } else {
                            let col = ((pending.len() - 1) / 2) % n;
                            2 * (n - col).min(4)
                        };
                        if fields != expected {
                            return Err(error(path, line, "invalid legacy network row width"));
                        }
                    }
                    for word in words {
                        pending.push(numeric(path, line, word)?);
                        if pending.len() == record {
                            if result.points.len() >= limits.frequencies
                                || result
                                    .points
                                    .len()
                                    .checked_add(1)
                                    .and_then(|count| count.checked_mul(matrix_bytes))
                                    .is_none_or(|bytes| bytes > limits.numeric_bytes)
                            {
                                return Err(error(
                                    path,
                                    line,
                                    "network point or retained memory limit exceeded",
                                ));
                            }
                            let hz = pending[0] * result.unit.scale();
                            if !hz.is_finite()
                                || hz < 0.
                                || result.points.last().is_some_and(|p| hz <= p.hz)
                            {
                                return Err(error(
                                    path,
                                    line,
                                    "network frequencies must increase strictly",
                                ));
                            }
                            let mut matrix = vec![Complex::ZERO; n * n];
                            for (pair, (r, c)) in ordering.iter().copied().enumerate() {
                                let value = complex_pair(
                                    path,
                                    line,
                                    pending[2 * pair + 1],
                                    pending[2 * pair + 2],
                                    result.encoding,
                                )?;
                                matrix[r * n + c] = value;
                                if result.matrix_format != MatrixFormat::Full {
                                    matrix[c * n + r] = value;
                                }
                            }
                            result.points.push(NetworkPoint { hz, matrix });
                            pending.clear();
                            if expected_points.is_some_and(|limit| result.points.len() > limit) {
                                return Err(error(path, line, "too many network frequencies"));
                            }
                        }
                    }
                    continue;
                }
            }
            if section == Section::Noise {
                let values = without_comment
                    .split_ascii_whitespace()
                    .take(6)
                    .map(|x| numeric(path, line, x))
                    .collect::<TouchstoneResult<Vec<_>>>()?;
                if n != 2 || values.len() != 5 || !pending.is_empty() {
                    return Err(error(
                        path,
                        line,
                        "noise section only permits five fields per two-port line",
                    ));
                }
                let hz = values[0] * result.unit.scale();
                if !hz.is_finite() || hz < 0. || result.noise.last().is_some_and(|p| hz <= p.hz) {
                    return Err(error(
                        path,
                        line,
                        "noise frequencies must increase strictly",
                    ));
                }
                if !(0. ..=1.).contains(&values[2]) || values[4] < 0. {
                    return Err(error(
                        path,
                        line,
                        "invalid optimum noise reflection or resistance",
                    ));
                }
                let rad = values[3].to_radians();
                result.noise.push(NoisePoint {
                    hz,
                    min_figure_db: values[1],
                    optimum_reflection: Complex::new(values[2] * rad.cos(), values[2] * rad.sin()),
                    resistance_ohms: values[4],
                });
                if result
                    .points
                    .len()
                    .checked_mul(matrix_bytes)
                    .and_then(|bytes| {
                        result
                            .noise
                            .len()
                            .checked_mul(std::mem::size_of::<NoisePoint>())
                            .and_then(|noise| bytes.checked_add(noise))
                    })
                    .is_none_or(|bytes| bytes > limits.numeric_bytes)
                    || result.noise.len() > limits.frequencies
                    || expected_noise.is_some_and(|limit| result.noise.len() > limit)
                {
                    return Err(error(path, line, "noise count budget exceeded"));
                }
                continue;
            }
            return Err(error(path, line, "numeric data in unexpected section"));
        }

        if !option_seen || ports.is_none() {
            return Err(error(path, 0, "missing options or port count"));
        }
        if continuation.is_some() || section == Section::Information {
            return Err(error(path, 0, "unfinished section or header list"));
        }
        let n = ports.unwrap_or_default();
        if n == 0 || n > limits.ports {
            return Err(error(path, 0, "port count exceeds budget"));
        }
        if result.version.modern()
            && (!version_seen
                || section != Section::End
                || !keywords.contains("network data")
                || expected_points.is_none())
        {
            return Err(error(
                path,
                0,
                "v2 requires [Version], [Number of Frequencies], [Network Data] and [End]",
            ));
        }
        if result.version.modern() && n == 2 && !keywords.contains("two-port data order") {
            return Err(error(path, 0, "two-port v2 requires [Two-Port Data Order]"));
        }
        if !pending.is_empty()
            || result.points.is_empty()
            || expected_points.is_some_and(|want| want != result.points.len())
        {
            return Err(error(path, 0, "network frequency record count mismatch"));
        }
        if expected_noise.is_some_and(|want| want != result.noise.len()) {
            return Err(error(path, 0, "noise frequency record count mismatch"));
        }
        if !result.version.modern() && option_refs.len() == n && n > 1 {
            result.version = Version::V1_1;
        }
        if option_refs.len() > 1 && (result.version.modern() || option_refs.len() != n) {
            return Err(error(path, 0, "wrong number of R reference arguments"));
        }
        let default_reference = option_refs
            .first()
            .map(|word| positive(path, 0, word))
            .transpose()?
            .unwrap_or(50.);
        result.references = if ref_args.is_empty() {
            if result.version == Version::V1_1 {
                option_refs
                    .iter()
                    .map(|word| positive(path, 0, word))
                    .collect::<TouchstoneResult<Vec<_>>>()?
            } else {
                vec![default_reference; n]
            }
        } else {
            if ref_args.len() != n {
                return Err(error(path, 0, "[Reference] requires N impedances"));
            }
            ref_args
                .iter()
                .map(|word| positive(path, 0, word))
                .collect::<TouchstoneResult<Vec<_>>>()?
        };
        if !mixed_args.is_empty() {
            result.mixed_mode = Some(mixed_axes(path, 0, &mixed_args, &result.references)?);
        }
        if result.mixed_mode.is_some() && matches!(result.parameter, Parameter::H | Parameter::G) {
            return Err(error(path, 0, "mixed-mode H/G matrices are invalid"));
        }
        if !result.version.modern() {
            // Touchstone 1.x normalizes Y and Z by its reference resistances.
            // Store physical siemens/ohms, independent from on-disk version.
            for point in &mut result.points {
                for r in 0..n {
                    for c in 0..n {
                        let factor = legacy_factor(result.parameter, &result.references, r, c);
                        if !factor.is_finite() || factor <= 0. {
                            return Err(error(path, 0, "unrepresentable reference normalization"));
                        }
                        let value = point.matrix[r * n + c];
                        point.matrix[r * n + c] =
                            Complex::new(value.re * factor, value.im * factor);
                    }
                }
            }
            for point in &mut result.noise {
                point.resistance_ohms *= default_reference;
            }
        }
        if result
            .points
            .iter()
            .flat_map(|point| &point.matrix)
            .any(|v| !v.is_finite())
            || result.noise.iter().any(|p| !p.resistance_ohms.is_finite())
        {
            return Err(error(
                path,
                0,
                "normalization produced non-finite coefficients",
            ));
        }
        if !result.noise.is_empty()
            && (n != 2 || result.noise[0].hz > result.points.last().map_or(0., |p| p.hz))
        {
            return Err(error(path, 0, "invalid two-port noise frequency range"));
        }
        result.validate(limits)?;
        Ok(result)
    }

    /// Validate an owned network before export or physical simulation.
    /// # Errors
    /// Invalid data, incompatible dimensions/metadata or excess resources.
    pub fn validate(&self, limits: TouchstoneLimits) -> TouchstoneResult<()> {
        let n = self.references.len();
        if n == 0
            || n > limits.ports
            || self.points.is_empty()
            || self.points.len() > limits.frequencies
            || self.noise.len() > limits.frequencies
        {
            return Err(error("model", 0, "invalid port/point limits"));
        }
        let cells = n
            .checked_mul(n)
            .and_then(|v| v.checked_mul(self.points.len()))
            .and_then(|v| v.checked_mul(std::mem::size_of::<Complex>()))
            .and_then(|v| {
                self.noise
                    .len()
                    .checked_mul(std::mem::size_of::<NoisePoint>())
                    .and_then(|noise| v.checked_add(noise))
            });
        if cells.is_none_or(|v| v > limits.numeric_bytes) {
            return Err(error("model", 0, "numeric storage limit exceeded"));
        }
        if self.references.iter().any(|&r| !r.is_finite() || r <= 0.)
            || self.points.iter().any(|p| {
                p.matrix.len() != n * n
                    || !p.hz.is_finite()
                    || p.hz < 0.
                    || p.matrix.iter().any(|value| !value.is_finite())
            })
            || self.points.windows(2).any(|w| w[0].hz >= w[1].hz)
        {
            return Err(error(
                "model",
                0,
                "invalid frequencies, matrix shape or references",
            ));
        }
        if self.noise.iter().any(|v| {
            !v.hz.is_finite()
                || v.hz < 0.
                || !v.min_figure_db.is_finite()
                || !v.optimum_reflection.is_finite()
                || v.optimum_reflection.magnitude() > 4.0_f64.mul_add(f64::EPSILON, 1.)
                || !v.resistance_ohms.is_finite()
                || v.resistance_ohms < 0.
        }) || self.noise.windows(2).any(|w| w[0].hz >= w[1].hz)
            || self
                .noise
                .first()
                .is_some_and(|p| n != 2 || p.hz > self.points.last().map_or(0., |last| last.hz))
        {
            return Err(error("model", 0, "invalid noise entries"));
        }
        if let Some(mixed) = &self.mixed_mode {
            if matches!(self.parameter, Parameter::H | Parameter::G)
                || mixed.iter().any(|axis| match *axis {
                    MixedPort::Single(a) => a >= n,
                    MixedPort::Differential(a, b) | MixedPort::Common(a, b) => a >= n || b >= n,
                })
            {
                return Err(error("model", 0, "invalid mixed-mode axes"));
            }
            let words = mixed.iter().map(|x| x.label()).collect::<Vec<_>>();
            mixed_axes("model", 0, &words, &self.references)?;
        }
        if matches!(self.parameter, Parameter::G | Parameter::H) && n != 2 {
            return Err(error(
                "model",
                0,
                "hybrid parameter is only defined for two ports",
            ));
        }
        if self.matrix_format != MatrixFormat::Full
            && self
                .points
                .iter()
                .any(|p| (0..n).any(|r| (0..n).any(|c| p.matrix[r * n + c] != p.matrix[c * n + r])))
        {
            return Err(error(
                "model",
                0,
                "triangular format requires symmetric matrices",
            ));
        }
        Ok(())
    }

    fn write_v2_header(&self, output: &mut BoundedText) -> TouchstoneResult<()> {
        let n = self.references.len();
        writeln!(output, "[Number of Ports] {n}")
            .map_err(|_| error("output", 0, "format failure"))?;
        if n == 2 {
            let label = match self.two_port_order {
                TwoPortOrder::N21BeforeN12 => "21_12",
                TwoPortOrder::N12BeforeN21 => "12_21",
            };
            writeln!(output, "[Two-Port Data Order] {label}")
                .map_err(|_| error("output", 0, "format failure"))?;
        }
        writeln!(output, "[Number of Frequencies] {}", self.points.len())
            .map_err(|_| error("output", 0, "format failure"))?;
        if !self.noise.is_empty() {
            writeln!(output, "[Number of Noise Frequencies] {}", self.noise.len())
                .map_err(|_| error("output", 0, "format failure"))?;
        }
        output.push_str("[Reference]");
        for &r in &self.references {
            write!(output, " {r:.17e}").map_err(|_| error("output", 0, "format failure"))?;
        }
        output.push('\n');
        if self.matrix_format != MatrixFormat::Full {
            let label = if self.matrix_format == MatrixFormat::Lower {
                "Lower"
            } else {
                "Upper"
            };
            writeln!(output, "[Matrix Format] {label}")
                .map_err(|_| error("output", 0, "format failure"))?;
        }
        if let Some(axes) = &self.mixed_mode {
            output.push_str("[Mixed-Mode Order]");
            for &axis in axes {
                write!(output, " {}", axis.label())
                    .map_err(|_| error("output", 0, "format failure"))?;
            }
            output.push('\n');
        }
        if !self.information.is_empty() {
            output.push_str("[Begin Information]\n");
            for line in &self.information {
                if !line.is_ascii()
                    || !line.bytes().all(|b| b == b'\t' || (32..127).contains(&b))
                    || line
                        .split_once('!')
                        .map_or(line.as_str(), |(part, _)| part)
                        .trim()
                        .eq_ignore_ascii_case("[End Information]")
                {
                    return Err(error("output", 0, "invalid information text"));
                }
                output.push_str(line);
                output.push('\n');
            }
            output.push_str("[End Information]\n");
        }
        output.push_str("[Network Data]\n");
        Ok(())
    }

    fn write_network_data(
        &self,
        output: &mut BoundedText,
        version: Version,
        encoding: Encoding,
    ) -> TouchstoneResult<()> {
        let n = self.references.len();
        let order = coordinates(
            n,
            self.matrix_format,
            if version.modern() {
                self.two_port_order
            } else {
                TwoPortOrder::N21BeforeN12
            },
        );
        for point in &self.points {
            write!(output, "{:.17e}", point.hz / self.unit.scale())
                .map_err(|_| error("output", 0, "format failure"))?;
            for (index, (r, c)) in order.iter().copied().enumerate() {
                let mut value = point.matrix[r * n + c];
                if !version.modern() {
                    let scale = legacy_factor(self.parameter, &self.references, r, c);
                    if !scale.is_finite() || scale <= 0. {
                        return Err(error(
                            "output",
                            0,
                            "unrepresentable reference normalization",
                        ));
                    }
                    value = Complex::new(value.re / scale, value.im / scale);
                }
                let (a, b) = match encoding {
                    Encoding::Ri => (value.re, value.im),
                    Encoding::Ma => (value.magnitude(), value.phase().to_degrees()),
                    Encoding::Db => {
                        let magnitude = value.magnitude();
                        if magnitude == 0. {
                            return Err(error(
                                "output",
                                0,
                                "exact zero cannot be written as finite dB",
                            ));
                        }
                        (20. * magnitude.log10(), value.phase().to_degrees())
                    }
                };
                if !a.is_finite() || !b.is_finite() {
                    return Err(error("output", 0, "unrepresentable complex encoding"));
                }
                write!(output, " {a:.17e} {b:.17e}")
                    .map_err(|_| error("output", 0, "format failure"))?;
                if !version.modern()
                    && n > 2
                    && ((c + 1) % 4 == 0 || c + 1 == n)
                    && index + 1 < order.len()
                {
                    output.push('\n');
                }
            }
            output.push('\n');
        }
        Ok(())
    }

    /// Export a verified network in the requested official Touchstone version.
    /// # Errors
    /// Unsupported format/version combinations or unrepresentable coefficients.
    pub fn write(&self, version: Version, encoding: Encoding) -> TouchstoneResult<String> {
        self.write_with_limits(version, encoding, TouchstoneLimits::default())
    }

    /// Write to an owned bounded ASCII string; the encoded byte limit is
    /// enforced *during* every append, not after allocation has grown.
    /// # Errors
    /// Invalid data, unsupported versions, or exceeded output byte/point caps.
    pub fn write_with_limits(
        &self,
        version: Version,
        encoding: Encoding,
        limits: TouchstoneLimits,
    ) -> TouchstoneResult<String> {
        self.validate(limits)?;
        if !version.modern()
            && (self.mixed_mode.is_some()
                || self.matrix_format != MatrixFormat::Full
                || !self.information.is_empty())
        {
            return Err(error(
                "output",
                0,
                "v1 cannot encode mixed-mode, triangular matrices or information sections",
            ));
        }
        if version == Version::V1_0
            && self
                .references
                .iter()
                .any(|&r| r.to_bits() != self.references[0].to_bits())
        {
            return Err(error("output", 0, "v1.0 requires equal references"));
        }
        let name = match self.parameter {
            Parameter::S => "S",
            Parameter::Y => "Y",
            Parameter::Z => "Z",
            Parameter::G => "G",
            Parameter::H => "H",
        };
        let pair = match encoding {
            Encoding::Ri => "RI",
            Encoding::Ma => "MA",
            Encoding::Db => "DB",
        };
        let mut output = BoundedText::new(limits.source_bytes);
        if version.modern() {
            writeln!(output, "[Version] {}", version.label())
                .map_err(|_| error("output", 0, "format failure"))?;
        }
        write!(output, "# {} {name} {pair} R", self.unit.label())
            .map_err(|_| error("output", 0, "format failure"))?;
        if version == Version::V1_1 {
            for &r in &self.references {
                write!(output, " {r:.17e}").map_err(|_| error("output", 0, "format failure"))?;
            }
        } else {
            write!(output, " {:.17e}", self.references[0])
                .map_err(|_| error("output", 0, "format failure"))?;
        }
        output.push('\n');
        if version.modern() {
            self.write_v2_header(&mut output)?;
        }
        self.write_network_data(&mut output, version, encoding)?;
        if !self.noise.is_empty() {
            if version.modern() {
                output.push_str("[Noise Data]\n");
            }
            for point in &self.noise {
                let resistance = if version.modern() {
                    point.resistance_ohms
                } else {
                    point.resistance_ohms / self.references[0]
                };
                if !resistance.is_finite() {
                    return Err(error("output", 0, "unrepresentable noise resistance"));
                }
                writeln!(
                    output,
                    "{:.17e} {:.17e} {:.17e} {:.17e} {resistance:.17e}",
                    point.hz / self.unit.scale(),
                    point.min_figure_db,
                    point.optimum_reflection.magnitude().min(1.),
                    point.optimum_reflection.phase().to_degrees(),
                )
                .map_err(|_| error("output", 0, "format failure"))?;
            }
        }
        if version.modern() {
            output.push_str("[End]\n");
        }
        output.finish()
    }
}
