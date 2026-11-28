# Headless LTspice schematic import

Use the explicit preparation format:

```sh
ngspice-rs-preprocess ltspice-schematic circuit.asc prepared \
  --search ./custom-symbols --search /path/to/ltspice/lib
```

The root ASC is a schematic, not a SPICE netlist. The importer resolves
relative ASY files (including Windows separators and case-insensitive
library spelling), electrical pin positions and SpiceOrder, rotations and
reflections, wire endpoints and flags, and hierarchical BLOCK symbols.
Geometric crossings without a shared endpoint, pin, or flag do not become
junctions. Repeated child sheets share a definition, and formal port order
comes from the block symbol. Model paths and text directives go through the
ordinary linked-source LTspice preparation pipeline. Original ASC and
included-model line locations are retained in the output provenance.

The symbol's effective instance attributes select the electrical call.
For a fixed X symbol with Value2 and no ModelFile, Value2 is the complete
call tail and SpiceModel names the external file if present. Otherwise the
fields compose in vendor netlist order. X instance names acquire the
vendor's X prefix. The standard QN/QP, MN/MP and JN/JP prefixes select
their corresponding primitive family. Three-terminal Q and M symbols emit
grounded substrate and source-tied bulk respectively. Sparse A-device
terminal slots and physically unattached pins connect to the common
terminal; an unattached common terminal is ground.

Semiconductor primitives use the standard component-library declarations
emitted by LTspice. Provide the appropriate standard.dio, standard.bjt,
standard.jft or standard.mos resource in the search roots or the sibling
cmp directory of a supplied sym directory. The importer does not contain
or download vendor model data. Explicit model resources and generated
generic default declarations are processed by the same normalizer as an
exported deck.

The importer rejects ambiguous label connections, invalid or duplicate
pin orders, recursive hierarchy, unresolved resources, unknown electrical
records and unsupported prefixes rather than inventing electrical
behavior. Global $G_ labels require separate dialect-aware global node
binding; currently they are diagnosed, as are inaccessible absolute
Windows resource paths on non-Windows hosts. Successful topology import
does not by itself qualify every LTspice device or analysis for execution.

Qualification includes independently authored synthetic schematics exported
by LTspice, a licensed public RC schematic/netlist pair, and independent
OP, DC, complex AC and transient comparison of an imported hierarchical
RC circuit against a separately authored native reference deck. The
request's existing input, work, recursion and output limits apply to all
schematics, symbols and libraries.
