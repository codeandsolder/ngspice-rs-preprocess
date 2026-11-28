# Source-selected classic BJT thermal convention

The SIMetrix simulation mapping selects classic Gummel–Poon for NPN/PNP
with omitted or LEVEL=1, or an explicit `.MAP ... DEVICE=BJT` selection.
The [public device-info reference](https://help.simetrix.co.uk/8.4/simplis/script_manual/topics/func_getdeviceinfo.htm)
identifies BJT as the internal classic implementation name; lexical language
headers are not physical model selectors. The [BJT parameter reference](https://help.simetrix.co.uk/8.3/simetrix/simulator_reference/topics/analogdevicereference_bipolarjunctiontransistor_spicegummelpoon.htm)
documents the nominal-temperature aliases and distinct LPNP family.

Preparation emits `gummel_poon_legacy_npn` or `gummel_poon_legacy_pnp`
with canonical LEVEL=1. The source `ModelSelection` retains original type,
level, source scope, spelling and mapping origin; catalog alias validation
checks the original declaration type while emitting the selected kernel type.
Native-deck admission and explicit VBIC selection retain their own models.
The lateral PNP case remains explicitly unqualified rather than being inferred
from its polarity. No temperature or transport parameter is rescaled;
T_MEASURED is canonicalized to TNOM, and existing model T_ABS / instance
TEMP resolution applies to the selected family. The solver #266 handles the
historical junction thermal-voltage convention per model. The selector does
not imply qualification of distinct quasi-saturation, noise or unported
source parameter semantics.
