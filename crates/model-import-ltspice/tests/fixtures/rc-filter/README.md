# Public schematic/netlist witness

`filter.asc` and `reference.net` are the unmodified corresponding
`RC_filter.asc` and `RC_filter.net` from the MIT-licensed
[ltspice-skill repository](https://github.com/ZhangHanyu/ltspice-skill/tree/8720c60a024e6cde3370e2026ecca1bd0eb3202d/examples/TRAN_analysis)
at commit `8720c60a024e6cde3370e2026ecca1bd0eb3202d`.
The supplied netlist identifies itself as an LTspice netlist. The source
and reference are used to verify connectivity, attributes, and directives
before numerical normalization. No claim is made that this test reruns the
vendor executable or that the downstream startup analysis is supported.

The three minimal .asy files are independently authored electrical pin
descriptions; they do not contain vendor graphics or library content.
