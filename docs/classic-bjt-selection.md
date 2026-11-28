# Classic BJT source selection

The classic Gummel–Poon family has two physically distinct thermal-voltage
conventions. The producer selects a model identity, not a temperature
rescaling or a global solver mode:

| Selected implementation | Canonical native model family | Thermal voltage |
| --- | --- | --- |
| SIMetrix classic `BJT` | `gummel_poon_legacy_npn`, `gummel_poon_legacy_pnp` | `T * (1.3806226e-23 / 1.6021918e-19)` |
| Native classic BJT | `NPN`, `PNP` | Native ngspice convention |

The legacy ratio is 8.617086918058125e-5 V/K. The original model and
instance parameters, nominal and ambient temperatures, and lexical
expressions are retained. The consumer must select the associated law
for each model; it must not rewrite the ambient or nominal temperature.
The producer's source-neutral `ClassicBjtFamily` type communicates polarity
and thermal convention independently.

SIMetrix deck selection uses the built-in NPN/PNP level-0 (omitted) and
level-1 mappings, or the documented `BJT` implementation selected through
`.MAP` or device configuration. As with other families, the file-language
header controls parsing and is independent of request-local model mapping.
A native model can coexist with a legacy one using the explicit producer
extension `.MAP ModelName=NPN Level=70 Device=NGSPICE_BJT Letter=Q` and a
corresponding `NPN LEVEL=70` card. Both implementation selections emit
canonical `LEVEL=1`, with distinct family spelling and manifest identity.
The level 70 here is an example of a request-local mapping key, not a
vendor model level. Native deck preparation never applies this SIMetrix
mapping. Explicit VBIC selection retains its own implementation.

`LPNP` substrate semantics and unqualified explicit `VERSION` values are
rejected, not silently mapped to classic PNP. The legacy thermal-voltage
qualification does not establish that VBIC, HICUM, lateral models, or all
other physical constants of a vendor implementation use this convention.

Model `T_ABS` is lowered using the existing occurrence-scoped temperature
policy; an explicit instance `TEMP` retains precedence. The original
`TNOM`, model parameters and multiplicity remain unscaled.

The independent fixed-bias temperature fixtures use measured SIMetrix
collector currents and the historical ideal transport equation. The tests
bind the emitted model and instance parameters, including scoped temperature
expressions, before evaluating this equation. They do not pass an explicit
legacy alias to stock ngspice or silently substitute its modern law.

Public vendor references: [Gummel–Poon BJT syntax](https://help.simetrix.co.uk/9.4/simetrix/simulator_reference/topics/analogdevicereference_bipolarjunctiontransistor_spicegummelpoon.htm)
and [GetDeviceInfo implementation identifiers](https://help.simetrix.co.uk/8.3/simetrix/script_manual/topics/func_getdeviceinfo.htm).
The numerical consumer is coordinated in ngspice-rs-fast #266.
