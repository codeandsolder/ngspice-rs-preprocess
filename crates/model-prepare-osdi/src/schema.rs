//! Static device metadata, separate from setup-time parameter values.

use super::{
    Abi, ModuleEnvelope,
    image::{Cursor, Image},
    inspect, required,
};
use anyhow::{Context, Result, bail, ensure};
use std::collections::BTreeSet;

/// Native unknown and residual units, including non-electrical disciplines.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Node {
    pub name: String,
    pub units: String,
    pub residual_units: String,
    pub binding: NodeBinding,
    pub kind: NodeKind,
    pub resistive_offset: Option<u32>,
    pub reactive_offset: Option<u32>,
    pub resistive_limit_offset: Option<u32>,
    pub reactive_limit_offset: Option<u32>,
}

/// Circuit-visible or device-local unknown.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeBinding {
    Terminal,
    Hidden,
}

/// Across-variable or through-variable coordinate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeKind {
    Potential,
    Flow,
}

/// Ordered local coordinate pair; absent second coordinate denotes ground.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodePair {
    pub first: u32,
    pub second: Option<u32>,
}

/// A sparse Jacobian coordinate and the ABI's contribution properties.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JacobianEntry {
    pub row: u32,
    pub column: u32,
    pub resistive: bool,
    pub reactive_offset: Option<u32>,
    pub resistive_constant: bool,
    pub reactive_constant: bool,
}

/// Noise source class supplied by OSDI 0.4.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NoiseKind {
    White,
    Flicker,
    Table,
}

/// An independently generated noise source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NoiseSource {
    pub name: String,
    pub nodes: NodePair,
    pub kind: Option<NoiseKind>,
}

/// Allocation and mapping offsets relative to the opaque instance buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InstanceLayout {
    pub bytes: u32,
    pub node_mapping: u32,
    pub resistive_jacobian_mapping: u32,
    pub collapse_mapping: u32,
    pub state_mapping: u32,
    pub states: u32,
    pub bound_step: Option<u32>,
}

/// The value representation is independent of model or instance ownership.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValueType {
    Real,
    Integer,
    String,
}

/// Original ABI parameter namespace.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParameterKind {
    Model,
    Instance,
    OperatingPoint,
}

/// A parameter slot and its aliases. Defaults depend on setup callbacks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Parameter {
    pub id: u32,
    pub names: Vec<String>,
    pub description: String,
    pub units: String,
    pub kind: ParameterKind,
    pub value_type: ValueType,
    /// Zero denotes a scalar; otherwise the element count.
    pub array_len: u32,
}

/// A statically validated descriptor prefix.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Device {
    pub name: String,
    pub nodes: Vec<Node>,
    pub parameters: Vec<Parameter>,
    pub instance: InstanceLayout,
    pub model_bytes: u32,
    pub jacobian: Vec<JacobianEntry>,
    pub collapsible_nodes: Vec<NodePair>,
    pub noise_sources: Vec<NoiseSource>,
    pub charge: bool,
    pub uses_absolute_time: Option<bool>,
}

/// Metadata tied to the exact compiled artifact bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleSchema {
    envelope: ModuleEnvelope,
    devices: Vec<Device>,
}

impl ModuleSchema {
    /// Validated envelope of the exact inspected binary.
    #[must_use = "the envelope identifies the inspected module and ABI"]
    pub const fn envelope(&self) -> &ModuleEnvelope {
        &self.envelope
    }

    /// Ordered device descriptors, immutable after validation.
    #[must_use = "the descriptors are inputs to parameter and topology binding"]
    pub fn devices(&self) -> &[Device] {
        &self.devices
    }
}

fn checked_offset(offset: u32, count: u32, width: u64, size: u32) -> Result<()> {
    if count != 0 {
        ensure!(
            offset != u32::MAX && u64::from(offset) + u64::from(count) * width <= u64::from(size),
            "OSDI instance offset outside allocation"
        );
    }
    Ok(())
}

fn optional_offset(offset: u32, size: u32) -> Result<Option<u32>> {
    if offset == u32::MAX {
        return Ok(None);
    }
    checked_offset(offset, 1, 8, size)?;
    Ok(Some(offset))
}

fn nodes(
    image: &Image<'_>,
    address: u64,
    count: u32,
    terminals: u32,
    size: u32,
) -> Result<Vec<Node>> {
    let stride = (3 * image.pointer + 17).next_multiple_of(image.pointer);
    image.table(address, count, stride)?;
    let mut result = Vec::new();
    let mut names = BTreeSet::new();
    for i in 0..count {
        let mut cursor = Cursor::new(image, address + u64::from(i) * stride);
        let name = cursor.string()?;
        ensure!(
            !name.is_empty() && names.insert(name.clone()),
            "invalid or duplicate node name {name:?}"
        );
        let units = cursor.string()?;
        let residual_units = cursor.string()?;
        let resistive_offset = optional_offset(cursor.u32()?, size)?;
        let reactive_offset = optional_offset(cursor.u32()?, size)?;
        let resistive_limit_offset = optional_offset(cursor.u32()?, size)?;
        let reactive_limit_offset = optional_offset(cursor.u32()?, size)?;
        let kind = match cursor.byte()? {
            0 => NodeKind::Potential,
            1 => NodeKind::Flow,
            _ => bail!("invalid OSDI flow flag"),
        };
        result.push(Node {
            name,
            units,
            residual_units,
            binding: if i < terminals {
                NodeBinding::Terminal
            } else {
                NodeBinding::Hidden
            },
            kind,
            resistive_offset,
            reactive_offset,
            resistive_limit_offset,
            reactive_limit_offset,
        });
    }
    Ok(result)
}

fn parameters(
    image: &Image<'_>,
    address: u64,
    total: u32,
    instances: u32,
    opvars: u32,
) -> Result<Vec<Parameter>> {
    let count = total
        .checked_add(opvars)
        .context("parameter count overflow")?;
    let stride = (image.pointer + 4).next_multiple_of(image.pointer) + 2 * image.pointer + 8;
    image.table(address, count, stride)?;
    let mut result = Vec::new();
    for id in 0..count {
        let mut cursor = Cursor::new(image, address + u64::from(id) * stride);
        let names_address = cursor.pointer()?;
        let name_count = cursor
            .u32()?
            .checked_add(1)
            .context("alias count overflow")?;
        image.table(names_address, name_count, image.pointer)?;
        let mut names = Vec::new();
        for i in 0..name_count {
            let name =
                image.string(image.pointer(names_address + u64::from(i) * image.pointer)?)?;
            ensure!(!name.is_empty(), "empty OSDI parameter name");
            names.push(name);
        }
        let description = cursor.string()?;
        let units = cursor.string()?;
        let flags = cursor.u32()?;
        ensure!(
            flags & !0xc000_0003 == 0,
            "unsupported parameter flags {flags:#x}"
        );
        let value_type = match flags & 3 {
            0 => ValueType::Real,
            1 => ValueType::Integer,
            2 => ValueType::String,
            _ => bail!("unsupported OSDI parameter type"),
        };
        let kind = if id < instances {
            ParameterKind::Instance
        } else if id < total {
            ParameterKind::Model
        } else {
            ParameterKind::OperatingPoint
        };
        let expected = match kind {
            ParameterKind::Model => 0,
            ParameterKind::Instance => 1,
            ParameterKind::OperatingPoint => 2,
        };
        ensure!(
            flags >> 30 == expected,
            "parameter scope disagrees with descriptor partition"
        );
        result.push(Parameter {
            id,
            names,
            description,
            units,
            kind,
            value_type,
            array_len: cursor.u32()?,
        });
    }
    Ok(result)
}

struct Header {
    name: String,
    node_count: u32,
    terminals: u32,
    nodes: u64,
    jacobian_count: u32,
    jacobian: u64,
    collapse_count: u32,
    collapse: u64,
    collapse_offset: u32,
    noise: u64,
    noise_count: u32,
    parameters: u32,
    instances: u32,
    opvars: u32,
    parameter_table: u64,
    node_mapping: u32,
    jacobian_mapping: u32,
    states: u32,
    state_mapping: u32,
    step: u32,
    instance_bytes: u32,
    model_bytes: u32,
}

impl Header {
    fn read(c: &mut Cursor<'_, '_>) -> Result<Self> {
        Ok(Self {
            name: c.string()?,
            node_count: c.u32()?,
            terminals: c.u32()?,
            nodes: c.pointer()?,
            jacobian_count: c.u32()?,
            jacobian: c.pointer()?,
            collapse_count: c.u32()?,
            collapse: c.pointer()?,
            collapse_offset: c.u32()?,
            noise: c.pointer()?,
            noise_count: c.u32()?,
            parameters: c.u32()?,
            instances: c.u32()?,
            opvars: c.u32()?,
            parameter_table: c.pointer()?,
            node_mapping: c.u32()?,
            jacobian_mapping: c.u32()?,
            states: c.u32()?,
            state_mapping: c.u32()?,
            step: c.u32()?,
            instance_bytes: c.u32()?,
            model_bytes: c.u32()?,
        })
    }

    fn validate(&self, image: &Image<'_>) -> Result<()> {
        ensure!(
            !self.name.is_empty()
                && self.terminals <= self.node_count
                && self.instances <= self.parameters,
            "invalid OSDI descriptor counts"
        );
        checked_offset(self.node_mapping, self.node_count, 4, self.instance_bytes)?;
        checked_offset(
            self.jacobian_mapping,
            self.jacobian_count,
            image.pointer,
            self.instance_bytes,
        )?;
        checked_offset(
            self.collapse_offset,
            self.collapse_count,
            1,
            self.instance_bytes,
        )?;
        checked_offset(self.state_mapping, self.states, 4, self.instance_bytes)?;
        optional_offset(self.step, self.instance_bytes)?;
        image.table(self.jacobian, self.jacobian_count, 16)?;
        image.table(self.collapse, self.collapse_count, 8)?;
        image.table(self.noise, self.noise_count, image.pointer + 8)?;
        Ok(())
    }
}

fn node_pair(c: &mut Cursor<'_, '_>, count: u32, ground: bool) -> Result<NodePair> {
    let first = c.u32()?;
    let second = c.u32()?;
    ensure!(
        first < count && (second < count || (ground && second == u32::MAX)),
        "invalid OSDI coordinate pair"
    );
    Ok(NodePair {
        first,
        second: (second != u32::MAX).then_some(second),
    })
}

fn jacobian(image: &Image<'_>, h: &Header) -> Result<Vec<JacobianEntry>> {
    let mut entries = Vec::new();
    let mut coordinates = BTreeSet::new();
    for i in 0..h.jacobian_count {
        let mut c = Cursor::new(image, h.jacobian + u64::from(i) * 16);
        let pair = node_pair(&mut c, h.node_count, false)?;
        let column = pair.second.context("Jacobian ground column")?;
        ensure!(
            coordinates.insert((pair.first, column)),
            "duplicate OSDI Jacobian coordinate"
        );
        let offset = c.u32()?;
        let flags = c.u32()?;
        ensure!(flags & !15 == 0, "unsupported OSDI Jacobian flags");
        let reactive = flags & 8 != 0;
        ensure!(
            reactive == (offset != u32::MAX),
            "reactive Jacobian offset disagrees with flags"
        );
        if reactive {
            checked_offset(offset, 1, image.pointer, h.instance_bytes)?;
        }
        entries.push(JacobianEntry {
            row: pair.first,
            column,
            resistive: flags & 4 != 0,
            reactive_offset: reactive.then_some(offset),
            resistive_constant: flags & 1 != 0,
            reactive_constant: flags & 2 != 0,
        });
    }
    Ok(entries)
}

fn collapse_pairs(image: &Image<'_>, h: &Header) -> Result<Vec<NodePair>> {
    (0..h.collapse_count)
        .map(|index| {
            node_pair(
                &mut Cursor::new(image, h.collapse + u64::from(index) * 8),
                h.node_count,
                true,
            )
        })
        .collect()
}

fn noise_sources(image: &Image<'_>, h: &Header) -> Result<Vec<NoiseSource>> {
    let stride = image.pointer + 8;
    (0..h.noise_count)
        .map(|index| {
            let mut c = Cursor::new(image, h.noise + u64::from(index) * stride);
            Ok(NoiseSource {
                name: c.string()?,
                nodes: node_pair(&mut c, h.node_count, true)?,
                kind: None,
            })
        })
        .collect()
}

fn callbacks(
    c: &mut Cursor<'_, '_>,
    abi: Abi,
    header: &Header,
    charge: bool,
    noise_sources: &mut [NoiseSource],
) -> Result<Option<bool>> {
    let mut present = [false; 14];
    for slot in &mut present {
        *slot = c.callback()?;
    }
    ensure!(
        present[0..4].iter().all(|&value| value) && present[5] && present[11],
        "missing mandatory OSDI callback"
    );
    ensure!(
        !charge || (present[6] && present[12]),
        "missing reactive OSDI callback"
    );
    ensure!(
        header.noise_count == 0 || present[4],
        "missing noise callback"
    );
    if abi == Abi::V03 {
        return Ok(None);
    }
    c.callback()?;
    c.callback()?;
    let resistive = c.u32()?;
    let reactive = c.u32()?;
    ensure!(
        resistive <= header.jacobian_count && reactive <= header.jacobian_count,
        "invalid OSDI Jacobian counts"
    );
    c.callback()?;
    c.callback()?;
    let inputs = c.u32()?;
    let input_table = c.pointer()?;
    c.image.table(input_table, inputs, 8)?;
    c.callback()?;
    c.callback()?;
    let unknown = c.pointer()?;
    let residual = c.pointer()?;
    let noise = c.pointer()?;
    c.image.table(unknown, header.node_count, 8)?;
    c.image.table(residual, header.node_count, 8)?;
    c.image.table(noise, header.noise_count, 4)?;
    for (index, source) in noise_sources.iter_mut().enumerate() {
        source.kind = Some(match c.image.u32(noise + u64::try_from(index)? * 4)? {
            0 => NoiseKind::White,
            1 => NoiseKind::Flicker,
            2 => NoiseKind::Table,
            _ => bail!("unsupported OSDI noise source type"),
        });
    }
    c.callback()?;
    let flags = c.u32()?;
    ensure!(flags & !1 == 0, "unsupported OSDI module flags");
    Ok(Some(flags & 1 != 0))
}

fn device(image: &Image<'_>, address: u64, abi: Abi) -> Result<Device> {
    let mut c = Cursor::new(image, address);
    let h = Header::read(&mut c)?;
    h.validate(image)?;
    let nodes = nodes(image, h.nodes, h.node_count, h.terminals, h.instance_bytes)?;
    let parameters = parameters(
        image,
        h.parameter_table,
        h.parameters,
        h.instances,
        h.opvars,
    )?;
    let jacobian = jacobian(image, &h)?;
    let collapsible_nodes = collapse_pairs(image, &h)?;
    let mut noise_sources = noise_sources(image, &h)?;
    let charge = nodes.iter().any(|node| node.reactive_offset.is_some())
        || jacobian.iter().any(|entry| entry.reactive_offset.is_some());
    let uses_absolute_time = callbacks(&mut c, abi, &h, charge, &mut noise_sources)?;
    Ok(Device {
        name: h.name,
        nodes,
        parameters,
        instance: InstanceLayout {
            bytes: h.instance_bytes,
            node_mapping: h.node_mapping,
            resistive_jacobian_mapping: h.jacobian_mapping,
            collapse_mapping: h.collapse_offset,
            state_mapping: h.state_mapping,
            states: h.states,
            bound_step: (h.step != u32::MAX).then_some(h.step),
        },
        model_bytes: h.model_bytes,
        jacobian,
        collapsible_nodes,
        noise_sources,
        charge,
        uses_absolute_time,
    })
}

/// Read the device metadata and verify its ABI-local table extents and callbacks.
///
/// # Errors
/// Invalid envelope, unsupported relocation, malformed device schema or missing callback.
pub fn inspect_schema(bytes: &[u8]) -> Result<ModuleSchema> {
    let envelope = inspect(bytes)?;
    let file = object::File::parse(bytes)?;
    let table = required(&file, "OSDI_DESCRIPTORS")?;
    let image = Image::new(bytes)?;
    let mut devices = Vec::new();
    for index in 0..envelope.descriptor_count {
        devices.push(
            device(
                &image,
                table.address + u64::from(index) * u64::from(envelope.descriptor_stride),
                envelope.abi,
            )
            .with_context(|| format!("OSDI descriptor {index}"))?,
        );
    }
    Ok(ModuleSchema { envelope, devices })
}
