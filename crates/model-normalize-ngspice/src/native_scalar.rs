//! Shared deterministic native ngspice scalar operators and intrinsics.
//!
//! These types deliberately do not select a source dialect, resolve symbols,
//! realize stochastic calls or evaluate expression trees. The producer and
//! solver each retain their lexical and resource contexts and use this
//! single arithmetic contract for native numparam (xpressn.c).
//! Runtime behavioural laws use a different grammar and power semantics.

use std::cmp::Ordering;

/// Prefix signs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    /// `+x`
    Plus,
    /// `-x`
    Minus,
    /// `!x`
    Not,
}

/// Binary arithmetic operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `^` or `**` (both spellings are one operator in C). Left associative:
    /// `2^3^2` is `(2^3)^2 = 64` in C. C's default `^` is `pow(fabs(x), y)`
    /// (xpressn.c `operate()`), so an evaluator must not use plain `powf`.
    Pow,
    /// Remainder after truncating the quotient.
    Rem,
    /// Truncated magnitude of the quotient.
    IntDiv,
    /// Equality.
    Eq,
    /// Inequality.
    Ne,
    /// Less than.
    Lt,
    /// Less than or equal.
    Le,
    /// Greater than.
    Gt,
    /// Greater than or equal.
    Ge,
    /// Logical conjunction.
    And,
    /// Logical disjunction.
    Or,
}

/// Native conditional reduction, with separate question and colon precedence.
///
/// The caller parses each operand and retains its own spans and budgets.
/// A pair followed by another question chains on the false branch; consecutive
/// questions or colons retain the earlier accumulator (ngspice xpressn.c).
/// The chain contains only surviving operands; all source operands must still
/// be parsed, charged to the budget and syntax-checked by the caller.
pub struct ConditionalChain<T, M> {
    condition: T,
    marker: M,
    pairs: Vec<(T, T, M)>,
    positive: Option<T>,
    questions: usize,
    colons: usize,
}

/// Unbalanced native conditional syntax.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConditionalError {
    /// A question has no colon.
    MissingColon,
    /// Too many colons.
    ExtraColon,
}

impl<T, M> ConditionalChain<T, M> {
    /// Begin with the operand immediately preceding the first question.
    #[must_use = "the chain accumulates subsequent conditional operands"]
    pub const fn new(condition: T, marker: M) -> Self {
        Self {
            condition,
            marker,
            pairs: Vec::new(),
            positive: None,
            questions: 1,
            colons: 0,
        }
    }

    /// Consume an operand followed by another question.
    pub fn question(&mut self, operand: T, marker: M) {
        self.questions += 1;
        if let Some(positive) = self.positive.take() {
            self.pairs.push((
                std::mem::replace(&mut self.condition, operand),
                positive,
                std::mem::replace(&mut self.marker, marker),
            ));
        }
    }

    /// Consume an operand followed by a colon.
    ///
    /// # Errors
    /// Returns an error when there are more colons than questions.
    pub fn colon(&mut self, operand: T) -> Result<(), ConditionalError> {
        self.colons += 1;
        if self.colons > self.questions {
            return Err(ConditionalError::ExtraColon);
        }
        if self.positive.is_none() {
            self.positive = Some(operand);
        }
        Ok(())
    }

    /// Fold surviving pairs from right to left, using the caller's node builder.
    ///
    /// # Errors
    /// Returns an error for unbalanced syntax or a failure in the node builder.
    pub fn finish<E>(
        self,
        fallback: T,
        unbalanced: impl FnOnce(ConditionalError) -> E,
        mut build: impl FnMut(T, T, T, M) -> Result<T, E>,
    ) -> Result<T, E> {
        let Some(positive) = self.positive else {
            return Err(unbalanced(ConditionalError::MissingColon));
        };
        if self.questions != self.colons {
            return Err(unbalanced(ConditionalError::MissingColon));
        }
        let mut result = build(self.condition, positive, fallback, self.marker)?;
        for (condition, positive, marker) in self.pairs.into_iter().rev() {
            result = build(condition, positive, result, marker)?;
        }
        Ok(result)
    }
}

impl UnaryOp {
    /// Native prefix operator spelling.
    #[must_use = "the spelling identifies the operator in native syntax"]
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::Plus => "+",
            Self::Minus => "-",
            Self::Not => "!",
        }
    }

    /// Native prefix operator lookup.
    #[must_use = "the returned value describes native scalar semantics"]
    pub fn from_symbol(symbol: &str) -> Option<Self> {
        match symbol {
            "+" => Some(Self::Plus),
            "-" => Some(Self::Minus),
            "!" => Some(Self::Not),
            _ => None,
        }
    }

    /// Apply a native prefix operation.
    #[must_use = "the returned value describes native scalar semantics"]
    pub const fn evaluate(self, value: f64) -> f64 {
        match self {
            Self::Plus => value,
            Self::Minus => -value,
            Self::Not => {
                if value == 0.0 {
                    1.0
                } else {
                    0.0
                }
            }
        }
    }
}

/// Binary operators by native numparam precedence, lowest first.
/// Within each level match longer spellings before their prefixes.
pub const OPERATOR_LEVELS: &[&[(&str, BinaryOp)]] = &[
    &[("||", BinaryOp::Or)],
    &[("&&", BinaryOp::And)],
    &[
        ("==", BinaryOp::Eq),
        ("!=", BinaryOp::Ne),
        ("<>", BinaryOp::Ne),
        ("<=", BinaryOp::Le),
        (">=", BinaryOp::Ge),
        ("=", BinaryOp::Eq),
        ("<", BinaryOp::Lt),
        (">", BinaryOp::Gt),
    ],
    &[("+", BinaryOp::Add), ("-", BinaryOp::Sub)],
    &[
        ("*", BinaryOp::Mul),
        ("/", BinaryOp::Div),
        ("%", BinaryOp::Rem),
        ("\\", BinaryOp::IntDiv),
    ],
    &[("**", BinaryOp::Pow), ("^", BinaryOp::Pow)],
];

impl BinaryOp {
    /// Lookup native binary operator aliases.
    #[must_use = "the returned value describes native scalar semantics"]
    pub fn from_symbol(symbol: &str) -> Option<Self> {
        OPERATOR_LEVELS
            .iter()
            .flat_map(|level| level.iter())
            .find_map(|&(name, op)| (name == symbol).then_some(op))
    }

    /// Canonical native binary spelling.
    #[must_use = "the spelling identifies the operator in native syntax"]
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
            Self::Div => "/",
            Self::Pow => "**",
            Self::Rem => "%",
            Self::IntDiv => "\\",
            Self::Eq => "==",
            Self::Ne => "!=",
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::And => "&&",
            Self::Or => "||",
        }
    }

    /// Binding power for a precedence-climbing native scalar parser.
    #[must_use = "the returned value describes native scalar semantics"]
    pub const fn precedence(self) -> u8 {
        match self {
            Self::Or => 2,
            Self::And => 3,
            Self::Eq | Self::Ne | Self::Lt | Self::Le | Self::Gt | Self::Ge => 4,
            Self::Add | Self::Sub => 5,
            Self::Mul | Self::Div | Self::Rem | Self::IntDiv => 6,
            Self::Pow => 8,
        }
    }

    /// Short-circuit a logical operation before its right operand is read.
    #[must_use = "the returned value describes native scalar semantics"]
    pub const fn short_circuit(self, left: f64) -> Option<f64> {
        match self {
            Self::And if left == 0.0 => Some(0.0),
            Self::Or if left != 0.0 => Some(1.0),
            _ => None,
        }
    }

    /// Apply the native operation to finite operands. The caller checks the
    /// result for finiteness and adds source-specific diagnostics.
    #[must_use = "the returned value describes native scalar semantics"]
    pub fn evaluate(self, left: f64, right: f64) -> f64 {
        match self {
            Self::Add => left + right,
            Self::Sub => left - right,
            Self::Mul => left * right,
            Self::Div => left / right,
            Self::Pow => left.abs().powf(right),
            // Separate multiplication and subtraction reproduce the C
            // remainder operation, not a fused multiply-add or Rust %.
            Self::Rem => remainder(left, right),
            Self::IntDiv => (left / right).abs().trunc(),
            Self::Eq => f64::from(left.partial_cmp(&right) == Some(Ordering::Equal)),
            Self::Ne => f64::from(left.partial_cmp(&right) != Some(Ordering::Equal)),
            Self::Lt => f64::from(left < right),
            Self::Le => f64::from(left <= right),
            Self::Gt => f64::from(left > right),
            Self::Ge => f64::from(left >= right),
            Self::And => f64::from(left != 0.0 && right != 0.0),
            Self::Or => f64::from(left != 0.0 || right != 0.0),
        }
    }
}

fn remainder(left: f64, right: f64) -> f64 {
    let quotient = (left / right).trunc();
    let product = right * quotient;
    left - product
}

macro_rules! functions {
    ($($variant:ident => $name:literal / $arity:literal,)*) => {
        /// The allowlisted numparam functions (a subset of `fmathS` in
        /// `xpressn.c`). Statistical calls are realized separately from
        /// ordinary deterministic math functions.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Function {
            $(
                #[doc = concat!("`", $name, "`")]
                $variant,
            )*
        }

        impl Function {
            /// Every allowlisted function, in `fmathS` order.
            pub const ALL: &'static [Function] = &[$(Self::$variant,)*];

            /// Looks a function up by (case-insensitive) name.
            #[must_use = "the lookup result identifies the supported intrinsic function"]
            pub fn from_name(name: &str) -> Option<Self> {
                match name.to_ascii_lowercase().as_str() {
                    $($name => Some(Self::$variant),)*
                    _ => None,
                }
            }

            /// Canonical lowercase name.
            #[must_use = "the name is the canonical intrinsic function spelling"]
            pub const fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $name,)*
                }
            }

            /// Required argument count.
            #[must_use = "the returned arity determines the required argument count"]
            pub const fn arity(self) -> usize {
                match self {
                    $(Self::$variant => $arity,)*
                }
            }
        }
    };
}

functions! {
    Sqr => "sqr" / 1,
    Sqrt => "sqrt" / 1,
    Sin => "sin" / 1,
    Cos => "cos" / 1,
    Exp => "exp" / 1,
    Ln => "ln" / 1,
    Arctan => "arctan" / 1,
    Abs => "abs" / 1,
    Pow => "pow" / 2,
    Pwr => "pwr" / 2,
    Max => "max" / 2,
    Min => "min" / 2,
    Int => "int" / 1,
    Log => "log" / 1,
    Log10 => "log10" / 1,
    Sinh => "sinh" / 1,
    Cosh => "cosh" / 1,
    Tanh => "tanh" / 1,
    Ternary => "ternary_fcn" / 3,
    Sgn => "sgn" / 1,
    Ceil => "ceil" / 1,
    Floor => "floor" / 1,
    Asin => "asin" / 1,
    Acos => "acos" / 1,
    Atan => "atan" / 1,
    Asinh => "asinh" / 1,
    Acosh => "acosh" / 1,
    Atanh => "atanh" / 1,
    Tan => "tan" / 1,
    Nint => "nint" / 1,
}

/// xpressn.c `mathfunction(f, z, x)`: `z` is the first argument of two.
impl Function {
    /// Evaluate the deterministic intrinsic on already evaluated arguments.
    ///
    /// Returns None for the wrong arity. The caller owns finite-value
    /// diagnostics, source positions and lazy selection of ternary branches.
    #[must_use = "the scalar result must be checked before use"]
    pub fn evaluate(self, args: &[f64]) -> Option<f64> {
        if args.len() != self.arity() {
            return None;
        }
        Some(evaluate_function(self, args))
    }
}

fn evaluate_function(function: Function, args: &[f64]) -> f64 {
    let x = args[args.len() - 1];
    let z = args[0];
    match function {
        Function::Ternary => {
            if z != 0.0 {
                args[1]
            } else {
                x
            }
        }
        Function::Sqr => x * x,
        Function::Sqrt => x.sqrt(),
        Function::Sin => x.sin(),
        Function::Cos => x.cos(),
        Function::Exp => x.exp(),
        Function::Ln | Function::Log => x.ln(),
        Function::Arctan | Function::Atan => x.atan(),
        Function::Abs => x.abs(),
        Function::Pow => z.powf(x),
        Function::Pwr => z.abs().powf(x),
        Function::Max => {
            if x > z {
                x
            } else {
                z
            }
        }
        Function::Min => {
            if x < z {
                x
            } else {
                z
            }
        }
        Function::Int => x.trunc(),
        Function::Nint => x.round_ties_even(),
        Function::Log10 => x.log10(),
        Function::Sinh => x.sinh(),
        Function::Cosh => x.cosh(),
        Function::Tanh => x.tanh(),
        Function::Sgn => {
            if x > 0.0 {
                1.0
            } else if x == 0.0 {
                0.0
            } else {
                -1.0
            }
        }
        Function::Ceil => x.ceil(),
        Function::Floor => x.floor(),
        Function::Asin => x.asin(),
        Function::Acos => x.acos(),
        Function::Asinh => x.asinh(),
        Function::Acosh => x.acosh(),
        Function::Atanh => x.atanh(),
        Function::Tan => x.tan(),
    }
}
