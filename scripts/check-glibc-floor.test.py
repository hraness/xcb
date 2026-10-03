#!/usr/bin/env python3
"""Synthetic tests for the Linux glibc floor check: no toolchain or real binary needed."""
import contextlib
import importlib.util
import io
from pathlib import Path
import struct
import tarfile
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("floor", Path(__file__).with_name("check-glibc-floor.py"))
f = importlib.util.module_from_spec(spec)
spec.loader.exec_module(f)


def elf(needs, machine=62):
    """An ELF64 file whose version-needs table lists `needs`:
    [(library, [(version, [symbols], weak), ...]), ...]."""
    dynstr = bytearray(b"\0")

    def name(text):
        offset = len(dynstr)
        dynstr.extend(text.encode() + b"\0")
        return offset

    symbols, verneed, index = [(0, 0)], bytearray(), 2
    entries = []
    for library, versions in needs:
        auxes = []
        for version, names, weak in versions:
            auxes.append((index, name(version), 0x2 if weak else 0))
            symbols += [(name(symbol), index) for symbol in names]
            index += 1
        entries.append((name(library), auxes))
    for position, (library, auxes) in enumerate(entries):
        following = 0 if position == len(entries) - 1 else 16 + 16 * len(auxes)
        verneed += struct.pack("<HHIII", 1, len(auxes), library, 16, following)
        for number, (other, version, flags) in enumerate(auxes):
            verneed += struct.pack("<IHHII", 0, flags, other, version, 0 if number == len(auxes) - 1 else 16)
    dynsym = b"".join(struct.pack("<IBBHQQ", offset, 0x12 if offset else 0, 0, 0, 0, 0) for offset, _ in symbols)
    versym = b"".join(struct.pack("<H", version) for _, version in symbols)
    shstrtab = b"\0.shstrtab\0.dynstr\0.dynsym\0.gnu.version\0.gnu.version_r\0"
    # (name offset, type, data, link, info, entsize)
    sections = [(1, 3, shstrtab, 0, 0, 0), (11, 3, bytes(dynstr), 0, 0, 0), (19, 11, dynsym, 2, 1, 24),
                (27, f.SHT_GNU_VERSYM, versym, 3, 0, 2)]
    if needs:
        sections.append((40, f.SHT_GNU_VERNEED, bytes(verneed), 2, len(entries), 0))
    body, headers, offset = bytearray(), [bytes(64)], 64
    for section_name, kind, data, link, info, entsize in sections:
        body += bytes(-len(body) % 8)
        headers.append(struct.pack("<IIQQQQIIQQ", section_name, kind, 0, 0, offset + len(body), len(data), link, info, 8, entsize))
        body += data
    body += bytes(-len(body) % 8)
    shoff = 64 + len(body)
    header = b"\x7fELF" + bytes([2, 1, 1]) + bytes(9) + struct.pack("<HHIQQQIHHHHHH", 3, machine, 1, 0, 0, shoff, 0, 64, 56, 0, 64, len(headers), 1)
    return header + body + b"".join(headers)


LIBC = ("libc.so.6", [("GLIBC_2.2.5", ["malloc", "free"], False), ("GLIBC_2.34", ["__libc_start_main", "pthread_create"], False)])
GCC = ("libgcc_s.so.1", [("GCC_4.2.0", ["_Unwind_GetIPInfo"], False)])


class FloorTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="xcb-glibc-floor-")
        self.root = Path(self.temp.name)

    def tearDown(self):
        self.temp.cleanup()

    def write(self, name, data):
        path = self.root / name
        path.write_bytes(data)
        return str(path)

    def archive(self, name, members):
        path = self.root / name
        with tarfile.open(path, "w:gz") as archive:
            for member_name, data in members:
                member = tarfile.TarInfo(member_name)
                member.size = len(data)
                archive.addfile(member, io.BytesIO(data))
        return str(path)

    def run_main(self, *argv):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            status = f.main(list(argv))
        return status, out.getvalue(), err.getvalue()

    def test_a_binary_at_the_floor_passes_and_names_its_libraries(self):
        path = self.write("xcb", elf([GCC, LIBC]))
        status, out, _ = self.run_main(path)
        self.assertEqual(status, 0)
        self.assertIn("needs glibc 2.34 or newer (floor 2.34)", out)
        self.assertIn("libc.so.6, libgcc_s.so.1", out)

    def test_a_newer_glibc_need_fails_and_names_its_symbols(self):
        newer = ("libc.so.6", LIBC[1] + [("GLIBC_2.38", ["__isoc23_strtol"], False), ("GLIBC_2.39", ["pidfd_spawnp", "pidfd_getpid"], True)])
        status, _, err = self.run_main(self.write("xcb", elf([GCC, newer])))
        self.assertEqual(status, 1)
        self.assertIn("needs glibc newer than 2.34: GLIBC_2.39 (pidfd_getpid, pidfd_spawnp); GLIBC_2.38 (__isoc23_strtol)", err)
        self.assertEqual(self.run_main("--floor", "2.39", self.write("xcb2", elf([newer])))[0], 0)

    def test_libm_and_private_versions_count(self):
        libm = ("libm.so.6", [("GLIBC_2.35", ["fmaximum"], False)])
        self.assertIn("GLIBC_2.35 (fmaximum)", self.run_main(self.write("m", elf([LIBC, libm])))[2])
        private = ("ld-linux-x86-64.so.2", [("GLIBC_PRIVATE", ["_dl_catch_error"], False)])
        self.assertIn("GLIBC_PRIVATE (_dl_catch_error)", self.run_main(self.write("p", elf([LIBC, private])))[2])

    def test_version_order_is_numeric(self):
        old = ("libc.so.6", [("GLIBC_2.9", ["pipe2"], False), ("GLIBC_2.10", ["accept4"], False), ("GLIBC_2.3.4", ["x"], False)])
        status, out, _ = self.run_main("--floor", "2.10", self.write("xcb", elf([old])))
        self.assertEqual(status, 0)
        self.assertIn("needs glibc 2.10 or newer", out)

    def test_a_static_binary_needs_no_glibc(self):
        status, out, _ = self.run_main(self.write("xcb", elf([])))
        self.assertEqual(status, 0)
        self.assertIn("needs no glibc version", out)

    def test_archives_hold_one_regular_xcb_matching_their_machine(self):
        binary = elf([LIBC])
        self.assertEqual(self.run_main(self.archive("xcb-0.10.0-linux-x86_64.tar.gz", [("xcb", binary)]))[0], 0)
        cases = {
            "xcb-0.10.0-linux-aarch64.tar.gz": [("xcb", binary)],
            "xcb-0.10.0-linux-x86_64.tar.gz": [("xcb", binary), ("extra", b"")],
            "xcb-0.10.0-darwin-aarch64.tar.gz": [("xcb", binary)],
            "xcb-0.10.0-linux-riscv64.tar.gz": [("xcb", binary)],
        }
        for name, members in cases.items():
            status, _, err = self.run_main(self.archive(name, members))
            self.assertEqual(status, 1, name)
            self.assertIn("error:", err)
        self.assertEqual(self.run_main(self.archive("xcb-0.10.0-linux-aarch64.tar.gz", [("xcb", elf([LIBC], machine=183))]))[0], 0)

    def test_anything_but_a_readable_elf64_fails_closed(self):
        for data, message in [(b"\xcf\xfa\xed\xfe" + bytes(60), "not an ELF file"), (b"\x7fELF\x01\x01" + bytes(58), "64-bit"),
                              (elf([LIBC])[:80], "malformed ELF")]:
            status, _, err = self.run_main(self.write("xcb", data))
            self.assertEqual(status, 1)
            self.assertIn(message, err)
        self.assertEqual(self.run_main(str(self.root / "missing"))[0], 1)
        with self.assertRaises(SystemExit), contextlib.redirect_stderr(io.StringIO()):
            f.main(["--floor", "latest", "xcb"])


if __name__ == "__main__":
    unittest.main()
