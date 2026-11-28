# Prepared MOS junction laws and source selection

The physical junction implementation lives in `model-compact-intrinsic::mos_junction` (exported through the crate root). It prepares single-device drain/source current, depletion/diffusion charge, their voltage derivatives and extrinsic series resistance. Neither the source parser nor the physical law owns solver topology, limiting, numerical integration, GMIN, polarity normalization, accepted/trial history or parallel instance multiplicity.

## Exact geometry and constitutive boundaries

HSPICE levels 49/53 select two distinct junction families. The table follows the HSPICE MOSFET Models Manual, X-2005.09, pp. 39–58 and 402–404; the mode number is not a portable native model selector.

| Source ACM | Current/charge law | Area/perimeter rule | Series resistance law |
| --- | --- | --- | --- |
| 0 | Linear reverse + exponential forward, diffusion storage | Explicit, lithographically scaled | Sheet squares, or direct terminal resistance; plus contact |
| 1 | Linear reverse + exponential forward, diffusion storage | Width normalized, bottom density per metre | Light diffusion + supplied/default squares; plus contact |
| 2 | Linear reverse + exponential forward, diffusion storage | Isolated contacts, generated only for omitted values | Light diffusion + supplied/automatic squares; plus contact |
| 3 | Linear reverse + exponential forward, diffusion storage | Folded/shared contacts, additional gate edge | Light diffusion + supplied/automatic squares; plus contact |
| 10 | Berkeley BSIM3 | Explicit physical geometry | As ACM 0 |
| 11 | Berkeley BSIM3 | Explicit physical geometry | As ACM 1 |
| 12 | Berkeley BSIM3 | Explicit unless CALCACM=1; then isolated | As ACM 2 |
| 13 | Berkeley BSIM3 | Explicit physical geometry | As ACM 3 |

ACM 1/11's ASPEC/UPDATE case with zero LDIF uses the direct-resistance law. `Option<f64>` preserves omission versus explicit zero independently for each terminal's area, perimeter and number of squares. `HspiceBsim3Junctions::bind_occurrence_geometry` resolves the element-scale transform, source instance values, explicit-mode global defaults, GEO sharing and DTEMP while preserving omission. Global option dimensions in its typed input are already SI. Automatic diffusion geometry ignores global area/perimeter defaults, and automatic square calculation ignores default squares. The caller supplies the electrical effective channel width after channel preparation.

For linear-reverse modes, `IS` is the direct nominal saturation current when the geometry-weighted current is zero. It is temperature-corrected with the selected total current. `CBD` and `CBS` are independent direct capacitances when the geometry-weighted depletion capacitance vanishes. The sidewall and gate-edge capacitances use the same PHP/MJSW shape. The reverse transitions at zero and VNDS have continuous current and charge, with the more-positive branch supplying the derivative exactly at each transition. The TT diffusion charge is the transit time times the current. No artificial conductance enters that physical charge.

The thermal state supports the documented TLEV bandgap selection and all four TLEVC laws (full semiconductor potential correction, linear coefficients, power-law rescaling, and semiconductor tangent). Resistance temperature correction is shared by both constitutive families and applies to the complete terminal series resistance. The source binder selects the physical law once from ACM; consumers use the prepared physical type, not a second source-mode selector. Model-card SCALM converts explicit coefficients to SI, while documented SI defaults and inherited coefficients retain their physical units. Source parameters N, PHP and CJGATE are not aliases for NJ, PBSW and CJSWG: those belong to different physical families.

Berkeley BSIM3 uses a different, exponential reverse current and optional IJTH continuation, with no TT diffusion charge. Native ngspice 47 reference revision `fa6c2a236507fd440c671de237cad4f49a4e6b68` has a further distinction: `bsim3v32/b3v32temp.c` calculates the forward knee from transformed ACM geometry, whereas `bsim3/b3temp.c` calculates it from raw explicit AD/AS/PD/PS. The latter is not necessarily a tangent of the load-time current. This behavior is represented by an explicit knee-reference choice; the source-selected Berkeley law uses its resolved physical geometry. Native revision-specific gate-edge capacitance correction likewise remains an explicit input rather than an implicit family alias.

## Qualification boundary

The physical laws and model-card binder are independently usable components, **not complete BSIM3/BSIM4 device support**. They do not replace the existing solver-owned channel, intrinsic charge/noise, NQS, or topology. HSPICE source lowering still rejects variants for which a qualified complete device is missing. In particular BSIM4 ACM 10–13 require BSIM4's asymmetric junction/breakdown law, rather than the BSIM3 law. Its native ACM12 route is qualified separately. BSIM4 occurrence modifiers are separately bound by `ModelSelection::bind_hspice_bsim4_occurrence_modifiers`. Model and instance expressions are evaluated in their respective scopes. Physical preparation applies the signed threshold offset, mobility multiplier, body-effect and subthreshold shifts to the resolved primary channel coefficients. The full finger-group series resistance includes the per-finger contact contribution divided by NF, before its terminal temperature correction. Negative contact/mobility and excessive negative body-effect shifts produce explicit reset flags. Dependent channel coefficients and electrical topology must be rebuilt from these occurrence-local outputs; the shared size-bin cache remains immutable. The native level-54 ACM12 route already supports the threshold and mobility modifiers: it canonicalizes DELVT0/DELVTO and expresses the negative MULU0 reset without freezing occurrence expressions. The remaining contact and post-bin body/subthreshold operations are not yet wired into native device admission.

Focused checks:

```sh
cargo +nightly test --locked -p model-compact-intrinsic --test mos_junction --test mos_linear_reverse --test bsim4_occurrence
cargo +nightly test --locked -p model-preprocess-simplis --test model_mapping
# Native level-54 end-to-end comparisons with ngspice on PATH:
cargo +nightly test --locked -p ngspice-rs-preprocess --test hspice
# With the independently built ngspice 47 reference on PATH:
cargo +nightly test --locked -p model-compact-intrinsic --test mos_junction -- --include-ignored
```

The native witness compares 3.2.4 and 3.3.0 ACM12/CALCACM1, both terminals, mixed explicit/omitted geometry, three temperatures, four biases and two IJTH values, including the native forward knee. The linear-reverse checks compare independent documented I–V and charge equations, all thermal laws, geometry and resistance laws and finite-difference Jacobians. These are equation-level witnesses, not measurements from an HSPICE executable. Source binding tests cover parameter precedence, mode selection, unit scaling, source defaults, independent drain/source coefficients and invalid modes.

Reference: [HSPICE MOSFET Models Manual X-2005.09](https://people.engr.tamu.edu/spalermo/ecen474/hspice_mosfet.pdf), especially pp. 39–58, 98–106, 402–404, 406, 423–425 and 442–444. No simulator implementation is copied from that manual.
