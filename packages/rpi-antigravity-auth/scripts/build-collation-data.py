#!/usr/bin/env python3
"""Reproduce Node 24.22.0 collation data on Linux (Python 3.12+, cargo, gh, make, C++)."""
import base64
import hashlib
import os
import pathlib
import shutil
import subprocess
import tarfile
import tempfile
import urllib.request
import zipfile

PACKAGE = pathlib.Path(__file__).resolve().parent.parent
SOURCES = [
    ("unicode-org/icu", "release-78.3", "icu4c-78.3-sources.tgz", "3a2e7a47604ba702f345878308e6fefeca612ee895cf4a5f222e7955fabfe0c0"),
    ("unicode-org/icu", "release-78.3", "icu4x-icuexportdata-78.3.zip", "eb63a12439f3fd9199886808275900229a5638fb0ee88d3c3c528eca7b811e60"),
    ("unicode-org/cldr-json", "48.0.0", "cldr-48.0.0-json-full.zip", "25ab2651a1a874dd62a727d3c213d7c6bf0fc269b09d2937f0499b82d99fa58e"),
]
KEYS = ["collator/data@1", "collator/dia@1", "collator/jamo@1", "collator/reord@1", "collator/meta@1", "collator/prim@1", "normalizer/nfd@1", "normalizer/nfdex@1", "fallback/likelysubtags@1", "fallback/parents@1", "fallback/supplement/co@1"]
EXPECTED = "7cc7de342224f2abe4fbec382c3bae6c870a73a908bda512d796f10bb19684ff"


def check(data, expected):
    if hashlib.sha256(data).hexdigest() != expected:
        raise RuntimeError("Pinned source or generated-data checksum mismatch")
    return data


def run(*args, **kwargs):
    subprocess.run([str(arg) for arg in args], check=True, **kwargs)


with tempfile.TemporaryDirectory(prefix="rpi-collation-") as scratch:
    directory = pathlib.Path(scratch)
    for repo, tag, asset, sha256 in SOURCES:
        run("gh", "release", "download", tag, "--repo", repo, "--pattern", asset, "--dir", directory)
        check((directory / asset).read_bytes(), sha256)
    with tarfile.open(directory / SOURCES[0][2]) as archive:
        archive.extractall(directory / "source", filter="data")
    source = directory / "source/icu/source"
    exporter = base64.b64decode(subprocess.check_output([
        "gh", "api", "repos/unicode-org/icu/contents/icu4c/source/tools/icuexportdata/icuexportdata.cpp?ref=release-75-1", "--jq", ".content",
    ]))
    check(exporter, "5e4ef7a520f1f0fa2134ff6037c2dc07fdeb48d7c42a810d76ac18336e7bf625")
    old = b"} else if (uset_contains(backwardCombiningStarters, utf32[0])) {"
    assert exporter.count(old) == 1
    # Only NFD is consumed: complex decompositions need no NFC backward-composition flag.
    exporter = exporter.replace(old, b"} else if (len == 1 && uset_contains(backwardCombiningStarters, utf32[0])) {")
    (source / "tools/icuexportdata/icuexportdata.cpp").write_bytes(exporter)
    build = directory / "build"
    build.mkdir()
    run(source / "runConfigureICU", "Linux", "--disable-tests", "--disable-samples", "--disable-shared", "--enable-static", cwd=build)
    run("make", "-j8", cwd=build)
    normalization = directory / "normalization"
    normalization.mkdir()
    run(build / "bin/icuexportdata", "-m", "norm", "--trie-type", "small", "-d", normalization, env={**os.environ, "ICU_DATA": str(source / "data/in")})
    export = directory / "compatible-export.zip"
    with zipfile.ZipFile(directory / SOURCES[1][2]) as original, zipfile.ZipFile(export, "w") as merged:
        for entry in original.infolist():
            data = (normalization / pathlib.PurePosixPath(entry.filename).name).read_bytes() if entry.filename in ["norm/small/nfd.toml", "norm/small/nfdex.toml"] else original.read(entry.filename)
            merged.writestr(entry, data)
    crate = directory / "icu_datagen.crate"
    with urllib.request.urlopen("https://static.crates.io/crates/icu_datagen/icu_datagen-1.5.0.crate") as response:
        crate.write_bytes(check(response.read(), "d1e80c1b833419752592da4d8a37239f6f11dd04dc3c0048f90acc000b17f4a9"))
    with tarfile.open(crate) as archive:
        archive.extractall(directory, filter="data")
    generator = directory / "icu_datagen-1.5.0"
    # zerovec 0.10.4 fixes the tuple layout used by persisted locale-fallback data.
    shutil.copyfile(PACKAGE / "scripts/icu-datagen.lock", generator / "Cargo.lock")
    run("cargo", "build", "--release", "--locked", "--bin", "icu4x-datagen", cwd=generator)
    output = directory / "collation.postcard"
    run(generator / "target/release/icu4x-datagen", "--cldr-root", directory / SOURCES[2][2], "--icuexport-root", export, "--keys", *KEYS, "--locales", "full", "--collation-han-database", "unihan", "--format", "blob2", "--out", output)
    check(output.read_bytes(), EXPECTED)
    shutil.copyfile(output, PACKAGE / "data/icu78-collation.postcard")
    shutil.copyfile(directory / "source/icu/LICENSE", PACKAGE / "data/UNICODE-LICENSE")
    with zipfile.ZipFile(directory / SOURCES[2][2]) as archive:
        (PACKAGE / "data/CLDR-LICENSE").write_bytes(archive.read("LICENSE"))
print("Pinned ICU 78.3 / CLDR 48 collation data reproduced")
