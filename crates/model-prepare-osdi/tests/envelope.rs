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

const PREFIX: &str = "#include <stdint.h>\n#define BASE (sizeof(void*) == 8 ? 240 : 144)\n#define V04 (sizeof(void*) == 8 ? 352 : 204)\nconst uint32_t OSDI_VERSION_MAJOR = 0;\n";

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
        "{PREFIX}\nconst uint32_t OSDI_VERSION_MINOR=4, OSDI_NUM_DESCRIPTORS=2, OSDI_DESCRIPTOR_SIZE=V04;\n_Alignas(sizeof(void*)) const char OSDI_DESCRIPTORS[2][V04] = {{{{1}}}};\n__attribute__((constructor)) static void never_execute(void) {{ __builtin_trap(); }}"
    ))?;
    let new = inspect(&new)?;
    assert_eq!(new.abi, Abi::V04);
    assert_eq!(new.descriptor_count, 2);
    assert_eq!(
        new.descriptor_stride,
        if old.pointer_bytes == 8 { 352 } else { 204 }
    );
    assert_ne!(old.sha256, new.sha256);
    Ok(())
}

#[test]
fn rejects_unsupported_versions_missing_stride_and_truncated_arrays() -> Result<()> {
    for (minor, stride, count, extent) in [
        (5, "", 1, "BASE"),
        (4, "", 1, "BASE"),
        (4, "const uint32_t OSDI_DESCRIPTOR_SIZE=V04;", 2, "V04"),
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

const DEVICE: &str = include_str!("fixtures/device.c");

#[test]
fn static_schema_and_bindings_are_independent_of_loading() -> Result<()> {
    use model_prepare_osdi::{
        binding::{Assignment, Scalar, Scope, Value},
        schema::{NodeBinding, NoiseKind, inspect_schema},
    };
    for minor in [3, 4] {
        let bytes = shared_object(&format!("#define MINOR {minor}\n{DEVICE}"))?;
        let schema = inspect_schema(&bytes)?;
        let device = &schema.devices()[0];
        assert_eq!(device.name, "test");
        assert_eq!(device.nodes.len(), 2);
        assert_eq!(device.nodes[0].binding, NodeBinding::Terminal);
        assert_eq!(device.nodes[0].resistive_offset, Some(0));
        assert_eq!(device.nodes[1].resistive_offset, Some(8));
        assert_eq!(device.jacobian[0].column, 1);
        assert_eq!(device.noise_sources[0].nodes.second, Some(1));
        assert_eq!(
            device.noise_sources[0].kind,
            (minor == 4).then_some(NoiseKind::White)
        );
        assert!(!device.charge);
        let real = |name: &str, value| Assignment {
            name: name.to_owned(),
            value: Value::Scalar(Scalar::Real(value)),
        };
        let bound = schema.bind(0, Scope::Instance, vec![real("R", 2700.)])?;
        assert_eq!(bound.digest(), &schema.envelope().sha256);
        assert_eq!(bound.descriptor(), 0);
        assert_eq!(bound.scope(), Scope::Instance);
        assert_eq!(
            bound.values().collect::<Vec<_>>(),
            [(0, &Value::Scalar(Scalar::Real(2700.)))]
        );
        assert!(
            schema
                .bind(
                    0,
                    Scope::Instance,
                    vec![real("R", 1.), real("Resistance", 2.)]
                )
                .is_err()
        );
        assert!(
            schema
                .bind(0, Scope::Instance, vec![real("R", f64::NAN)])
                .is_err()
        );
        assert!(
            schema
                .bind(0, Scope::Instance, vec![real("mode", 1.)])
                .is_err()
        );
        assert!(
            schema
                .bind(0, Scope::Model, vec![real("current", 1.)])
                .is_err()
        );
        let labels = Assignment {
            name: "labels".into(),
            value: Value::Array(vec![Scalar::String("a".into()), Scalar::String("b".into())]),
        };
        assert!(schema.bind(0, Scope::Model, vec![real("r", 20.)]).is_err());
        let model = schema.bind(0, Scope::Model, vec![labels])?;
        assert_eq!(model.values().map(|(id, _)| id).collect::<Vec<_>>(), [2]);
    }
    Ok(())
}

#[test]
fn malformed_descriptor_contents_rejected() -> Result<()> {
    for define in ["#define ROW 7", "#define FLAGS 12", "#define FLAGS 32"] {
        let bytes = shared_object(&format!("{define}\n{DEVICE}"))?;
        assert!(inspect(&bytes).is_ok());
        assert!(model_prepare_osdi::schema::inspect_schema(&bytes).is_err());
    }
    Ok(())
}
