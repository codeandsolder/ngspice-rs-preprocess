//! Read relocated metadata as virtual addresses, without invoking a loader.

use super::{Export, data};
use anyhow::{Context, Result, bail, ensure};
use object::{
    Architecture, Object, ObjectSegment, ObjectSymbol, ObjectSymbolTable, Relocation,
    RelocationFlags, RelocationTarget, SegmentFlags, elf,
};
use std::collections::BTreeMap;

pub struct Image<'a> {
    file: object::File<'a>,
    relocations: BTreeMap<u64, Relocation>,
    pub pointer: u64,
}

impl<'a> Image<'a> {
    pub fn new(bytes: &'a [u8]) -> Result<Self> {
        let file = object::File::parse(bytes)?;
        let pointer = if file.is_64() { 8 } else { 4 };
        let mut relocations = BTreeMap::new();
        for (address, relocation) in file
            .dynamic_relocations()
            .context("missing dynamic relocations")?
        {
            ensure!(
                relocations.insert(address, relocation).is_none(),
                "duplicate ELF relocation at {address:#x}"
            );
        }
        Ok(Self {
            file,
            relocations,
            pointer,
        })
    }

    pub fn bytes(&self, address: u64, size: u64) -> Result<&'a [u8]> {
        ensure!(
            address != 0 && address.checked_add(size).is_some(),
            "invalid metadata address"
        );
        data(&self.file, Export { address, size })
    }

    pub fn table(&self, address: u64, count: u32, stride: u64) -> Result<()> {
        if count != 0 {
            self.bytes(
                address,
                u64::from(count)
                    .checked_mul(stride)
                    .context("metadata table overflow")?,
            )?;
        }
        Ok(())
    }

    fn integer(&self, address: u64, size: u64) -> Result<u64> {
        let bytes = self.bytes(address, size)?;
        let mut value = [0; 8];
        let size = usize::try_from(size)?;
        if self.file.is_little_endian() {
            value[..size].copy_from_slice(bytes);
            Ok(u64::from_le_bytes(value))
        } else {
            value[8 - size..].copy_from_slice(bytes);
            Ok(u64::from_be_bytes(value))
        }
    }

    pub fn u32(&self, address: u64) -> Result<u32> {
        Ok(u32::try_from(self.integer(address, 4)?)?)
    }

    fn relocation_base(&self, relocation: &Relocation) -> Result<u64> {
        let RelocationFlags::Elf { r_type } = relocation.flags() else {
            bail!("non-ELF pointer relocation");
        };
        let (relative, absolute) = match self.file.architecture() {
            Architecture::X86_64 => (elf::R_X86_64_RELATIVE, elf::R_X86_64_64),
            Architecture::I386 => (elf::R_386_RELATIVE, elf::R_386_32),
            Architecture::Aarch64 => (elf::R_AARCH64_RELATIVE, elf::R_AARCH64_ABS64),
            Architecture::Riscv64 => (elf::R_RISCV_RELATIVE, elf::R_RISCV_64),
            Architecture::Riscv32 => (elf::R_RISCV_RELATIVE, elf::R_RISCV_32),
            architecture => bail!("unsupported OSDI relocation architecture {architecture:?}"),
        };
        match relocation.target() {
            RelocationTarget::Absolute if r_type == relative => Ok(0),
            RelocationTarget::Symbol(index) if r_type == absolute => {
                let table = self
                    .file
                    .dynamic_symbol_table()
                    .context("missing dynamic symbol table")?;
                let symbol = table.symbol_by_index(index)?;
                ensure!(
                    symbol.is_definition(),
                    "metadata pointer refers to an external symbol"
                );
                Ok(symbol.address())
            }
            _ => bail!("unsupported OSDI metadata relocation {r_type:?}"),
        }
    }

    pub fn pointer(&self, address: u64) -> Result<u64> {
        let raw = self.integer(address, self.pointer)?;
        let Some(relocation) = self.relocations.get(&address) else {
            ensure!(
                raw == 0,
                "non-null metadata pointer without supported relocation at {address:#x}"
            );
            return Ok(0);
        };
        let addend = if relocation.has_implicit_addend() {
            if self.pointer == 4 {
                i64::from(i32::from_ne_bytes(u32::try_from(raw)?.to_ne_bytes()))
            } else {
                i64::from_ne_bytes(raw.to_ne_bytes())
            }
        } else {
            relocation.addend()
        };
        self.relocation_base(relocation)?
            .checked_add_signed(addend)
            .context("metadata pointer overflow")
    }

    pub fn string(&self, address: u64) -> Result<String> {
        ensure!(address != 0, "null metadata string");
        for segment in self.file.segments() {
            let Some(offset) = address.checked_sub(segment.address()) else {
                continue;
            };
            let bytes = segment.data()?;
            let Ok(offset) = usize::try_from(offset) else {
                continue;
            };
            let Some(tail) = bytes.get(offset..) else {
                continue;
            };
            if tail.is_empty() {
                continue;
            }
            let end = tail
                .iter()
                .position(|&byte| byte == 0)
                .context("unterminated metadata string")?;
            return Ok(std::str::from_utf8(&tail[..end])?.to_owned());
        }
        bail!("metadata string outside file-backed load segment")
    }

    pub fn callback(&self, address: u64) -> Result<bool> {
        let address = self.pointer(address)?;
        if address == 0 {
            return Ok(false);
        }
        ensure!(self.file.segments().any(|segment| {
            matches!(segment.flags(), SegmentFlags::Elf { p_flags, .. } if p_flags.contains(elf::PF_X))
                && address.checked_sub(segment.address()).is_some_and(|offset| offset < segment.file_range().1)
        }), "OSDI callback outside executable file-backed segment");
        Ok(true)
    }
}

/// Cursor for C ABI structs made of pointers, uint32 values and bytes.
pub struct Cursor<'i, 'a> {
    pub image: &'i Image<'a>,
    start: u64,
    offset: u64,
}

impl<'i, 'a> Cursor<'i, 'a> {
    pub const fn new(image: &'i Image<'a>, start: u64) -> Self {
        Self {
            image,
            start,
            offset: 0,
        }
    }

    fn field(&mut self, size: u64) -> Result<u64> {
        self.offset = self.offset.next_multiple_of(size);
        let address = self
            .start
            .checked_add(self.offset)
            .context("metadata field overflow")?;
        self.offset += size;
        Ok(address)
    }

    pub fn u32(&mut self) -> Result<u32> {
        let address = self.field(4)?;
        self.image.u32(address)
    }

    pub fn pointer(&mut self) -> Result<u64> {
        let address = self.field(self.image.pointer)?;
        self.image.pointer(address)
    }

    pub fn string(&mut self) -> Result<String> {
        let address = self.pointer()?;
        self.image.string(address)
    }

    pub fn byte(&mut self) -> Result<u8> {
        let address = self.field(1)?;
        Ok(self.image.bytes(address, 1)?[0])
    }

    pub fn callback(&mut self) -> Result<bool> {
        let address = self.field(self.image.pointer)?;
        self.image.callback(address)
    }
}
