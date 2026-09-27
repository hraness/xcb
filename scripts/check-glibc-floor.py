#!/usr/bin/env python3
"""Fail when a Linux release binary needs a newer glibc than the floor.

    check-glibc-floor.py [--floor 2.34] ARCHIVE.tar.gz|ELF [...]

The floor is the oldest glibc the Linux archives support (docs/publishing.md).
The dynamic loader refuses to start a program that needs a GLIBC_x.y version
its libc does not define, so this reads every version the binary needs from
its version-needs table (.gnu.version_r, what `objdump -p` lists as Version
References) and fails when one is newer than the floor, naming the symbols
that need it. GLIBC_PRIVATE is never portable and always fails. A binary
without version needs (static) passes. An archive must hold exactly one
regular member named `xcb`, and an `xcb-<version>-linux-<arch>.tar.gz` name
must match the binary's machine.
"""
import argparse
import re
import struct
import sys
import tarfile

FLOOR = "2.34"
MACHINES = {"x86_64": 62, "aarch64": 183}
ARCHIVE = re.compile(r"xcb-[0-9]+\.[0-9]+\.[0-9]+-linux-([a-z0-9_]+)\.tar\.gz")
GLIBC = re.compile(r"GLIBC_([0-9]+(?:\.[0-9]+)+)")
SHT_DYNSYM, SHT_GNU_VERNEED, SHT_GNU_VERSYM = 11, 0x6FFFFFFE, 0x6FFFFFFF
VER_FLG_WEAK = 0x2
LIMIT = 512 * 1024 * 1024


class Refusal(Exception):
    pass


def version_tuple(text):
    return tuple(int(part) for part in text.split("."))


def version_needs(data):
    """Return (e_machine, [(library, version name, weak, [symbols])])."""
    try:
        return _version_needs(data)
    except (struct.error, IndexError, ValueError) as error:
        raise Refusal(f"malformed ELF: {error}") from None


def _version_needs(data):
    if data[:4] != b"\x7fELF":
        raise Refusal("not an ELF file")
    if data[4] != 2 or data[5] != 1:
        raise Refusal("not a 64-bit little-endian ELF file")
    (machine,) = struct.unpack_from("<H", data, 18)
    (shoff,) = struct.unpack_from("<Q", data, 0x28)
    shentsize, shnum = struct.unpack_from("<HH", data, 0x3A)
    if shoff == 0 or shnum == 0 or shentsize != 64:
        raise Refusal("the ELF file has no section headers to read")
    sections = [struct.unpack_from("<IIQQQQIIQQ", data, shoff + index * shentsize) for index in range(shnum)]

    def string(table, offset):
        start = sections[table][4] + offset
        return data[start:data.index(b"\0", start)].decode("ascii", "replace")

    needs = {}
    for _, kind, _, _, offset, _, link, count, _, _ in sections:
        if kind != SHT_GNU_VERNEED:
            continue
        entry = offset
        for _ in range(count):
            _, aux_count, file_name, aux, following = struct.unpack_from("<HHIII", data, entry)
            library, position = string(link, file_name), entry + aux
            for _ in range(aux_count):
                _, flags, index, name, next_aux = struct.unpack_from("<IHHII", data, position)
                needs[index] = (library, string(link, name), bool(flags & VER_FLG_WEAK))
                if next_aux == 0:
                    break
                position += next_aux
            if following == 0:
                break
            entry += following

    symbols = {}
    dynsym = [section for section in sections if section[1] == SHT_DYNSYM]
    versym = [section for section in sections if section[1] == SHT_GNU_VERSYM]
    if needs and dynsym and versym:
        _, _, _, _, offset, size, link, _, _, entsize = dynsym[0]
        for index in range(size // (entsize or 24)):
            (name,) = struct.unpack_from("<I", data, offset + index * (entsize or 24))
            (version,) = struct.unpack_from("<H", data, versym[0][4] + index * 2)
            if version & 0x7FFF in needs:
                symbols.setdefault(version & 0x7FFF, set()).add(string(link, name))
    return machine, [(*needs[index], sorted(symbols.get(index, ()))) for index in sorted(needs)]


def read_binary(path):
    """Return (the bytes to check, the machine its archive name requires or None)."""
    name = path.rsplit("/", 1)[-1]
    if not name.endswith(".tar.gz"):
        with open(path, "rb") as binary:
            return binary.read(LIMIT + 1), None
    match = ARCHIVE.fullmatch(name)
    if match is None:
        raise Refusal("not an xcb-<version>-linux-<arch>.tar.gz archive")
    if match.group(1) not in MACHINES:
        raise Refusal(f"no known machine for linux-{match.group(1)}")
    with tarfile.open(path, "r:gz") as archive:
        members = archive.getmembers()
        if [member.name for member in members] != ["xcb"] or not members[0].isfile():
            raise Refusal("the archive must hold exactly one regular member named xcb")
        return archive.extractfile(members[0]).read(LIMIT + 1), MACHINES[match.group(1)]


def check(path, floor):
    data, machine = read_binary(path)
    if len(data) > LIMIT:
        raise Refusal(f"larger than {LIMIT} bytes")
    actual, needs = version_needs(data)
    if machine is not None and actual != machine:
        raise Refusal(f"the binary's ELF machine is {actual}, not {machine} as its name says")
    glibc = [(GLIBC.fullmatch(version), library, version, weak, symbols)
             for library, version, weak, symbols in needs if version.startswith("GLIBC_")]
    newer = [(match, version, symbols) for match, _, version, _, symbols in glibc
             if match is None or version_tuple(match.group(1)) > version_tuple(floor)]
    # Newest first; GLIBC_PRIVATE ahead of every numbered version.
    newer = [(version, symbols) for match, version, symbols in
             sorted(newer, key=lambda item: version_tuple(item[0].group(1)) if item[0] else (float("inf"),), reverse=True)]
    if newer:
        detail = "; ".join(f"{version} ({', '.join(symbols) or 'no symbols listed'})" for version, symbols in newer)
        raise Refusal(f"needs glibc newer than {floor}: {detail}")
    newest = max((match.group(1) for match, *_ in glibc), key=version_tuple, default=None)
    libraries = sorted({library for library, *_ in needs})
    if newest is None:
        return "needs no glibc version (static or no versioned libc symbols)"
    return f"needs glibc {newest} or newer (floor {floor}); libraries: {', '.join(libraries)}"


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n", 1)[0])
    parser.add_argument("--floor", default=FLOOR, help=f"oldest supported glibc (default {FLOOR})")
    parser.add_argument("paths", nargs="+", metavar="ARCHIVE|ELF")
    args = parser.parse_args(argv)
    if not re.fullmatch(r"[0-9]+\.[0-9]+", args.floor):
        parser.error("--floor must look like 2.34")
    failed = False
    for path in args.paths:
        try:
            print(f"ok: {path} {check(path, args.floor)}")
        except (Refusal, OSError, tarfile.TarError) as error:
            print(f"error: {path}: {error}", file=sys.stderr)
            failed = True
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
