//! Static preparation of compiled Verilog-A device modules. This stage never
//! loads a shared library or executes its constructors or model callbacks.

#![forbid(unsafe_code)]

use anyhow::{Context, Result, bail, ensure};
use object::{Object, ObjectKind, ObjectSegment, ObjectSymbol, SymbolKind, SymbolScope};
use sha2::{Digest, Sha256};

/// ABI revisions with a documented, inspectable descriptor layout.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Abi {
    V03,
    V04,
}

/// Binary identity and layout of a statically inspected ELF module. A valid
/// envelope is not yet a validated device schema or executable kernel.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleEnvelope {
    pub sha256: [u8; 32],
    pub architecture: object::Architecture,
    pub pointer_bytes: u8,
    pub little_endian: bool,
    pub abi: Abi,
    pub descriptor_count: u32,
    pub descriptor_stride: u32,
}

#[derive(Clone, Copy)]
struct Export {
    address: u64,
    size: u64,
}

fn export(file: &object::File<'_>, name: &str) -> Result<Option<Export>> {
    let mut found = None;
    for symbol in file.dynamic_symbols() {
        if symbol.name_bytes()? != name.as_bytes() {
            continue;
        }
        ensure!(found.is_none(), "ambiguous OSDI export {name}");
        ensure!(
            symbol.is_definition()
                && symbol.kind() == SymbolKind::Data
                && symbol.scope() == SymbolScope::Dynamic
                && !symbol.is_weak(),
            "OSDI export {name} is not a defined global data object"
        );
        found = Some(Export {
            address: symbol.address(),
            size: symbol.size(),
        });
    }
    Ok(found)
}

fn required(file: &object::File<'_>, name: &str) -> Result<Export> {
    export(file, name)?.with_context(|| format!("missing OSDI export {name}"))
}

fn data<'a>(file: &object::File<'a>, export: Export) -> Result<&'a [u8]> {
    for segment in file.segments() {
        if let Some(bytes) = segment.data_range(export.address, export.size)? {
            return Ok(bytes);
        }
    }
    bail!("OSDI export is not backed by module bytes")
}

fn read_scalar(file: &object::File<'_>, export: Export) -> Result<u32> {
    ensure!(export.size == 4, "OSDI scalar export must be a uint32");
    let bytes: [u8; 4] = data(file, export)?.try_into()?;
    Ok(if file.is_little_endian() {
        u32::from_le_bytes(bytes)
    } else {
        u32::from_be_bytes(bytes)
    })
}

fn scalar(file: &object::File<'_>, name: &str) -> Result<u32> {
    read_scalar(file, required(file, name)?).with_context(|| format!("reading OSDI export {name}"))
}

// OSDI 0.3 descriptor fields are pointers and uint32 values. Derive the
// C ABI layout from field groups rather than host sizeof or architecture.
fn prefix_size(pointer: u32) -> u32 {
    // Field sequence: name; node, Jacobian and collapse metadata; noise
    // metadata; parameter schema; instance layout; fourteen callbacks.
    let fields = [
        pointer, 4, 4, pointer, 4, pointer, 4, pointer, 4, pointer, 4, 4, 4, 4, pointer, 4, 4, 4,
        4, 4, 4, 4,
    ];
    let mut offset = 0_u32;
    for size in fields.into_iter().chain(std::iter::repeat_n(pointer, 14)) {
        offset = offset.next_multiple_of(size) + size;
    }
    offset.next_multiple_of(pointer)
}

/// Validate the ABI exports and descriptor array of an ELF module.
///
/// The caller chooses and reads the module explicitly; no dynamic loading
/// is performed.
///
/// # Errors
/// Invalid ELF, non-shared-object input, missing or malformed exports,
/// unsupported ABI revision or an inconsistent descriptor array.
pub fn inspect(bytes: &[u8]) -> Result<ModuleEnvelope> {
    let file = object::File::parse(bytes).context("invalid OSDI ELF module")?;
    ensure!(
        file.kind() == ObjectKind::Dynamic && file.entry() == 0,
        "OSDI input must be a shared object"
    );
    let abi = match (
        scalar(&file, "OSDI_VERSION_MAJOR")?,
        scalar(&file, "OSDI_VERSION_MINOR")?,
    ) {
        (0, 3) => Abi::V03,
        (0, 4) => Abi::V04,
        (major, minor) => bail!("unsupported OSDI ABI {major}.{minor}"),
    };
    let pointer = if file.is_64() { 8_u8 } else { 4_u8 };
    let minimum = prefix_size(u32::from(pointer));
    let stride = match (abi, export(&file, "OSDI_DESCRIPTOR_SIZE")?) {
        (Abi::V03, None) => minimum,
        (Abi::V03, Some(size)) => {
            let stride = read_scalar(&file, size)?;
            ensure!(stride == minimum, "invalid OSDI 0.3 descriptor stride");
            stride
        }
        (Abi::V04, Some(size)) => {
            let stride = read_scalar(&file, size)?;
            ensure!(
                stride > minimum && stride.is_multiple_of(u32::from(pointer)),
                "invalid OSDI 0.4 descriptor stride"
            );
            stride
        }
        (Abi::V04, None) => bail!("OSDI 0.4 requires OSDI_DESCRIPTOR_SIZE"),
    };
    let count = scalar(&file, "OSDI_NUM_DESCRIPTORS")?;
    let descriptors = required(&file, "OSDI_DESCRIPTORS")?;
    ensure!(
        count != 0
            && descriptors.size == u64::from(count) * u64::from(stride)
            && descriptors.address.is_multiple_of(u64::from(pointer)),
        "OSDI descriptor array size or alignment mismatch"
    );
    data(&file, descriptors)?;
    Ok(ModuleEnvelope {
        sha256: Sha256::digest(bytes).into(),
        architecture: file.architecture(),
        pointer_bytes: pointer,
        little_endian: file.is_little_endian(),
        abi,
        descriptor_count: count,
        descriptor_stride: stride,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_prefix_matches_documented_lp64_layout() {
        assert_eq!(prefix_size(8), 240);
        assert_eq!(prefix_size(4), 144);
    }

    #[test]
    fn arbitrary_input_is_not_a_module() {
        assert!(inspect(b"not a shared object").is_err());
    }
}
