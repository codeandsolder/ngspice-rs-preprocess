use model_kernel_bipolar::{
    Temperature,
    vbic::{
        binding::{self, Assignment, Polarity, Value},
        temperature::{Coefficient, Nominal},
    },
};
fn scalar(name: &str, value: f64) -> Assignment<'_> {
    Assignment {
        name,
        value: Value::Scalar(value),
    }
}

#[test]
fn complete_catalog_and_native_setter_order() -> Result<(), Box<dyn std::error::Error>> {
    let defaults = Nominal::source_defaults();
    for (index, slot) in Coefficient::ALL.into_iter().enumerate() {
        assert_eq!(slot.index(), index);
        assert_eq!(Coefficient::from_name(slot.name()), Some(slot));
        assert_eq!(
            defaults.value(slot).to_bits(),
            slot.source_default().to_bits()
        );
    }
    let base = binding::model("npn", 9., &[])?;
    assert_eq!(base.nominal().value(Coefficient::Rcx), 0.);
    assert_eq!(base.nominal().value(Coefficient::Rci), 0.1);
    assert!(!base.self_heating());
    let model = binding::model(
        "npn",
        4.,
        &[
            scalar("rcx", 0.),
            scalar("rbx", -2.),
            scalar("tref", 30.),
            scalar("TNOM", 31.),
            scalar("v0", 2.),
            scalar("qc0", 1e-12),
            scalar("xrc", 2.),
            scalar("rth", 100.),
            Assignment {
                name: "pnp",
                value: Value::Flag(true),
            },
            scalar("bvbe", 4.),
        ],
    )?;
    assert_eq!(model.polarity(), Polarity::Pnp);
    assert_eq!(model.nominal().value(Coefficient::Rcx), 0.01);
    assert_eq!(model.nominal().value(Coefficient::Rbx), 0.01);
    assert_eq!(model.nominal().value(Coefficient::Tnom), 31.);
    assert_eq!(model.nominal().value(Coefficient::Vo), 2.);
    assert_eq!(model.nominal().value(Coefficient::Qco), 1e-12);
    assert_eq!(model.nominal().value(Coefficient::Xrcx), 2.);
    assert_eq!(model.nominal().value(Coefficient::Cth), 1e-12);
    assert!(model.self_heating());
    assert_eq!(model.limits().reverse_be, 4.);
    assert_ne!(base.digest(), model.digest());
    let cold = binding::model("npn", 9., &[scalar("rth", 100.), scalar("selft", 0.)])?;
    assert!(!cold.self_heating());
    assert!(binding::model("npn", 1., &[]).is_err());
    assert!(binding::model("npn", 9., &[scalar("unknown", 1.)]).is_err());
    assert!(binding::model("npn", 9., &[scalar("nf", 0.)]).is_err());
    Ok(())
}

#[test]
fn instance_temperature_scaling_and_ordered_initial_conditions()
-> Result<(), Box<dyn std::error::Error>> {
    let circuit = Temperature::new(300.)?;
    let instance = binding::instance(&[
        scalar("AREA", 2.),
        scalar("m", 3.),
        scalar("temp", 50.),
        scalar("dtemp", 5.),
        Assignment {
            name: "ic",
            value: Value::Initial(&[0.7, 1.2]),
        },
        scalar("icvbe", 0.8),
        Assignment {
            name: "off",
            value: Value::Flag(true),
        },
    ])?;
    assert_eq!(instance.scale(), 6.);
    assert_eq!(instance.multiplicity(), 3.);
    assert_eq!(instance.ambient(circuit)?.kelvin(), 328.15);
    assert_eq!(instance.initial().vbe, Some(0.8));
    assert_eq!(instance.initial().vce, Some(1.2));
    assert!(instance.initial().off);
    let model = binding::model("npn", 9., &[scalar("dtemp", 10.)])?;
    let prepared = model.nominal().prepare(instance.ambient(circuit)?)?;
    assert_eq!(prepared.temperature().kelvin(), 338.15);
    assert!(binding::instance(&[scalar("m", 0.)]).is_err());
    assert!(
        binding::instance(&[Assignment {
            name: "ic",
            value: Value::Initial(&[])
        }])
        .is_err()
    );
    Ok(())
}
