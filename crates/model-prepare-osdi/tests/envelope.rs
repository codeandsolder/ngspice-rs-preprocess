//! Independent ELF fixtures emitted by the platform C compiler.
#![cfg(target_os = "linux")]

use anyhow::{Result, ensure};
use model_prepare_osdi::{Abi, inspect};
use std::{fs, process::Command};

fn shared_object(source: &str) -> Result<Vec<u8>> {
    shared_object_with_flags(source, &[])
}

fn shared_object_with_flags(source: &str, flags: &[&str]) -> Result<Vec<u8>> {
    let directory = tempfile::tempdir()?;
    let input = directory.path().join("module.c");
    let output = directory.path().join("module.osdi");
    fs::write(&input, source)?;
    let result = Command::new("cc")
        .args(flags)
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

#[test]
fn compiler_prepares_frozen_expansion_with_explicit_limits() -> Result<()> {
    use model_prepare_osdi::compiler::{Compiler, Target};
    use model_preprocess_command::{Cancellation, ProcessLimits};
    use sha2::{Digest, Sha256};
    use std::{collections::BTreeMap, os::unix::fs::PermissionsExt, time::Duration};

    let directory = tempfile::tempdir()?;
    let root = directory.path();
    let executable = root.join("test-openvaf");
    let source = root.join("root.va");
    let include = root.join("include");
    let scratch = root.join("scratch");
    fs::create_dir(&include)?;
    fs::create_dir(&scratch)?;
    fs::write(&source, b"original root\n")?;
    fs::write(include.join("dep.vams"), b"first include\n")?;
    fs::write(root.join("fixture.osdi"), shared_object(DEVICE)?)?;
    fs::write(
        &executable,
        br#"#!/bin/sh
set -eu
case "$1" in
  --version) printf 'fixture OpenVAF 0.4\n';;
  --print-expansion)
    test "$2" = -I
    test "$4" = -D
    test "$5" = TEST=7
    cat "$6" "$3/dep.vams"
    printf 'expansion diagnostic\n' >&2;;
  --target)
    test "$2" = x86_64-unknown-linux
    test "$3" = --target_cpu
    test "$4" = generic
    test "$5" = -O
    test "$6" = 3
    test "$7" = -o
    test "$(dirname "$9")" = "$PWD"
    test "$(basename "$9")" = source.va
    test "$(wc -l < "$9")" -eq 2
    cp "$(dirname "$0")/fixture.osdi" "$8"
    printf 'compiled\n'
    printf 'compiler diagnostic\n' >&2;;
  *) exit 10;;
esac
"#,
    )?;
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))?;
    let mut compiler = Compiler {
        executable,
        scratch: scratch.clone(),
        include_directories: vec![include.clone()],
        defines: BTreeMap::from([("TEST".into(), Some("7".into()))]),
        target: Target::X86_64,
        cpu: "generic".into(),
        optimization: 3,
        process: ProcessLimits {
            timeout: Duration::from_secs(5),
            ..ProcessLimits::default()
        },
        source_bytes: 1024,
        artifact_bytes: 1024 * 1024,
    };
    let cancellation = Cancellation::default();
    let first = compiler.compile(&source, &cancellation)?;
    assert_eq!(first.expanded(), b"original root\nfirst include\n");
    assert_eq!(first.identity().compiler_version, "fixture OpenVAF 0.4");
    assert_eq!(
        first.diagnostics().expansion_stderr,
        b"expansion diagnostic\n"
    );
    assert_eq!(first.diagnostics().compiler_stdout, b"compiled\n");
    assert_eq!(
        first.diagnostics().compiler_stderr,
        b"compiler diagnostic\n"
    );
    let digest: [u8; 32] = Sha256::digest(first.expanded()).into();
    assert_eq!(first.identity().expanded_sha256, digest);
    let module_digest: [u8; 32] = Sha256::digest(first.bytes()).into();
    assert_eq!(first.schema().envelope().sha256, module_digest);
    assert_eq!(fs::read_dir(&scratch)?.count(), 0);
    fs::write(include.join("dep.vams"), b"second include\n")?;
    let second = compiler.compile(&source, &cancellation)?;
    assert_eq!(first.identity().root_sha256, second.identity().root_sha256);
    assert_ne!(
        first.identity().expanded_sha256,
        second.identity().expanded_sha256
    );
    assert_eq!(second.expanded(), b"original root\nsecond include\n");
    compiler.source_bytes = 20;
    assert!(compiler.compile(&source, &cancellation).is_err());
    compiler.source_bytes = 1024;
    compiler.artifact_bytes = 32;
    assert!(compiler.compile(&source, &cancellation).is_err());
    cancellation.cancel();
    assert!(compiler.compile(&source, &cancellation).is_err());
    assert_eq!(fs::read_dir(&scratch)?.count(), 0);
    Ok(())
}

#[test]
fn packed_relative_relocations_preserve_static_schema() -> Result<()> {
    use object::Object;
    let ordinary = shared_object(DEVICE)?;
    let packed = shared_object_with_flags(DEVICE, &["-Wl,-z,pack-relative-relocs"])?;
    let file = object::File::parse(packed.as_slice())?;
    assert!(file.section_by_name(".relr.dyn").is_some());
    let ordinary = model_prepare_osdi::schema::inspect_schema(&ordinary)?;
    let packed = model_prepare_osdi::schema::inspect_schema(&packed)?;
    assert_eq!(ordinary.devices(), packed.devices());
    assert_ne!(ordinary.envelope().sha256, packed.envelope().sha256);
    Ok(())
}
