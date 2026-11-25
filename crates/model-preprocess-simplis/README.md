# SIMPLIS control-flow preprocessing

The preprocessor expands active `.IF`/`.ELSE` and `.WHILE` blocks before
electrical translation. Nested loops have independent counts, and generated
device names, node names and compile-time actuals are substituted at the
iteration in which they are emitted.

## Loop semantics

The [official netlist preprocessor reference](https://www.simplis.com/documentation/simplis/simplis_reference/topics/runningsimplis_netlistpreprocessor.htm)
documents `.WHILE {expression} [max_loop_count]`. Direct synthetic probes
using SIMetrix/SIMPLIS Elements 9.40d
(`PreProcessNetlist /simulator SIMPLIS input.net expanded.out`)
establish the following behavior:

| Source limit | Maximum successful iterations |
| --- | ---: |
| Omitted | 1000 |
| 3 | 3 |
| 2.9 | 2 |
| 0 or negative | 0 |
| Unresolved expression or nonnumeric value | 1000 |

The tested version uses 1000, rather than the 100 printed in the manual.
Numeric expressions, including braced expressions and parameters, are
evaluated once on entry to each active loop. An initially false condition
does not require the limit to evaluate. A loop that finishes exactly at
its limit succeeds; if the condition is still true, preprocessing fails.
The reference can emit partial text on overrun, but this API returns an
error instead of publishing partially expanded output. An independent
100000-iteration implementation ceiling applies to each loop, even when
the source requests a higher limit.

For example:

```spice
.VAR I=0
.WHILE {I<3} 3
R{I} 1 0 {I+1}
.VAR I={I+1}
.ENDWHILE
```

expands to:

```spice
R0 1 0 1
R1 1 0 2
R2 1 0 3
```

Changing the limit to 2 makes this expansion fail. Changing the condition
to `{I<2}` makes it succeed at the limit.

## Hierarchy

Both flat and linked preparation expand loop-generated subcircuit
calls and specialize explicitly passed `VARS:` actuals. A plain wrapper
does not prevent expansion of a controlled helper farther down the call
graph. Control-flow-only processing expands known compile-time
expressions while retaining runtime parameter declarations and unresolved
electrical expressions. Unresolved runtime expressions may still need
validation by the downstream parser.

`.FOR` and `.ENDFOR` are explicitly rejected when active; no supported
source dialect has been established for them. Cumulative expansion
budgets and full local/global parameter propagation are separate policies.
