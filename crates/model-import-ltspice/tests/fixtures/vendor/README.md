# Vendor exporter witnesses

Authored synthetic schematics exported by LTspice using -netlist on 2026-11-28.
The exporter-generated absolute title paths and CRLF line endings have been removed; naming.net is transcoded from CP1252 to UTF-8. All circuit records are retained.
res.asy is a minimal authored equivalent of the standard two-pin resistor symbol.

The standard/ and digital/ subdirectories contain additional authored
schematics and corresponding fresh LTspice exports. Only the title path,
CRLF line endings and absolute standard-library path prefix were removed.
The tests construct minimal symbols with authored pin definitions and empty
standard-library files; no vendor library model data is included. The
floating-terminal witness compares connectivity rather than vendor-specific
anonymous node numbering.
