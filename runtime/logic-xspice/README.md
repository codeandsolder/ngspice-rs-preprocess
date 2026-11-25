# Event-based logic runtime for ngspice 47

The SIMPLIS translator emits three XSPICE models for numbered AND/OR/NAND/NOR
gates, DIGITAL1 AND/OR and S-R latches, and classic SRFF. This runtime is
independent of the experimental ALB runtime. Translation results advertise
the required code-model library through `required_code_models`.

Build against the configured **matching** ngspice source tree, including
its CIDER/KLU/XSPICE configuration and built cmpp. Code-model ABI mismatches
can crash the simulator. Use the managed compiler wrapper:

    bash runtime/logic-xspice/build.sh /path/to/ngspice-build /path/to/output

Load the resulting module before parsing any translated deck (for example,
put the following command in the simulation working directory's .spiceinit):

    codemodel /path/to/output/ngspice_preprocess_logic.cm

The reference regression example accepts the module path explicitly:

    cargo +nightly run --locked -p model-translate-simplis --example logic_ngspice -- /path/to/output/ngspice_preprocess_logic.cm

The receiver detects threshold crossings from accepted analog samples and
requests a simulator breakpoint. Event scheduling and state storage use
the simulator's rollback-aware interfaces. Classic gates and SRFF propagate
transport events; DIGITAL1 devices cancel pending transitions if the input
condition ceases to hold. Inertial latches evaluate against committed Q.
The output bridge is ideal, with the specified output resistance represented
by a separate resistor. Complemented outputs use the same scheduled state.
No artificial minimum propagation delay is imposed. Analog crossing
precision remains subject to the integrator's timestep and breakpoint
resolution.

The event-model operation identifiers are 0 AND, 1 OR, 2 NAND, 3 NOR,
4 reset-dominant latch, 5 set-dominant latch, and 6 hold-on-equal latch.
