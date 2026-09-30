#!/usr/bin/env python3
"""Behavioral tests use mocked Apple tools; no credentials or signing service."""
import base64
import gzip
import importlib.util
import io
import json
import os
from pathlib import Path
import stat
import struct
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import zipfile

SPEC = importlib.util.spec_from_file_location("signing", Path(__file__).with_name("sign-macos-release.py"))
signing = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(signing)
TEAM = "A1B2C3D4E5"
VERSION = "0.15.2"
UUID = "12345678-1234-1234-1234-123456789abc"


class SigningTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="xcb-signing-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.work = self.root / "xcb-apple-signing"
        self.output = self.root / "signed-artifacts"
        self.candidate = self.root / "candidate"
        self.candidate_zip = self.root / "candidate.zip"
        self.archive = self.root / signing.archive_name(VERSION, unsigned=True)
        self.binary = struct.pack("<IIIIIIII", 0xFEEDFACF, 0x0100000C, 0, 2, 0, 0, 0, 0) + b"not executable"
        self.native_archive()
        self.calls = []
        self.original_search_list = [str(self.root / "existing user.keychain-db")]
        self.search_list = self.original_search_list.copy()
        self.status = "Accepted"
        self.wait_id = UUID
        self.metadata = ("Identifier=dev.hraness.xcb\nTeamIdentifier=" + TEAM + "\n"
                         "CodeDirectory v=20500 size=100 flags=0x10000(runtime) hashes=2+7 location=embedded\n"
                         "Timestamp=Sep 30, 2026 at 2:00:00 AM\n")
        self.identity_team = TEAM
        self.tool_failure = None
        self.environment = {
            "RUNNER_TEMP": str(self.root), "HOME": str(self.root),
            "GITHUB_REPOSITORY": "hraness/xcb", "GITHUB_RUN_ID": "321", "GITHUB_RUN_ATTEMPT": "1",
            "VERIFIED_SHA": "a" * 40, "UNSIGNED_ARTIFACT_ID": "123", "UNSIGNED_ARTIFACT_DIGEST": "b" * 64,
            "CANDIDATE_PRODUCER_ATTEMPT": "1",
            "APPLE_DEVELOPER_ID_P12_BASE64": base64.b64encode(b"fake private p12").decode(),
            "APPLE_DEVELOPER_ID_P12_PASSWORD": "never-print-me",
            "APPLE_NOTARY_KEY_P8_BASE64": base64.b64encode(b"fake private p8").decode(),
            "APPLE_NOTARY_KEY_ID": "ABCDE12345", "APPLE_NOTARY_ISSUER_ID": UUID,
        }
        for active in (patch.dict(os.environ, self.environment), patch.object(signing, "TEAM_ID", TEAM),
                       patch.object(signing.sys, "platform", "darwin"), patch.object(signing, "run", self.tool)):
            active.start()
            self.addCleanup(active.stop)

    def native_archive(self, extra=False, symlink=False):
        with tarfile.open(self.archive, "w:gz", format=tarfile.USTAR_FORMAT) as archive:
            member = tarfile.TarInfo("xcb")
            member.size = len(self.binary)
            member.mode = 0o755
            if symlink:
                member.type = tarfile.SYMTYPE
                member.linkname = "/bin/sh"
                member.size = 0
            archive.addfile(member, None if symlink else io.BytesIO(self.binary))
            if extra:
                archive.addfile(tarfile.TarInfo("extra"))
        Path(str(self.archive) + ".sha256").write_text(signing.digest(self.archive.read_bytes()) + "\n")

    def tool(self, args, timeout=60):
        args = [str(arg) for arg in args]
        self.calls.append(args)
        self.assertFalse(any(name in os.environ for name in signing.SECRET_NAMES))
        if self.tool_failure and self.tool_failure in args:
            raise signing.SigningError("mock Apple rejection")
        if "create-keychain" in args:
            Path(args[-1]).touch(mode=0o600)
            for path in (self.work / "credentials").iterdir():
                self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
        if "find-identity" in args:
            self.assertIn(args[-1], self.search_list)
            return f'  1) {"A" * 40} "Developer ID Application: Example ({self.identity_team})"\n'
        if "list-keychains" in args:
            if "-s" in args:
                self.search_list = args[args.index("-s") + 1:]
            return "\n".join(json.dumps(path) for path in self.search_list)
        if "delete-keychain" in args:
            self.search_list = [path for path in self.search_list if path != args[-1]]
        for option in ("--requirements", "--test-requirement"):
            if option in args:
                self.assertTrue(args[args.index(option) + 1].startswith("="))
        if "--display" in args:
            return self.metadata
        if "notarytool" in args:
            if "submit" in args:
                self.assertEqual(timeout, 180)
                self.assertNotIn("--wait", args)
                return json.dumps({"id": UUID})
            self.assertEqual(timeout, 960)
            self.assertIn("15m", args)
            self.assertIn("wait", args)
            self.assertEqual(args[3], UUID)
            return json.dumps({"status": self.status, "id": self.wait_id})
        return ""

    def submit(self):
        signing.submit(self.archive, VERSION, self.candidate, self.work)
        self.assertFalse(self.work.exists())
        with zipfile.ZipFile(self.candidate_zip, "w") as archive:
            for path in self.candidate.iterdir():
                archive.write(path, path.name)
        self.candidate_digest = signing.digest(self.candidate_zip.read_bytes())

    def finalize(self):
        # A different runner receives only the notary credentials and original
        # immutable upload. Repeated attempts use the same ZIP and UUID.
        for name in signing.SECRET_NAMES[2:]:
            os.environ[name] = self.environment[name]
        (self.root / "xcb-apple-notarization.json").unlink(missing_ok=True)
        signing.finalize(self.candidate_zip, self.candidate_digest, VERSION, self.output, self.work)

    def sign(self):
        self.submit()
        self.finalize()

    def test_final_archive_is_signed_then_notarized_and_keychain_is_removed(self):
        self.sign()
        final = self.output / signing.archive_name(VERSION)
        self.assertTrue(final.is_file())
        self.assertEqual(Path(str(final) + ".sha256").read_text().strip(), signing.digest(final.read_bytes()))
        with tarfile.open(final) as archive:
            self.assertEqual([entry.name for entry in archive], ["xcb"])
            self.assertEqual(archive.extractfile("xcb").read(), self.binary)
        codesign = next(args for args in self.calls if "--sign" in args)
        self.assertIn("runtime", codesign)
        self.assertIn("--timestamp", codesign)
        self.assertIn("dev.hraness.xcb", codesign)
        self.assertIn("certificate leaf[field.1.2.840.113635.100.6.1.13] exists", codesign[-2])
        notarized = next(i for i, args in enumerate(self.calls) if "--check-notarization" in args)
        removed = next(i for i, args in enumerate(self.calls) if "delete-keychain" in args)
        self.assertLess(removed, notarized)
        self.assertFalse(self.work.exists())
        self.assertEqual(self.search_list, self.original_search_list)
        self.assertTrue(all(args[0] in ("/usr/bin/security", "/usr/bin/codesign", "/usr/bin/xcrun") for args in self.calls))
        receipt = json.loads((self.root / "xcb-apple-notarization.json").read_text())
        self.assertEqual(receipt["submissionId"], UUID)
        self.assertEqual(receipt["status"], "Accepted")
        self.assertEqual(receipt["state"], "verified")
        self.assertEqual(receipt["signedBinarySha256"], signing.digest(self.binary))

    def test_notary_rejection_removes_credentials_and_never_creates_release(self):
        self.status = "Invalid"
        with self.assertRaisesRegex(signing.SigningError, "not Accepted"):
            self.sign()
        self.assertFalse(self.output.exists())
        self.assertFalse(self.work.exists())
        self.assertEqual(self.search_list, self.original_search_list)
        self.assertTrue(any("delete-keychain" in args for args in self.calls))
        receipt = json.loads((self.root / "xcb-apple-notarization.json").read_text())
        self.assertEqual(receipt["submissionId"], UUID)
        self.assertEqual(receipt["status"], "Invalid")

    def test_cleanup_preserves_keychains_added_during_signing(self):
        added = str(self.root / "another user.keychain-db")

        def add_during_signing(args, timeout=60):
            if "--sign" in args:
                self.search_list.append(added)
            return self.tool(args, timeout)

        with patch.object(signing, "run", add_during_signing):
            self.sign()
        self.assertEqual(self.search_list, [*self.original_search_list, added])
        self.assertTrue(self.output.exists())

    def test_incomplete_notary_status_is_not_success(self):
        self.status = "In Progress"
        with self.assertRaisesRegex(signing.SigningError, "not Accepted"):
            self.sign()
        self.assertFalse(self.output.exists())

    def test_wait_timeout_preserves_submission_and_exact_hashes_without_retry(self):
        def timed_out(args, timeout=60):
            if "wait" in args:
                self.calls.append([str(arg) for arg in args])
                raise RuntimeError("Apple tool failed or timed out: xcrun")
            return self.tool(args, timeout)
        with patch.object(signing, "run", timed_out):
            with self.assertRaisesRegex(RuntimeError, "timed out"):
                self.sign()
        self.assertFalse(self.output.exists())
        self.assertFalse(self.work.exists())
        receipt_text = (self.root / "xcb-apple-notarization.json").read_text()
        receipt = json.loads(receipt_text)
        self.assertEqual(receipt["submissionId"], UUID)
        self.assertEqual(receipt["state"], "wait-incomplete")
        self.assertIsNone(receipt["status"])
        self.assertEqual(receipt["signedBinarySha256"], signing.digest(self.binary))
        self.assertEqual(receipt["unsignedArchiveSha256"], signing.digest(self.archive.read_bytes()))
        self.assertRegex(receipt["submissionZipSha256"], "^[0-9a-f]{64}$")
        self.assertEqual(sum("submit" in args for args in self.calls), 1)
        self.assertEqual(sum("wait" in args for args in self.calls), 1)
        for private in ("fake private p12", "fake private p8", "never-print-me", str(self.work)):
            self.assertNotIn(private, receipt_text)

    def test_wait_cannot_accept_a_different_submission(self):
        self.wait_id = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"
        with self.assertRaisesRegex(signing.SigningError, "another submission"):
            self.sign()
        self.assertFalse(self.output.exists())
        receipt = json.loads((self.root / "xcb-apple-notarization.json").read_text())
        self.assertEqual(receipt["submissionId"], UUID)
        self.assertEqual(receipt["state"], "wait-incomplete")
        self.assertIsNone(receipt["status"])

    def test_submit_failure_retains_hashes_and_never_retries_an_unknown_submission(self):
        self.tool_failure = "submit"
        with self.assertRaisesRegex(signing.SigningError, "mock Apple rejection"):
            self.sign()
        self.assertFalse(self.output.exists())
        self.assertFalse(self.work.exists())
        receipt = json.loads((self.root / "xcb-apple-notarization.json").read_text())
        self.assertIsNone(receipt["submissionId"])
        self.assertEqual(receipt["state"], "submission-started")
        self.assertEqual(receipt["signedBinarySha256"], signing.digest(self.binary))
        self.assertEqual(sum("submit" in args for args in self.calls), 1)
        self.assertFalse(any("wait" in args for args in self.calls))

    def test_arbitrary_service_status_is_not_retained_in_diagnostics(self):
        self.status = "unexpected secret echoed by service"
        with self.assertRaisesRegex(signing.SigningError, "not Accepted"):
            self.sign()
        receipt_text = (self.root / "xcb-apple-notarization.json").read_text()
        self.assertNotIn(self.status, receipt_text)
        self.assertEqual(json.loads(receipt_text)["status"], "Unrecognized")

    def test_wrong_certificate_team_is_rejected_before_signing(self):
        self.identity_team = "Z9Y8X7W6V5"
        with self.assertRaisesRegex(signing.SigningError, "expected Developer ID"):
            self.sign()
        self.assertFalse(any("--sign" in args for args in self.calls))
        self.assertFalse(self.work.exists())

    def test_hardened_runtime_and_timestamp_are_required(self):
        self.metadata = self.metadata.replace("(runtime)", "(none)")
        with self.assertRaisesRegex(signing.SigningError, "hardened runtime"):
            self.sign()
        self.assertFalse(any("notarytool" in args for args in self.calls))

    def test_missing_secure_timestamp_is_rejected(self):
        self.metadata = "\n".join(line for line in self.metadata.splitlines() if not line.startswith("Timestamp="))
        with self.assertRaisesRegex(signing.SigningError, "secure timestamp"):
            self.sign()
        self.assertFalse(any("notarytool" in args for args in self.calls))

    def test_actual_signed_metadata_must_match_expected_team(self):
        self.metadata = self.metadata.replace("TeamIdentifier=" + TEAM, "TeamIdentifier=Z9Y8X7W6V5")
        with self.assertRaisesRegex(signing.SigningError, "identity mismatch"):
            self.sign()
        self.assertFalse(self.output.exists())

    def test_term_interruption_unwinds_and_removes_credentials(self):
        def interrupted(args, timeout=60):
            if "notarytool" in args:
                # The installed SIGTERM handler raises SystemExit(143).
                raise SystemExit(143)
            return self.tool(args, timeout)
        with patch.object(signing, "run", interrupted):
            with self.assertRaises(SystemExit) as stopped:
                self.sign()
        self.assertEqual(stopped.exception.code, 143)
        self.assertFalse(self.output.exists())
        self.assertFalse(self.work.exists())
        self.assertTrue(any("delete-keychain" in args for args in self.calls))

    def test_post_notarization_verification_failure_blocks_publication(self):
        self.tool_failure = "--check-notarization"
        with self.assertRaisesRegex(signing.SigningError, "mock Apple rejection"):
            self.sign()
        self.assertFalse(self.output.exists())
        self.assertFalse(self.work.exists())

    def test_keychain_cleanup_failure_blocks_publication_and_removes_private_files(self):
        self.tool_failure = "delete-keychain"
        with self.assertRaisesRegex(signing.SigningError, "mock Apple rejection"):
            self.sign()
        self.assertFalse(self.output.exists())
        self.assertFalse(self.work.exists())

    def test_extra_tar_member_rejected_before_apple_tools(self):
        self.native_archive(extra=True)
        with self.assertRaisesRegex(signing.SigningError, "extra members"):
            self.sign()
        self.assertFalse(self.calls)
        self.assertFalse(self.work.exists())

    def test_gzip_expansion_is_bounded_before_tar_headers_are_parsed(self):
        self.archive.write_bytes(gzip.compress(b"x" * 2048))
        Path(str(self.archive) + ".sha256").write_text(signing.digest(self.archive.read_bytes()) + "\n")
        with patch.object(signing, "MAX_TAR_BYTES", 1024), patch.object(signing.tarfile, "open") as parser:
            with self.assertRaisesRegex(signing.SigningError, "expanded archive byte limit"):
                self.sign()
            parser.assert_not_called()
        self.assertFalse(self.calls)
        self.assertFalse(self.work.exists())
        self.assertFalse(self.output.exists())

    def test_oversized_pax_body_cannot_make_unbounded_gzip_reads(self):
        extension = tarfile.TarInfo("extended")
        extension.type = tarfile.XHDTYPE
        extension.size = signing.MAX_TAR_BYTES * 1024
        self.archive.write_bytes(gzip.compress(extension.tobuf()))
        Path(str(self.archive) + ".sha256").write_text(signing.digest(self.archive.read_bytes()) + "\n")
        original_read = gzip.GzipFile.read
        def guarded_read(stream, size=-1):
            self.assertGreaterEqual(size, 0)
            self.assertLessEqual(size, 65_536)
            return original_read(stream, size)
        with patch.object(gzip.GzipFile, "read", guarded_read):
            with self.assertRaises((tarfile.ReadError, signing.SigningError)):
                self.sign()
        self.assertFalse(self.calls)
        self.assertFalse(self.work.exists())
        self.assertFalse(self.output.exists())

    def test_symlink_payload_rejected_before_apple_tools(self):
        self.native_archive(symlink=True)
        with self.assertRaisesRegex(signing.SigningError, "regular xcb"):
            self.sign()
        self.assertFalse(self.calls)

    def test_payload_checksum_rejected_before_apple_tools(self):
        Path(str(self.archive) + ".sha256").write_text("0" * 64)
        with self.assertRaisesRegex(signing.SigningError, "checksum mismatch"):
            self.sign()
        self.assertFalse(self.calls)

    def test_payload_must_be_arm64_macho_before_apple_tools(self):
        self.binary = b"#!/bin/sh\necho payload must never run\n"
        self.native_archive()
        with self.assertRaisesRegex(signing.SigningError, "arm64 Mach-O"):
            self.sign()
        self.assertFalse(self.calls)

    def test_macho_must_be_an_executable_not_a_dylib(self):
        self.binary = self.binary[:12] + struct.pack("<I", 6) + self.binary[16:]
        self.native_archive()
        with self.assertRaisesRegex(signing.SigningError, "arm64 Mach-O executable"):
            self.sign()
        self.assertFalse(self.calls)

    def test_team_placeholder_fails_closed(self):
        with patch.object(signing, "TEAM_ID", "__XCB_APPLE_TEAM_ID__"):
            with self.assertRaisesRegex(signing.SigningError, "not configured"):
                self.sign()
        self.assertFalse(self.calls)

    def artifact_zip(self, extra=None):
        path = self.root / "artifact.zip"
        with zipfile.ZipFile(path, "w") as archive:
            archive.write(self.archive, self.archive.name)
            archive.write(Path(str(self.archive) + ".sha256"), self.archive.name + ".sha256")
            if extra:
                archive.writestr(extra, b"unexpected")
        return path

    def test_exact_artifact_zip_digest_and_two_file_inventory_are_verified(self):
        archive = self.artifact_zip()
        destination = self.root / "extracted"
        signing.unpack_artifact(archive, signing.digest(archive.read_bytes()), VERSION, destination)
        self.assertEqual((destination / self.archive.name).read_bytes(), self.archive.read_bytes())

    def test_wrong_artifact_zip_digest_is_a_hard_failure(self):
        archive = self.artifact_zip()
        destination = self.root / "extracted"
        with self.assertRaisesRegex(signing.SigningError, "ZIP digest mismatch"):
            signing.unpack_artifact(archive, "0" * 64, VERSION, destination)
        self.assertFalse(destination.exists())

    def test_extra_or_traversal_zip_member_is_rejected(self):
        archive = self.artifact_zip("../escaped")
        with self.assertRaisesRegex(signing.SigningError, "exactly the unsigned archive"):
            signing.unpack_artifact(archive, signing.digest(archive.read_bytes()), VERSION, self.root / "extracted")
        self.assertFalse((self.root / "extracted").exists())

    def rewrite_candidate(self, mutate):
        with zipfile.ZipFile(self.candidate_zip) as archive:
            files = {name: archive.read(name) for name in archive.namelist()}
        mutate(files)
        with zipfile.ZipFile(self.candidate_zip, "w") as archive:
            for name, contents in files.items():
                archive.writestr(name, contents)
        self.candidate_digest = signing.digest(self.candidate_zip.read_bytes())

    def rewrite_receipt(self, field, value):
        def mutate(files):
            receipt = json.loads(files["receipt.json"])
            receipt[field] = value
            files["receipt.json"] = json.dumps(receipt).encode()
        self.rewrite_candidate(mutate)

    def test_submitted_checkpoint_is_secret_free_and_does_not_wait_or_package(self):
        self.submit()
        self.assertFalse(self.output.exists())
        self.assertFalse(any("wait" in args or "--check-notarization" in args for args in self.calls))
        self.assertEqual(set(path.name for path in self.candidate.iterdir()), {"notarization.zip", "receipt.json"})
        receipt, payload = signing.candidate_contents(self.candidate_zip, self.candidate_digest, VERSION)
        self.assertEqual(payload, self.binary)
        self.assertEqual(receipt["state"], "submitted")
        self.assertIsNone(receipt["status"])
        encoded = (self.candidate / "receipt.json").read_text()
        for secret in ("fake private p12", "fake private p8", "never-print-me", str(self.work)):
            self.assertNotIn(secret, encoded)
        self.assertEqual(self.search_list, self.original_search_list)

    def test_late_acceptance_reuses_original_candidate_and_uuid_without_signing_or_submitting(self):
        self.submit()
        before = self.candidate_zip.read_bytes()
        self.calls.clear()
        self.tool_failure = "wait"
        with self.assertRaises(signing.SigningError):
            self.finalize()
        self.assertFalse(self.output.exists())
        self.assertFalse(self.work.exists())
        self.assertEqual(self.candidate_zip.read_bytes(), before)
        self.tool_failure = None
        os.environ["GITHUB_RUN_ATTEMPT"] = "2"
        self.finalize()
        self.assertTrue(self.output.exists())
        self.assertEqual(self.candidate_zip.read_bytes(), before)
        self.assertEqual(sum("wait" in args for args in self.calls), 2)
        self.assertTrue(all(args[3] == UUID for args in self.calls if "notarytool" in args))
        self.assertFalse(any("--sign" in args or "submit" in args or args[0] == "/usr/bin/security" for args in self.calls))
        with tarfile.open(self.output / signing.archive_name(VERSION)) as archive:
            self.assertEqual(archive.extractfile("xcb").read(), self.binary)

    def test_finalizer_refuses_signing_credentials(self):
        self.submit()
        os.environ["APPLE_DEVELOPER_ID_P12_PASSWORD"] = "must-not-be-here"
        self.calls.clear()
        with self.assertRaisesRegex(signing.SigningError, "must not receive"):
            self.finalize()
        self.assertFalse(self.calls)
        self.assertFalse(self.output.exists())

    def test_candidate_byte_or_identity_substitution_rejected_before_apple_tools(self):
        self.submit()
        original = self.candidate_zip.read_bytes()
        original_digest = self.candidate_digest
        cases = {"schemaVersion": 1, "repository": "other/xcb", "sourceSha": "c" * 40,
                 "workflowRunId": "322", "unsignedArtifactId": "124", "unsignedArtifactDigest": "c" * 64,
                 "version": "9.9.9", "producerAttempt": "2", "teamId": "Z9Y8X7W6V5",
                 "identifier": "other.binary", "submissionId": "missing", "state": "verified",
                 "status": "Accepted", "signedBinarySha256": "c" * 64, "submissionZipSha256": "c" * 64,
                 "unsignedArchiveSha256": "not-a-digest", "extra": "untrusted"}
        for field, value in cases.items():
            with self.subTest(field=field):
                self.candidate_zip.write_bytes(original)
                self.candidate_digest = original_digest
                self.rewrite_receipt(field, value)
                self.calls.clear()
                with self.assertRaises(signing.SigningError):
                    self.finalize()
                self.assertFalse(self.calls)
                self.assertFalse(self.output.exists())
                self.assertFalse(self.work.exists())

    def test_candidate_attempt_must_match_immutable_artifact_metadata(self):
        self.submit()
        os.environ["GITHUB_RUN_ATTEMPT"] = "3"
        os.environ["CANDIDATE_PRODUCER_ATTEMPT"] = "2"
        with self.assertRaisesRegex(signing.SigningError, "producer attempt"):
            self.finalize()

    def test_wrong_candidate_upload_digest_rejected_before_apple_tools(self):
        self.submit()
        self.candidate_digest = "0" * 64
        self.calls.clear()
        with self.assertRaisesRegex(signing.SigningError, "ZIP digest mismatch"):
            self.finalize()
        self.assertFalse(self.calls)

    def test_candidate_inventory_and_duplicate_receipt_fields_are_rejected(self):
        self.submit()
        original = self.candidate_zip.read_bytes()
        for kind in ("extra", "duplicate-json", "missing", "inner-extra"):
            with self.subTest(kind=kind):
                self.candidate_zip.write_bytes(original)
                def mutate(files):
                    if kind == "extra":
                        files["../credentials"] = b"bad"
                    elif kind == "missing":
                        del files["receipt.json"]
                    elif kind == "duplicate-json":
                        files["receipt.json"] = files["receipt.json"].replace(b'{', b'{"schemaVersion":2,', 1)
                    else:
                        inner = io.BytesIO()
                        with zipfile.ZipFile(inner, "w") as archive:
                            archive.writestr("xcb", self.binary)
                            archive.writestr("credentials", b"bad")
                        files["notarization.zip"] = inner.getvalue()
                        receipt = json.loads(files["receipt.json"])
                        receipt["submissionZipSha256"] = signing.digest(inner.getvalue())
                        files["receipt.json"] = json.dumps(receipt).encode()
                self.rewrite_candidate(mutate)
                self.calls.clear()
                with self.assertRaises(signing.SigningError):
                    self.finalize()
                self.assertFalse(self.calls)

    def test_symlink_and_oversized_candidate_entries_are_rejected_without_reading(self):
        self.submit()
        for mode, size in ((stat.S_IFLNK | 0o777, 20), (stat.S_IFREG | 0o600, 8193)):
            with self.subTest(mode=mode):
                with zipfile.ZipFile(self.candidate_zip, "w") as archive:
                    entry = zipfile.ZipInfo("receipt.json")
                    entry.external_attr = mode << 16
                    archive.writestr(entry, b"x" * size)
                    archive.writestr("notarization.zip", b"not parsed")
                self.candidate_digest = signing.digest(self.candidate_zip.read_bytes())
                with patch.object(signing.zipfile.ZipFile, "read", side_effect=AssertionError("must validate before read")):
                    with self.assertRaises(signing.SigningError):
                        self.finalize()

    def test_candidate_rejects_unexpected_compression_before_reading(self):
        self.submit()
        with zipfile.ZipFile(self.candidate_zip, "w", compression=zipfile.ZIP_BZIP2) as archive:
            archive.writestr("receipt.json", b"bounded but unsupported")
            archive.writestr("notarization.zip", b"not parsed")
        self.candidate_digest = signing.digest(self.candidate_zip.read_bytes())
        with patch.object(signing.zipfile.ZipFile, "read", side_effect=AssertionError("must validate before read")):
            with self.assertRaises(signing.SigningError):
                self.finalize()

    def test_cancelled_finalizer_preserves_candidate_and_removes_notary_key(self):
        self.submit()
        before = self.candidate_zip.read_bytes()
        def interrupt(args, timeout=60):
            if "wait" in args:
                raise KeyboardInterrupt()
            return self.tool(args, timeout)
        with patch.object(signing, "run", interrupt):
            with self.assertRaises(KeyboardInterrupt):
                self.finalize()
        self.assertFalse(self.work.exists())
        self.assertFalse(self.output.exists())
        self.assertEqual(self.candidate_zip.read_bytes(), before)

    def test_finalizer_rechecks_signature_and_strict_notarization(self):
        self.submit()
        for failure in ("--test-requirement", "--check-notarization"):
            with self.subTest(failure=failure):
                self.tool_failure = failure
                self.calls.clear()
                with self.assertRaises(signing.SigningError):
                    self.finalize()
                self.assertFalse(self.work.exists())
                self.assertFalse(self.output.exists())
                if failure == "--test-requirement":
                    self.assertFalse(any("notarytool" in args for args in self.calls))

    def test_finalizer_never_packages_when_notary_credential_cleanup_fails(self):
        self.submit()
        original = signing.cleanup_credentials
        def failed_cleanup(work):
            present = (work / "credentials").exists()
            original(work)
            if present:
                raise signing.SigningError("cleanup failed")
        with patch.object(signing, "cleanup_credentials", failed_cleanup):
            with self.assertRaisesRegex(signing.SigningError, "cleanup failed"):
                self.finalize()
        self.assertFalse(self.output.exists())
        self.assertFalse(self.work.exists())

    def test_changed_binary_during_apple_verification_is_never_packaged(self):
        self.submit()
        def mutate(args, timeout=60):
            result = self.tool(args, timeout)
            if "--check-notarization" in args:
                Path(args[-1]).write_bytes(b"changed during verification")
            return result
        with patch.object(signing, "run", mutate):
            with self.assertRaisesRegex(signing.SigningError, "changed during verification"):
                self.finalize()
        self.assertFalse(self.output.exists())
        self.assertFalse(self.work.exists())

    def test_cleanup_rejects_unowned_path(self):
        with self.assertRaisesRegex(signing.SigningError, "dedicated runner"):
            signing.cleanup(self.root)
        self.assertTrue(self.root.exists())


class ToolBoundaryTests(unittest.TestCase):
    def test_subprocess_receives_no_apple_or_provider_credentials(self):
        result = subprocess.CompletedProcess([], 0, stdout="ok", stderr="")
        with patch.dict(os.environ, {"APPLE_DEVELOPER_ID_P12_PASSWORD": "private", "OPENAI_API_KEY": "private"}), \
             patch.object(signing.subprocess, "run", return_value=result) as child:
            self.assertEqual(signing.run(["/usr/bin/security", "test"]), "ok")
        self.assertEqual(set(child.call_args.kwargs["env"]), {"PATH", "HOME", "LC_ALL"})

    def test_tool_errors_do_not_echo_secret_arguments_or_output(self):
        result = subprocess.CompletedProcess([], 1, stdout="private", stderr="private")
        with patch.object(signing.subprocess, "run", return_value=result):
            with self.assertRaisesRegex(signing.SigningError, "^Apple tool failed: security$"):
                signing.run(["/usr/bin/security", "-p", "private"])


@unittest.skipUnless(sys.platform == "darwin", "requires Apple's actual requirement parser")
class NativeRequirementTests(unittest.TestCase):
    def test_literal_source_is_parsed_and_wrong_publisher_is_rejected(self):
        with tempfile.TemporaryDirectory(prefix="xcb-native-requirement-") as directory:
            root = Path(directory)
            source = root / "fixture.c"
            binary = root / "fixture"
            source.write_text("int main(void) { return 0; }\n")
            subprocess.run(["/usr/bin/xcrun", "clang", str(source), "-o", str(binary)],
                           check=True, capture_output=True, timeout=30)
            requirement = 'identifier "dev.hraness.xcb"'
            signed = subprocess.run(["/usr/bin/codesign", "--force", "--sign", "-",
                                     "--identifier", "dev.hraness.xcb", "--requirements",
                                     "=designated => " + requirement, str(binary)],
                                    capture_output=True, text=True, timeout=30)
            self.assertEqual(signed.returncode, 0, signed.stderr)
            for predicate, expected in ((requirement, 0), (signing.apple_requirement(), 1)):
                verified = subprocess.run(["/usr/bin/codesign", "--verify", "--strict",
                                           "--test-requirement", "=" + predicate, str(binary)],
                                          capture_output=True, text=True, timeout=30)
                if expected == 0:
                    self.assertEqual(verified.returncode, 0, verified.stderr)
                else:
                    self.assertNotEqual(verified.returncode, 0)
                    self.assertIn("failed to satisfy specified code requirement", verified.stderr.lower())
                self.assertNotIn("No such file", verified.stderr)
            # Without '=', codesign treats source as a filename. This is the
            # native failure that the mocked verifier used to conceal.
            filename = subprocess.run(["/usr/bin/codesign", "--verify", "--test-requirement",
                                       requirement, str(binary)], capture_output=True,
                                      text=True, timeout=30)
            self.assertNotEqual(filename.returncode, 0)


if __name__ == "__main__":
    unittest.main()
