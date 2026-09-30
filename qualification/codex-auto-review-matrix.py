#!/usr/bin/python3
"""Recheck every retained Codex artifact with the current shared configuration.

Run through the host scheduler's mac-native lane. This calls the existing
credential-free native canary and loopback inventory fixtures; it never reads
authentication or makes a real model request.
"""
import argparse
import datetime
import hashlib
import json
import pathlib
import re
import subprocess
import sys
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--releases', type=pathlib.Path, required=True, help='directory containing VERSION-aarch64-apple-darwin/bin/codex packages')
parser.add_argument('--output', type=pathlib.Path, required=True, help='private scratch and raw receipts')
parser.add_argument('--evidence', type=pathlib.Path, required=True, help='normalized compatibility report')
parser.add_argument('--resume', action='store_true', help='reuse complete same-day fixture evidence after checking its recorded identities')
args = parser.parse_args()
assert sys.platform == 'darwin', 'This fixture requires macOS Seatbelt'
repo = pathlib.Path(__file__).resolve().parents[1]
output = args.output.resolve()
output.mkdir(mode=0o700, parents=True, exist_ok=True)
source = (repo/'crates/xcb-runtime/src/codex/config.rs').read_text()
sha = lambda value: hashlib.sha256(value).hexdigest()
constant = lambda name: re.search(r'pub const ' + name + r': &str = "([^"]+)"', source).group(1)
pairs = [(constant('VERSION'), constant('BINARY_SHA256'))]
block = source.split('pub const REVIEWED_BUILDS: &[(&str, &str, &str)] = &[', 1)[1].split('];', 1)[0]
literals = [json.loads(value) for value in re.findall(r'"(?:[^"\\]|\\.)*"', block)]
assert len(literals) % 3 == 0
pairs += [(version, digest) for version, digest, _ in zip(*[iter(literals)] * 3)]
assert len({version for version, _ in pairs}) == len(pairs)
results = []

def reusable(root, version, digest):
    """Recover this matrix's completed fixtures, never an interrupted case."""
    try:
        native_path = root/'native/latest-receipt.json'
        inventory_path = root/'inventory/inventory.json'
        verdict_path = root/'inventory/verdict.json'
        config_path = root/'config-readback.json'
        assert all(0 <= time.time() - path.stat().st_mtime <= 86400
                   for path in (native_path, inventory_path, verdict_path, config_path)), 'receipt age'
        native = json.loads(native_path.read_text())
        inventory = json.loads(inventory_path.read_text())
        verdict = json.loads(verdict_path.read_text())
        config = json.loads(config_path.read_text())['config']
        assert native['binarySha256'] == inventory['binarySha256'] == verdict['binarySha256'] == digest, 'binary binding'
        assert inventory['version'] == verdict['version'] == version, 'version binding'
        assert native['probeSha256'] == sha((repo/'qualification/codex-native.py').read_bytes()), 'native harness changed'
        assert inventory['harnessSha256'] == sha((repo/'qualification/codex-inventory.py').read_bytes()), 'inventory harness changed'
        sandbox = (repo/'crates/xcb-runtime/src/sandbox.rs').read_text().split('pub fn codex_seatbelt(', 1)[1].split('\n/// Devin', 1)[0]
        assert native['sandboxFunctionSha256'] == sha(sandbox.encode()), 'sandbox function changed'
        # Rebuild the exact fixed config used by codex-native.py. Comments
        # outside configuration() do not change this execution identity.
        strings = lambda block: [json.loads(value) for value in re.findall(r'"(?:[^"\\]|\\.)*"', block)]
        features = strings(source.split('pub const ACCOUNT_FEATURES: &[&str] = &[', 1)[1].split('];', 1)[0])
        body = source.split('pub fn configuration(', 1)[1].split('\npub fn thread_configuration(', 1)[0]
        first, last = [strings(block) for block in re.findall(r'lines\.extend\(\s*\[(.*?)\]\s*\.map\(str::to_owned\)', body, re.S)]
        run_root = pathlib.Path(native['runDirectory']).resolve(strict=True)
        assert run_root.parent == (root/'native').resolve(), 'native run directory'
        expected = 'model_catalog_json = ' + json.dumps(str(run_root/'models.json')) + '\n'
        expected += '\n'.join(first + [json.dumps(name) + ' = false' for name in features] + last)
        assert native['configSha256'] == sha(expected.encode()), 'rendered configuration changed'
        assert (run_root/'scratch/profile/config.toml').read_text() == expected, 'retained configuration changed'
        assert config['approval_policy'] == 'on-request' and config['approvals_reviewer'] == 'auto_review', 'Auto Review readback'
        assert config['sandbox_mode'] == 'read-only', 'sandbox readback'
        assert native['rootExitCode'] == 0 and not native['errors'] and len(native['checks']) == 32, 'native completion'
        assert all(row['matched'] for row in native['checks']), 'native canaries'
        assert all(native[key] is True for key in ('stdioJoined', 'processGroupAbsent', 'binaryUnchanged', 'sourceFunctionUnchanged')), 'native custody'
        assert all(native[key] and all(native[key].values()) for key in ('protectedFilesUnchanged', 'protectedMetadataUnchanged')), 'protected data changed'
        assert verdict['outcome'] == 'passed' and not verdict['failures'], 'inventory verdict'
        assert verdict['inventorySha256'] == sha(inventory_path.read_bytes()), 'inventory digest'
        cases = inventory['cases']
        assert len(cases) == verdict['cases'] and cases, 'inventory case count'
        retained = {sha(path.read_bytes()) for path in (root/'inventory').glob('case-*/evidence.json')}
        assert all(row['evidenceSha256'] in retained for row in cases), 'missing inventory case evidence'
        assert all(row['rootExitCode'] == 0 and row['stdioJoined'] and row['listenerJoined']
                   and row['emptyManifestVerified'] and row['exactDynamicManifestVerified']
                   and row['permittedCallbackVerified'] and row['forgedBuiltinRejections'] == 9 for row in cases), 'incomplete inventory case'
        return True, None
    except (AssertionError, KeyError, OSError, TypeError, ValueError) as error:
        return False, str(error)

def run(command, log):
    with log.open('wb') as stream:
        # Each fixture bounds its own RPCs and joins its owned provider group.
        # Do not kill the fixture before that cleanup can run.
        return subprocess.run(command, cwd=repo, stdout=stream, stderr=subprocess.STDOUT).returncode

for version, digest in pairs:
    executable = (args.releases/(version + '-aarch64-apple-darwin')/'bin/codex').resolve(strict=True)
    assert sha(executable.read_bytes()) == digest, 'Executable digest mismatch for ' + version
    root = output/version
    root.mkdir(mode=0o700, parents=True, exist_ok=True)
    reused, reason = reusable(root, version, digest) if args.resume else (False, None)
    if not reused and any(root.iterdir()):
        print(json.dumps({'version': version, 'reuseSkipped': reason or 'resume not requested'}), flush=True)
        root = root/('attempt-' + str(time.time_ns()))
        root.mkdir(mode=0o700)
    common = ['--executable', str(executable), '--expect-version', version, '--expect-sha256', digest]
    native = [sys.executable, 'qualification/codex-native.py', *common, '--output', str(root/'native'),
              '--config-readback', str(root/'config-readback.json')]
    native_exit = None if reused else run(native, root/'native.log')
    inventory = [sys.executable, 'qualification/codex-inventory.py', *common, '--output', str(root/'inventory')]
    inventory_exit = None if reused else run(inventory, root/'inventory.log')
    native_file = root/'native/latest-receipt.json'
    native_receipt = json.loads(native_file.read_text()) if native_file.exists() else {}
    inventory_file = root/'inventory/inventory.json'
    inventory_receipt = json.loads(inventory_file.read_text()) if inventory_file.exists() else {}
    verdict_file = root/'inventory/verdict.json'
    verdict = json.loads(verdict_file.read_text()) if verdict_file.exists() else {}
    config_file = root/'config-readback.json'
    config_readback = json.loads(config_file.read_text()) if config_file.exists() else {}
    config = config_readback.get('config') or {}
    auto_review = config.get('approval_policy') == 'on-request' and config.get('approvals_reviewer') == 'auto_review'
    record = {
        'version': version, 'binarySha256': digest,
        'nativeExitCode': native_exit, 'inventoryExitCode': inventory_exit,
        'completedEvidenceReused': reused,
        'autoReviewConfigReadbackVerified': auto_review,
        'nativeChecks': [{key: item.get(key) for key in ('label', 'method', 'expectedAllowed', 'allowed', 'matched')} for item in native_receipt.get('checks', [])],
        'nativeCustody': {key: native_receipt.get(key) for key in ('rootExitCode', 'stdioJoined', 'processGroupAbsent', 'binaryUnchanged', 'sourceFunctionUnchanged')},
        'protectedFilesUnchanged': bool(native_receipt.get('protectedFilesUnchanged')) and all(native_receipt['protectedFilesUnchanged'].values()),
        'protectedMetadataUnchanged': bool(native_receipt.get('protectedMetadataUnchanged')) and all(native_receipt['protectedMetadataUnchanged'].values()),
        'nativeReceiptSha256': sha(native_file.read_bytes()) if native_file.exists() else None,
        'configReadbackSha256': sha(config_file.read_bytes()) if config_file.exists() else None,
        'inventory': inventory_receipt, 'inventoryVerdict': verdict,
        'passed': (reused or native_exit == 0 and inventory_exit == 0) and auto_review,
    }
    if native_exit not in (0, None): record['nativeFailureTail'] = (root/'native.log').read_text(errors='replace')[-4000:].replace(str(root), '/synthetic')
    if inventory_exit not in (0, None): record['inventoryFailureTail'] = (root/'inventory.log').read_text(errors='replace')[-4000:].replace(str(root), '/synthetic')
    results.append(record)
    print(json.dumps({'version': version, 'nativeExitCode': native_exit, 'inventoryExitCode': inventory_exit,
                      'completedEvidenceReused': reused, 'autoReviewConfigReadbackVerified': auto_review, 'passed': record['passed']}), flush=True)

report = {
    'schema': 'xcb.codex-auto-review-compatibility.v1', 'observedDate': datetime.date.today().isoformat(),
    'runtimeConfigSourceSha256': sha(source.encode()), 'matrixHarnessSha256': sha(pathlib.Path(__file__).read_bytes()),
    'authentication': 'none; isolated empty profiles; no existing credentials accessed',
    'realProviderInference': False, 'productionQualified': False,
    'nativeFixture': 'production Seatbelt, synthetic host RPC canaries, no model turns',
    'inventoryFixture': 'production Seatbelt narrowed to one owned loopback port, synthetic Responses requests',
    'approvalPolicy': 'on-request', 'approvalsReviewer': 'auto_review', 'nativeDelegationEnabled': False,
    'results': results, 'passed': bool(results) and all(row['passed'] for row in results),
}
assert (repo/'crates/xcb-runtime/src/codex/config.rs').read_text() == source, 'Runtime configuration changed during compatibility matrix'
args.evidence.write_text(json.dumps(report, indent=2, sort_keys=True) + '\n')
raise SystemExit(not report['passed'])
