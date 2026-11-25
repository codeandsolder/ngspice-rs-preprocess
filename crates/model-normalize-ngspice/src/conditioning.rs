//! Exact current-coordinate scaling of private differentiator loops.
//!
//! A cycle of ideal voltage-defined branches and one linear capacitor has
//! one circulating current. If all but at most one node are private, the
//! cycle contributes zero net current to the rest of the circuit. Scaling
//! its capacitance by a positive constant scales every branch current by
//! that constant while preserving all voltages. Substitute old current =
//! new current / scale at every observable use. This is a constant diagonal
//! coordinate change in the DAE, not a modification to its terminal law.

use crate::{
    lexical::{self, Edit, Statement},
    model_card::parse_spice_constant,
    parameter_scope::ModelLibrary,
};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt::Write as _,
    ops::Range,
};

/// One verified private current-coordinate change in a generated artifact.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CurrentScaling {
    /// Source definition names in lexical order.
    pub scope: Vec<String>,
    /// Capacitor whose current coordinate was scaled.
    pub capacitor: String,
    /// Voltage-defined branches carrying the same circulating current.
    pub voltage_branches: Vec<String>,
    /// Physical original branch current = emitted current / factor.
    pub factor: f64,
}

/// Conditioned bytes and the explicit internal-current conversion metadata.
#[derive(Debug)]
pub struct ConditionedArtifact {
    pub bytes: Vec<u8>,
    pub scalings: Vec<CurrentScaling>,
}

struct Card {
    artifact: usize,
    statement: Statement,
    scope: usize,
    fields: Vec<Range<usize>>,
    text: String,
    identifiers: BTreeMap<String, Vec<Range<usize>>>,
    probes: Vec<Probe>,
}

impl Card {
    fn field(&self, index: usize) -> Option<&str> {
        self.fields
            .get(index)
            .and_then(|span| self.text().get(span.clone()))
    }

    fn text(&self) -> &str {
        &self.text
    }

    fn mentions(&self, name: &str) -> &[Range<usize>] {
        self.identifiers.get(name).map_or(&[], Vec::as_slice)
    }

    fn first(&self) -> &str {
        self.field(0).unwrap_or_default()
    }

    fn name(&self) -> String {
        self.first().to_ascii_lowercase()
    }

    fn kind(&self) -> Option<u8> {
        self.first().as_bytes().first().map(u8::to_ascii_uppercase)
    }
}

#[derive(Default)]
struct Scope {
    path: Vec<String>,
    cards: Vec<usize>,
    ports: BTreeSet<String>,
}

/// A named emitted electrical artifact in the closed circuit graph.
pub struct ConditioningInput<'a> {
    pub filename: &'a str,
    pub bytes: &'a [u8],
}

struct Source {
    cards: Vec<Card>,
    scopes: Vec<Scope>,
    globals: BTreeSet<String>,
    parameters: ModelLibrary,
    references: BTreeMap<String, Vec<usize>>,
    constrained_capacitors: BTreeSet<usize>,
    waveform_sources: BTreeSet<usize>,
}

impl Source {
    fn parse(
        inputs: &[ConditioningInput<'_>],
        initialization: Option<&crate::passive_ic::InitialConditions>,
        waveforms: &[crate::waveform::PreparedWaveform],
    ) -> Result<Self> {
        let mut cards = Vec::new();
        let mut scopes = vec![Scope::default()];
        // Ground aliases and conventional global nodes cannot be private.
        let mut globals = BTreeSet::from(["0".to_owned(), "gnd".to_owned()]);
        let mut references = BTreeMap::<String, Vec<usize>>::new();
        // Only declarations needed for the scale estimate are passed to the
        // shared scalar resolver. Unrelated device/model grammars are not
        // an additional admission gate for already translated artifacts.
        let mut parameters = String::new();
        for (artifact, input) in inputs.iter().enumerate() {
            let mut stack = vec![0];
            for statement in lexical::statements(input.bytes)? {
                let text = std::str::from_utf8(&statement.text)?.to_owned();
                let fields = lexical::tokens(&statement.text)?;
                let first = fields
                    .first()
                    .and_then(|span| text.get(span.clone()))
                    .unwrap_or_default();
                let scope = *stack.last().context("missing conditioning scope")?;
                let upper = first.to_ascii_uppercase();
                match upper.as_str() {
                    ".SUBCKT" => {
                        let mut words = text.split_whitespace();
                        words.next();
                        let name = words.next().context("unnamed conditioning scope")?;
                        let mut path = scopes[scope].path.clone();
                        path.push(name.to_owned());
                        // The public model parser is the authoritative
                        // interpretation of ports/defaults. Here reserving
                        // all header words is safe even for parameter names.
                        let ports = words.map(str::to_ascii_lowercase).collect();
                        stack.push(scopes.len());
                        scopes.push(Scope {
                            path,
                            cards: Vec::new(),
                            ports,
                        });
                        writeln!(parameters, "{text}")?;
                    }
                    ".ENDS" => {
                        ensure!(stack.len() > 1, "unmatched conditioning scope terminator");
                        stack.pop();
                        writeln!(parameters, "{text}")?;
                    }
                    ".PARAM" | ".FUNC" => writeln!(parameters, "{text}")?,
                    ".GLOBAL" => {
                        globals
                            .extend(text.split_whitespace().skip(1).map(str::to_ascii_lowercase));
                        writeln!(parameters, "{text}")?;
                    }
                    // An arbitrary control program could observe branch
                    // current through a dynamically constructed name.
                    ".CONTROL" => anyhow::bail!("control program in conditioning input"),
                    _ => {}
                }
                scopes[scope].cards.push(cards.len());
                let identifiers = identifiers(&text);
                if matches!(upper.as_str(), ".OPTION" | ".OPTIONS")
                    && ["rshunt", "gshunt", "cshunt"]
                        .iter()
                        .any(|name| identifiers.contains_key(*name))
                {
                    anyhow::bail!("global shunts invalidate private differentiator topology");
                }
                for name in identifiers.keys() {
                    references
                        .entry(name.clone())
                        .or_default()
                        .push(cards.len());
                }
                let mut card = Card {
                    artifact,
                    statement,
                    scope,
                    fields,
                    text,
                    identifiers,
                    probes: Vec::new(),
                };
                card.probes = probes(&card);
                cards.push(card);
            }
            ensure!(stack.len() == 1, "unclosed conditioning scope");
        }
        let parameters = ModelLibrary::parse(parameters.as_bytes())?;
        ensure!(
            parameters.scopes.len() == scopes.len(),
            "conditioning scope mismatch"
        );
        let mut source = Self {
            cards,
            scopes,
            globals,
            parameters,
            references,
            constrained_capacitors: BTreeSet::new(),
            waveform_sources: BTreeSet::new(),
        };
        if let Some(initialization) = initialization {
            source.add_initialization_paths(inputs, initialization);
        }
        source.reserve_waveform_sources(inputs, waveforms);
        Ok(source)
    }

    fn reserve_waveform_sources(
        &mut self,
        inputs: &[ConditioningInput<'_>],
        waveforms: &[crate::waveform::PreparedWaveform],
    ) {
        // A DC=0 placeholder is not an ideal zero-voltage current sensor.
        // Treat its actual terminals as an ordinary nonmember electrical
        // path until the proof is extended to independent time functions.
        for waveform in waveforms {
            for &index in self
                .references
                .get(&waveform.source.to_ascii_lowercase())
                .into_iter()
                .flatten()
            {
                let card = &self.cards[index];
                if card.kind() == Some(b'V')
                    && card.first().eq_ignore_ascii_case(&waveform.source)
                    && inputs[card.artifact].filename == waveform.artifact
                    && self.scopes[card.scope].path == waveform.scope
                {
                    self.waveform_sources.insert(index);
                }
            }
        }
    }

    fn add_initialization_paths(
        &mut self,
        inputs: &[ConditioningInput<'_>],
        initialization: &crate::passive_ic::InitialConditions,
    ) {
        for device in &initialization.devices {
            if device.kind != crate::passive_ic::PassiveKind::Capacitor
                || (device.target.is_none() && device.branch.is_none())
            {
                continue;
            }
            // A typed bias constraint can carry current in parallel with
            // the capacitor even though the electrical card has no IC.
            for &index in self
                .references
                .get(&device.device.to_ascii_lowercase())
                .into_iter()
                .flatten()
            {
                let card = &self.cards[index];
                if inputs[card.artifact].filename == device.artifact
                    && self.scopes[card.scope].path == device.scope
                    && card.first().eq_ignore_ascii_case(&device.device)
                {
                    self.constrained_capacitors.insert(index);
                }
            }
        }
        for hint in &initialization.nodes {
            let node = hint.node.to_ascii_lowercase();
            // A local .IC adds a node-to-ground path during bias. Ports
            // are already exposed; root hierarchical hints can reach a
            // private node by its final path component.
            for scope in &mut self.scopes {
                if scope.path == hint.scope {
                    scope.ports.insert(node.clone());
                }
            }
            if let Some((_, tail)) = node.rsplit_once('.') {
                self.globals.insert(tail.to_owned());
            }
        }
    }
}

fn simple_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(name_byte)
}

const fn name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$')
}

/// Index each identifier once. Periods and branch-suffix markers are
/// separate, so hierarchical and @device observations remain discoverable.
fn identifiers(text: &str) -> BTreeMap<String, Vec<Range<usize>>> {
    let bytes = text.as_bytes();
    let mut result = BTreeMap::<String, Vec<Range<usize>>>::new();
    let mut offset = 0;
    while offset < bytes.len() {
        if !name_byte(bytes[offset]) {
            offset += 1;
            continue;
        }
        let start = offset;
        while bytes.get(offset).copied().is_some_and(name_byte) {
            offset += 1;
        }
        result
            .entry(text[start..offset].to_ascii_lowercase())
            .or_default()
            .push(start..offset);
    }
    result
}

#[derive(Clone, Debug)]
struct Edge {
    card: usize,
    nodes: [String; 2],
    driven: bool,
}

fn edge(card: &Card, index: usize) -> Option<Edge> {
    let first = card.field(1)?;
    let second = card.field(2)?;
    if !simple_name(card.first())
        || !simple_name(first)
        || !simple_name(second)
        || first.eq_ignore_ascii_case(second)
    {
        return None;
    }
    let driven = match card.kind()? {
        // Ideal independent zero-volt current sensors. No series resistance,
        // AC value, transient waveform or other side condition is accepted.
        b'V' => {
            let value = match card.fields.len() {
                4 => card.field(3)?,
                5 if card.field(3)?.eq_ignore_ascii_case("DC") => card.field(4)?,
                6 if card.field(3)?.eq_ignore_ascii_case("DC") && card.field(4)? == "=" => {
                    card.field(5)?
                }
                _ => return None,
            };
            if parse_spice_constant(value).ok()? != 0. {
                return None;
            }
            false
        }
        b'E' => {
            // Native voltage-defined linear VCVS, or VALUE expression.
            if expression(card).is_none() && card.fields.len() != 6 {
                return None;
            }
            true
        }
        b'B' if card.field(3)?.eq_ignore_ascii_case("V") && expression(card).is_some() => true,
        _ => return None,
    };
    Some(Edge {
        card: index,
        nodes: [first.to_ascii_lowercase(), second.to_ascii_lowercase()],
        driven,
    })
}

fn capacitor(card: &Card) -> Option<[String; 2]> {
    if card.kind()? != b'C' || card.fields.len() < 4 || !simple_name(card.first()) {
        return None;
    }
    let (options, remainder) = card.fields.get(4..)?.as_chunks::<3>();
    if !remainder.is_empty()
        || !options.iter().all(|assignment| {
            let key = &card.text()[assignment[0].clone()];
            let equals = &card.text()[assignment[1].clone()];
            key.eq_ignore_ascii_case("IC") && equals == "="
        })
    {
        return None;
    }
    let p = card.field(1)?;
    let n = card.field(2)?;
    if !simple_name(p) || !simple_name(n) || p.eq_ignore_ascii_case(n) {
        return None;
    }
    Some([p.to_ascii_lowercase(), n.to_ascii_lowercase()])
}

/// Return the expression value's span for a supported behavioral source.
/// All occurrences of a scaled current must be inside these expressions.
fn expression(card: &Card) -> Option<Range<usize>> {
    let field = card.field(3)?;
    let valid = match card.kind()? {
        b'B' => field.eq_ignore_ascii_case("I") || field.eq_ignore_ascii_case("V"),
        b'E' | b'G' => field.eq_ignore_ascii_case("VALUE"),
        _ => false,
    };
    if !valid {
        return None;
    }
    let start = if card.field(4)? == "=" { 5 } else { 4 };
    // B sources may be an unquoted expression with whitespace; all the
    // remaining source text belongs to the expression.
    Some(card.fields.get(start)?.start..card.text().len())
}

fn current_output(card: &Card) -> bool {
    match card.kind() {
        Some(b'G') => card
            .field(3)
            .is_some_and(|field| field.eq_ignore_ascii_case("VALUE")),
        Some(b'B') => card
            .field(3)
            .is_some_and(|field| field.eq_ignore_ascii_case("I")),
        _ => false,
    }
}

struct Loop {
    capacitor: usize,
    nodes: BTreeSet<String>,
    /// Voltage edges and the direction relative to positive capacitor current.
    branches: Vec<(usize, f64)>,
}

struct VoltageGraph {
    edges: Vec<Edge>,
    adjacency: BTreeMap<String, Vec<usize>>,
}

impl VoltageGraph {
    fn new(source: &Source, scope: &Scope) -> Self {
        let mut edges = Vec::new();
        let mut adjacency = BTreeMap::<String, Vec<usize>>::new();
        for &index in &scope.cards {
            if !source.waveform_sources.contains(&index)
                && let Some(edge) = edge(&source.cards[index], index)
            {
                for node in &edge.nodes {
                    adjacency.entry(node.clone()).or_default().push(edges.len());
                }
                edges.push(edge);
            }
        }
        Self { edges, adjacency }
    }

    fn find_loop(&self, capacitor: usize, nodes: &[String; 2]) -> Option<Loop> {
        // Walk from capacitor N to P. A shortest simple path is sufficient:
        // the exposure proof rejects any additional electrical connection at
        // more than one cycle node, including a competing parallel path.
        let degree = |node: &String| self.adjacency.get(node).map_or(0, Vec::len);
        let (start, target, orientation) = if degree(&nodes[1]) <= degree(&nodes[0]) {
            (&nodes[1], &nodes[0], 1.)
        } else {
            (&nodes[0], &nodes[1], -1.)
        };
        let mut queue = VecDeque::from([start.clone()]);
        let mut previous =
            BTreeMap::<String, Option<(usize, String)>>::from([(start.clone(), None)]);
        while let Some(node) = queue.pop_front() {
            if node == *target {
                break;
            }
            for &index in self.adjacency.get(&node).into_iter().flatten() {
                let edge = &self.edges[index];
                let next = if edge.nodes[0] == node {
                    &edge.nodes[1]
                } else {
                    &edge.nodes[0]
                };
                if !previous.contains_key(next) {
                    previous.insert(next.clone(), Some((index, node.clone())));
                    queue.push_back(next.clone());
                }
            }
        }
        let mut node = target.clone();
        let mut vertices = BTreeSet::from([node.clone()]);
        let mut branches = Vec::new();
        let mut driven = false;
        while let Some((index, from)) = previous.get(&node)? {
            let edge = &self.edges[*index];
            driven |= edge.driven;
            let sign = if edge.nodes[0] == *from { 1. } else { -1. };
            branches.push((edge.card, sign * orientation));
            node.clone_from(from);
            vertices.insert(node.clone());
        }
        if !driven {
            return None;
        }
        Some(Loop {
            capacitor,
            nodes: vertices,
            branches,
        })
    }
}

#[derive(Clone, Debug)]
struct Probe {
    span: Range<usize>,
    name_span: Range<usize>,
    name: String,
    current: bool,
}

fn probes(card: &Card) -> Vec<Probe> {
    let mut result = Vec::new();
    for (symbol, current) in [(b"i", true), (b"v", false)] {
        for name in lexical::function_names(&card.statement.text, symbol, 0) {
            let text = card.text();
            let tail = text[name.end..].trim_start();
            let open = text.len() - tail.len();
            let Some(close) = tail.find(')') else {
                continue;
            };
            let inner = tail[1..close].trim();
            // A voltage probe may have two node arguments; a current
            // probe must name exactly one simple branch.
            if current && !simple_name(inner) {
                continue;
            }
            let start = open + 1 + tail[1..close].len() - tail[1..close].trim_start().len();
            result.push(Probe {
                span: name.start..open + close + 1,
                name_span: start..start + inner.len(),
                name: inner.to_ascii_lowercase(),
                current,
            });
        }
    }
    result.sort_by_key(|probe| probe.span.start);
    result
}

const fn contains(outer: &Range<usize>, inner: &Range<usize>) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}

struct Proof<'a> {
    source: &'a Source,
    circuit: &'a Loop,
    currents: BTreeMap<String, f64>,
    members: BTreeSet<usize>,
}

impl<'a> Proof<'a> {
    fn new(source: &'a Source, circuit: &'a Loop) -> Option<Self> {
        let cap = &source.cards[circuit.capacitor];
        let mut currents = BTreeMap::from([(cap.name(), 1.)]);
        let mut members = BTreeSet::from([circuit.capacitor]);
        for &(index, sign) in &circuit.branches {
            if currents.insert(source.cards[index].name(), sign).is_some() {
                return None;
            }
            members.insert(index);
        }
        Some(Self {
            source,
            circuit,
            currents,
            members,
        })
    }

    fn exposed(&self, node: &str, scope: usize) -> bool {
        if self.source.globals.contains(node)
            || node.starts_with("$g_")
            || self.source.scopes[scope].ports.contains(node)
        {
            return true;
        }
        for &index in self.source.references.get(node).into_iter().flatten() {
            let card = &self.source.cards[index];
            if card.scope != scope {
                continue;
            }
            for span in card.mentions(node) {
                // A member's output terminals are cycle edges.
                if self.members.contains(&index)
                    && [1, 2].iter().any(|&field| {
                        card.fields
                            .get(field)
                            .is_some_and(|field| contains(field, span))
                    })
                {
                    continue;
                }
                // The controlling terminals of a linear dependent source
                // are ideal voltage reads; they do not inject current.
                if matches!(card.kind(), Some(b'E' | b'G'))
                    && card.fields.len() == 6
                    && expression(card).is_none()
                    && [3, 4].iter().any(|&field| {
                        card.fields
                            .get(field)
                            .is_some_and(|field| contains(field, span))
                    })
                {
                    continue;
                }
                if expression(card).is_some_and(|expr| {
                    card.probes.iter().any(|probe| {
                        !probe.current
                            && contains(&expr, &probe.span)
                            && contains(&probe.name_span, span)
                    })
                }) {
                    continue;
                }
                return true;
            }
        }
        false
    }

    fn private_nodes(&self) -> Option<Vec<&str>> {
        let scope = self.source.cards[self.circuit.capacitor].scope;
        let mut private = Vec::new();
        let mut anchor = None;
        for node in &self.circuit.nodes {
            if self.exposed(node, scope) {
                if anchor.replace(node).is_some() {
                    return None;
                }
            } else {
                private.push(node.as_str());
            }
        }
        Some(private)
    }

    fn uses(&self, index: usize, card: &Card) -> Option<Vec<Probe>> {
        let mut uses = Vec::new();
        let expr = expression(card);
        for name in self.currents.keys() {
            for span in card.mentions(name) {
                if self.members.contains(&index)
                    && card.fields.first().is_some_and(|field| field == span)
                {
                    continue;
                }
                let probe = card.probes.iter().find(|probe| {
                    probe.current
                        && probe.name == *name
                        && &probe.name_span == span
                        && expr
                            .as_ref()
                            .is_some_and(|expr| contains(expr, &probe.span))
                })?;
                uses.push(probe.clone());
            }
        }
        uses.sort_by_key(|probe| probe.span.start);
        Some(uses)
    }

    fn outside_reference(&self, card: &Card, private_nodes: &[&str]) -> bool {
        // A hierarchical branch reference is not resolved by the local
        // name map. Reject rather than changing a caller's observable.
        // A source function can be expanded in a different lexical scope.
        // Without resolving that expansion, its local current observation
        // cannot be guaranteed to use the original coordinate.
        if card.first().eq_ignore_ascii_case(".func")
            && self
                .currents
                .keys()
                .any(|name| !card.mentions(name).is_empty())
        {
            return true;
        }
        self.currents
            .keys()
            .map(String::as_str)
            .chain(private_nodes.iter().copied())
            .any(|name| {
                card.mentions(name).iter().any(|span| {
                    card.text().as_bytes().get(span.start.wrapping_sub(1)) == Some(&b'.')
                        || card.text().as_bytes().get(span.start.wrapping_sub(1)) == Some(&b'@')
                        || matches!(
                            card.text().as_bytes().get(span.end),
                            Some(b'.' | b'#' | b'[')
                        )
                })
            })
    }

    fn evaluate_feedback(&self, card: &Card, polarity: f64) -> Result<f64> {
        let span = expression(card).context("missing feedback expression")?;
        let mut text = card.text()[span.clone()].to_owned();
        for probe in card
            .probes
            .iter()
            .rev()
            .filter(|probe| contains(&span, &probe.span))
        {
            let value = if probe.current {
                polarity * self.currents.get(&probe.name).copied().unwrap_or_default()
            } else {
                0.
            };
            text.replace_range(
                probe.span.start - span.start..probe.span.end - span.start,
                &format!("({value})"),
            );
        }
        self.source
            .parameters
            .evaluate_scalar(card.scope, &text, &[])
    }

    fn estimate(&self, card: &Card) -> Option<f64> {
        // Gain is only a scale-selection heuristic. The proof and all
        // current substitutions remain exact even for nonlinear feedback.
        let zero = self.evaluate_feedback(card, 0.).ok()?;
        let positive = self.evaluate_feedback(card, 1.).ok()?;
        let negative = self.evaluate_feedback(card, -1.).ok()?;
        Some((positive - zero).abs().max((negative - zero).abs()))
    }
}

struct Plan {
    artifact: usize,
    edits: Vec<Edit>,
    scaling: CurrentScaling,
    members: BTreeSet<usize>,
}

fn plan(source: &Source, circuit: &Loop) -> Option<Plan> {
    if source.constrained_capacitors.contains(&circuit.capacitor) {
        return None;
    }
    let proof = Proof::new(source, circuit)?;
    let private_nodes = proof.private_nodes()?;
    let cap = &source.cards[circuit.capacitor];
    let original = source
        .parameters
        .evaluate_scalar(cap.scope, cap.field(3)?, &[])
        .ok()?;
    if !original.is_normal() || original <= 0. {
        return None;
    }
    let mut uses = Vec::new();
    let mut factor = 0.0_f64;
    let referenced = proof
        .currents
        .keys()
        .map(String::as_str)
        .chain(private_nodes.iter().copied())
        .flat_map(|name| source.references.get(name).into_iter().flatten().copied())
        .collect::<BTreeSet<_>>();
    for index in referenced {
        let card = &source.cards[index];
        if card.scope != cap.scope {
            if proof.outside_reference(card, &private_nodes) {
                return None;
            }
            continue;
        }
        let refs = proof.uses(index, card)?;
        if !refs.is_empty() {
            if current_output(card) {
                factor = factor.max(proof.estimate(card)?);
            }
            uses.push((card, refs));
        }
    }
    // Normalize only downwards. A gain >= 1 does not justify reducing the
    // differentiator's current scale. Preserve subnormal/overflow cases.
    if !factor.is_normal()
        || factor <= 0.
        || factor >= 1.
        || !(original * factor).is_normal()
        || !factor.recip().is_normal()
    {
        return None;
    }
    let mut edits = Vec::new();
    let old = cap.field(3)?;
    let new = parse_spice_constant(old).map_or_else(
        |_| {
            let old = old
                .strip_prefix('{')
                .and_then(|s| s.strip_suffix('}'))
                .or_else(|| old.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
                .unwrap_or(old);
            format!("{{({old})*{factor:.17e}}}")
        },
        |value| format!("{:.17e}", value * factor),
    );
    lexical::replace_span(
        &cap.statement,
        cap.fields[3].clone(),
        new.as_bytes(),
        &mut edits,
    );
    for (card, refs) in uses {
        for probe in refs {
            let replacement = format!("({}/{factor:.17e})", &card.text()[probe.span.clone()]);
            lexical::replace_span(
                &card.statement,
                probe.span,
                replacement.as_bytes(),
                &mut edits,
            );
        }
    }
    Some(Plan {
        artifact: cap.artifact,
        edits,
        scaling: CurrentScaling {
            scope: source.scopes[cap.scope].path.clone(),
            capacitor: cap.first().to_owned(),
            voltage_branches: circuit
                .branches
                .iter()
                .map(|&(index, _)| source.cards[index].first().to_owned())
                .collect(),
            factor,
        },
        members: proof.members,
    })
}

/// Condition a closed set of already native generated electrical artifacts.
///
/// Unsupported or unprovable circuits are left unchanged. The original
/// bytes are never mutated. Root circuits are intentionally not candidates:
/// their branch currents are externally visible by definition.
///
/// # Errors
/// Internal source edit invariants fail.
pub fn condition_private_differentiators(
    inputs: &[ConditioningInput<'_>],
    initialization: Option<&crate::passive_ic::InitialConditions>,
    waveforms: &[crate::waveform::PreparedWaveform],
) -> Result<Vec<ConditionedArtifact>> {
    let mut result = inputs
        .iter()
        .map(|bytes| ConditionedArtifact {
            bytes: bytes.bytes.to_vec(),
            scalings: Vec::new(),
        })
        .collect::<Vec<_>>();
    let Ok(source) = Source::parse(inputs, initialization, waveforms) else {
        return Ok(result);
    };
    let mut plans = Vec::new();
    let mut used = BTreeSet::new();
    for scope in source.scopes.iter().skip(1) {
        let graph = VoltageGraph::new(&source, scope);
        for &index in &scope.cards {
            let Some(nodes) = capacitor(&source.cards[index]) else {
                continue;
            };
            let Some(circuit) = graph.find_loop(index, &nodes) else {
                continue;
            };
            let Some(plan) = plan(&source, &circuit) else {
                continue;
            };
            if plan.members.iter().any(|member| used.contains(member)) {
                continue;
            }
            used.extend(plan.members.iter().copied());
            plans.push(plan);
        }
    }
    for (artifact, output) in result.iter_mut().enumerate() {
        let mut edits = Vec::new();
        for plan in plans.iter_mut().filter(|plan| plan.artifact == artifact) {
            edits.append(&mut plan.edits);
            output.scalings.push(plan.scaling.clone());
        }
        edits.sort_by_key(|(span, _)| (span.start, span.end));
        ensure!(
            edits
                .windows(2)
                .all(|pair| pair[0].0.end <= pair[1].0.start),
            "overlapping conditioning edits"
        );
        if let Some(bytes) = lexical::apply_edits(inputs[artifact].bytes, edits) {
            output.bytes = bytes;
        }
    }
    Ok(result)
}
