//! Static preparation of compiled Verilog-A device modules. This stage never
//! loads a shared library or executes its constructors or model callbacks.

#![forbid(unsafe_code)]

use anyhow::{Context, Result, bail, ensure};
use object::{Object, ObjectKind, ObjectSegment, ObjectSymbol, SymbolKind, SymbolScope};
use sha2::{Digest, Sha256};
pub mod binding;
pub mod compiler;
mod image;
pub mod schema;

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

fn read_scalar(image: &image::Image<'_>, export: Export) -> Result<u32> {
    ensure!(export.size == 4, "OSDI scalar export must be a uint32");
    image.u32(export.address)
}

fn scalar(image: &image::Image<'_>, name: &str) -> Result<u32> {
    read_scalar(image, required(image.file(), name)?)
        .with_context(|| format!("reading OSDI export {name}"))
}

// ABI structure layouts are calculated using the target pointer width, not
// host sizeof. The 0.4 additions follow the entire 0.3 callback prefix.
fn descriptor_size(abi: Abi, pointer: u32) -> u32 {
    let prefix = [
        pointer, 4, 4, pointer, 4, pointer, 4, pointer, 4, pointer, 4, 4, 4, 4, pointer, 4, 4, 4,
        4, 4, 4, 4,
    ];
    let extension = [
        pointer, pointer, 4, 4, pointer, pointer, 4, pointer, pointer, pointer, pointer, pointer,
        pointer, pointer, 4,
    ];
    let extension = match abi {
        Abi::V03 => &extension[..0],
        Abi::V04 => extension.as_slice(),
    };
    let mut offset = 0_u32;
    for size in prefix
        .into_iter()
        .chain(std::iter::repeat_n(pointer, 14))
        .chain(extension.iter().copied())
    {
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
    let image = image::Image::new(bytes).context("invalid OSDI ELF module")?;
    inspect_image(&image, bytes)
}

fn inspect_image(image: &image::Image<'_>, bytes: &[u8]) -> Result<ModuleEnvelope> {
    let file = image.file();
    ensure!(
        file.kind() == ObjectKind::Dynamic && file.entry() == 0,
        "OSDI input must be a shared object"
    );
    let abi = match (
        scalar(image, "OSDI_VERSION_MAJOR")?,
        scalar(image, "OSDI_VERSION_MINOR")?,
    ) {
        (0, 3) => Abi::V03,
        (0, 4) => Abi::V04,
        (major, minor) => bail!("unsupported OSDI ABI {major}.{minor}"),
    };
    let pointer = if file.is_64() { 8_u8 } else { 4_u8 };
    let expected = descriptor_size(abi, u32::from(pointer));
    let stride = match (abi, export(file, "OSDI_DESCRIPTOR_SIZE")?) {
        (Abi::V03, None) => expected,
        (_, Some(size)) => {
            let stride = read_scalar(image, size)?;
            ensure!(
                stride == expected,
                "invalid OSDI descriptor stride: {stride}, expected {expected}"
            );
            stride
        }
        (Abi::V04, None) => bail!("OSDI 0.4 requires OSDI_DESCRIPTOR_SIZE"),
    };
    let count = scalar(image, "OSDI_NUM_DESCRIPTORS")?;
    let descriptors = required(file, "OSDI_DESCRIPTORS")?;
    ensure!(
        count != 0
            && descriptors.size == u64::from(count) * u64::from(stride)
            && descriptors.address.is_multiple_of(u64::from(pointer)),
        "OSDI descriptor array size or alignment mismatch"
    );
    data(file, descriptors)?;
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
        assert_eq!(descriptor_size(Abi::V03, 8), 240);
        assert_eq!(descriptor_size(Abi::V03, 4), 144);
        assert_eq!(descriptor_size(Abi::V04, 8), 352);
        assert_eq!(descriptor_size(Abi::V04, 4), 204);
    }

    #[test]
    fn arbitrary_input_is_not_a_module() {
        assert!(inspect(b"not a shared object").is_err());
    }
}
