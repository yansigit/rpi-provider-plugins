The roster uses ICU4X 1.5's native Rust collator with ICU 78.3 / CLDR 48 data,
matching Node 24.22.0 (Unicode 17). Stock ICU4X 1.5 data changes the order of some
credential IDs. Other workspace providers retain their existing Unicode data.

`icu78-collation.postcard` contains collation, canonical decomposition and locale
fallback data. It is compiled into the library and needs no Node process, download
or system ICU at runtime. SHA256:
`7cc7de342224f2abe4fbec382c3bae6c870a73a908bda512d796f10bb19684ff`.

`scripts/build-collation-data.py` reproduces it on Linux using Python 3.12+, gh,
Cargo, make and a C++ compiler. It verifies source and output checksums. Generation
tools are separate from the package's Rust 1.78 build. Unicode and CLDR licenses
are retained here.

ICU 78's normalization export format differs from ICU4X 1.5's format. The script
runs the pinned ICU 75 exporter against ICU 78's normalization engine/data. Its
one adjustment omits the NFC backward-composition flag for complex decompositions;
only NFD tables are consumed. This data must not be used as NFC composition data.
The acceptance test compares NFD for every Unicode scalar and combining-mark
reordering against Node. Collation uses the Unihan database, as Node does.

The datagen lock differs from the published 1.5.0 lock only by zerovec 0.10.4,
whose explicit C tuple layout makes serialized fallback data portable across
compiler versions. The package uses that same zerovec version.
