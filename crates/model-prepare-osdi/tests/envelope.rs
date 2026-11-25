//! Independent ELF fixtures emitted by the platform C compiler.
#![cfg(target_os = "linux")]

use anyhow::{Result, ensure};
use model_prepare_osdi::{Abi, inspect};
use std::{fs, process::Command};

fn shared_object(source: &str) -> Result<Vec<u8>> {
    let directory = tempfile::tempdir()?;
    let input = directory.path().join("module.c");
    let output = directory.path().join("module.osdi");
    fs::write(&input, source)?;
    let result = Command::new("cc")
        .args(["-shared", "-fPIC", "-o"])
        .arg(&output)
        .arg(&input)
        .output()?;
    ensure!(result.status.success(), "C fixture compiler: {result:?}");
    Ok(fs::read(output)?)
}

const PREFIX: &str = "#include <stdint.h>\n#define BASE (sizeof(void*) == 8 ? 240 : 144)\nconst uint32_t OSDI_VERSION_MAJOR = 0;\n";

#[test]
fn validates_fixed_and_strided_descriptor_tables_without_loading() -> Result<()> {
    let old = shared_object(&format!(
        "{PREFIX}\nconst uint32_t OSDI_VERSION_MINOR=3, OSDI_NUM_DESCRIPTORS=2;\n_Alignas(sizeof(void*)) const char OSDI_DESCRIPTORS[2][BASE] = {{{{1}}}};"
    ))?;
    let old = inspect(&old)?;
    assert_eq!(old.abi, Abi::V03);
    assert_eq!(old.descriptor_count, 2);
    assert_eq!(
        old.descriptor_stride,
        if old.pointer_bytes == 8 { 240 } else { 144 }
    );
    let new = shared_object(&format!(
        "{PREFIX}\nconst uint32_t OSDI_VERSION_MINOR=4, OSDI_NUM_DESCRIPTORS=2, OSDI_DESCRIPTOR_SIZE=BASE+16;\n_Alignas(sizeof(void*)) const char OSDI_DESCRIPTORS[2][BASE+16] = {{{{1}}}};\n__attribute__((constructor)) static void never_execute(void) {{ __builtin_trap(); }}"
    ))?;
    let new = inspect(&new)?;
    assert_eq!(new.abi, Abi::V04);
    assert_eq!(new.descriptor_count, 2);
    assert_eq!(new.descriptor_stride, old.descriptor_stride + 16);
    assert_ne!(old.sha256, new.sha256);
    Ok(())
}

#[test]
fn rejects_unsupported_versions_missing_stride_and_truncated_arrays() -> Result<()> {
    for (minor, stride, count, extent) in [
        (5, "", 1, "BASE"),
        (4, "", 1, "BASE"),
        (
            4,
            "const uint32_t OSDI_DESCRIPTOR_SIZE=BASE+16;",
            2,
            "BASE+16",
        ),
        (3, "", 0, "BASE"),
    ] {
        let source = format!(
            "{PREFIX}\nconst uint32_t OSDI_VERSION_MINOR={minor}, OSDI_NUM_DESCRIPTORS={count};\n{stride}\n_Alignas(sizeof(void*)) const char OSDI_DESCRIPTORS[{extent}]={{1}};"
        );
        let bytes = shared_object(&source)?;
        assert!(inspect(&bytes).is_err(), "{source}");
    }
    Ok(())
}
