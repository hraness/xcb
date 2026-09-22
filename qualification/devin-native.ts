/** Credential-free native Devin qualification. See devin-native.md. */
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { chmod, copyFile, mkdir, mkdtemp, readFile, realpath, stat, writeFile } from 'node:fs/promises';
import { arch, release, tmpdir } from 'node:os';
import { dirname, isAbsolute, join } from 'node:path';

const argv = process.argv.slice(2);
const options = new Map<string, string>();
for (let i = 0; i < argv.length; i += 2) {
  if (!['--runtime', '--helper', '--test-binary', '--output', '--candidate-inventory'].includes(argv[i]!) || !argv[i + 1] || options.has(argv[i]!)) throw Error('Expected unique --runtime, --helper, --test-binary, --output paths and optional --candidate-inventory');
  options.set(argv[i]!, argv[i + 1]!);
}
if (!['--runtime', '--helper', '--test-binary', '--output'].every(key => options.has(key)) || [...options.values()].some(p => !isAbsolute(p))) throw Error('All four required paths and any candidate inventory must be absolute');
const hash = (bytes: Uint8Array | string) => createHash('sha256').update(bytes).digest('hex');
const harnessDigest = hash(await readFile(import.meta.path));
const candidate = options.has('--candidate-inventory');
const inventoryPath = options.get('--candidate-inventory') ?? new URL('./devin-3000.11.1-inventory.json', import.meta.url);
if ((await stat(inventoryPath)).size > 1024 * 1024) throw Error('Inventory exceeds bound');
const inventoryBytes = await readFile(inventoryPath);
const expected = JSON.parse(inventoryBytes.toString('utf8'));
if (!/^\d+\.\d+\.\d+$/.test(expected.version) || !/^[0-9a-f]{64}$/.test(expected.provider_sha256) || !Array.isArray(expected.tools) || expected.tools.length < 1 || expected.tools.length > 64) throw Error('Invalid exact runtime inventory');
const directory = await mkdtemp(join(await realpath(tmpdir()), 'xdb-'));
await chmod(directory, 0o700);
async function snapshot(source: string, name: string) {
  source = await realpath(source);
  if ((await stat(source)).size > 512 * 1024 * 1024) throw Error('Executable exceeds bound');
  const digest = hash(await readFile(source));
  const target = join(directory, name);
  await copyFile(source, target, 1);
  await chmod(target, 0o500);
  if (hash(await readFile(source)) !== digest || hash(await readFile(target)) !== digest) throw Error('Executable changed during snapshot');
  return { path: target, digest };
}
const provider = await snapshot(options.get('--runtime')!, 'provider');
if (provider.digest !== expected.provider_sha256) throw Error('Unqualified Devin executable');
const helper = await snapshot(options.get('--helper')!, 'helper');
const test = await snapshot(options.get('--test-binary')!, 'fixture');
const varint = (n: number) => { const bytes = []; do { const b = n & 127; n = Math.floor(n / 128); bytes.push(b | (n ? 128 : 0)); } while (n); return bytes; };
const concat = (...b: Uint8Array[]) => Buffer.concat(b);
const field = (n: number, value: string | Uint8Array) => { const bytes = typeof value === 'string' ? Buffer.from(value) : value; return concat(new Uint8Array([...varint(n * 8 + 2), ...varint(bytes.length)]), bytes); };
const frame = (bytes: Uint8Array, flags = 0) => { const header = Buffer.alloc(5); header[0] = flags; header.writeUInt32BE(bytes.length, 1); return concat(header, bytes); };
function fields(bytes: Uint8Array) {
  let offset = 0;
  const integer = () => { let n = 0, shift = 0; for (;;) { if (offset >= bytes.length || shift > 49) throw Error('Invalid protobuf integer'); const b = bytes[offset++]!; n += (b & 127) * 2 ** shift; if (!(b & 128)) return n; shift += 7; } };
  const result: { number: number; bytes: Uint8Array }[] = [];
  while (offset < bytes.length) {
    const tag = integer(), wire = tag % 8, number = Math.floor(tag / 8);
    if (!number) throw Error('Invalid protobuf field');
    if (wire === 0) { integer(); continue; }
    const length = wire === 2 ? integer() : wire === 1 ? 8 : wire === 5 ? 4 : -1;
    if (length < 0 || offset + length > bytes.length) throw Error('Invalid protobuf length');
    if (wire === 2) result.push({ number, bytes: bytes.slice(offset, offset + length) });
    offset += length;
  }
  return result;
}
const canonical = (v: unknown): string => Array.isArray(v) ? `[${v.map(canonical).join(',')}]` : v !== null && typeof v === 'object' ? `{${Object.entries(v).sort(([a], [b]) => a.localeCompare(b)).map(([k, x]) => `${JSON.stringify(k)}:${canonical(x)}`).join(',')}}` : JSON.stringify(v);
function inventory(bytes: Uint8Array) {
  const view = Buffer.from(bytes);
  if (view.length < 5 || view[0] !== 0 || view.readUInt32BE(1) !== view.length - 5) throw Error('Unsupported model request frame');
  return fields(view.subarray(5)).filter(f => f.number === 10).map(f => {
    const row = new Map(fields(f.bytes).map(f => [f.number, Buffer.from(f.bytes).toString()]));
    return { name: row.get(1), inputSchema: JSON.parse(row.get(3)!) };
  }).sort((a, b) => a.name!.localeCompare(b.name!));
}
type Scenario = 'broker' | 'exec' | 'write' | 'config_write' | 'webfetch';
async function fixture(scenario: Scenario, index: number) {
  const run = join(directory, `r${index}`);
  await mkdir(run, { mode: 0o700 });
  let step = 0, overflow = false, canaryLeak = false, unexpectedFetch = false, toolFreeRequests = 0;
  const requests: { method: string; bytes: number; sha256: string }[] = [];
  const inventoryHashes = new Set<string>();
  const negative = {
    exec: { name: 'exec', args: { command: `touch '${join(run, 'scratch/home/exec-result.txt')}'` } },
    write: { name: 'write', args: { file_path: join(run, 'consumer/native-write.txt'), content: 'FORBIDDEN' } },
    config_write: { name: 'write', args: { file_path: join(run, 'scratch/home/.config/devin/config.json'), content: '{}' } },
    webfetch: { name: 'webfetch', args: { url: '' } },
  };
  const calls: { name: string; args: Record<string, unknown> }[] = scenario === 'broker' ? [
    { name: 'notebook_read', args: { notebook_path: join(run, 'persistent-auth/secret.ipynb') } },
    { name: 'notebook_read', args: { notebook_path: join(run, 'consumer/secret.ipynb') } },
    { name: 'notebook_read', args: { notebook_path: join(run, 'scratch/home/workspace-link.ipynb') } },
    { name: 'notebook_read', args: { notebook_path: join(run, 'scratch/home/stdin-link.ipynb') } },
    { name: 'read', args: { file_path: join(run, 'consumer/secret.ipynb') } },
    { name: 'mcp_list_tools', args: { server_name: 'xcb' } },
    { name: 'mcp_call_tool', args: { server_name: 'xcb', tool_name: 'workspace_write', arguments: { path: 'fixture.txt', text: 'SYNTHETIC_BROKER_WRITE', expectedRevision: null } } },
    { name: 'mcp_call_tool', args: { server_name: 'xcb', tool_name: 'workspace_read', arguments: { path: 'fixture.txt' } } },
  ] : [negative[scenario]];
  let captureError: string | undefined;
  const response = (bytes: Uint8Array) => new Response(concat(frame(bytes), frame(Buffer.from('{}'), 2)), { headers: { 'content-type': 'application/connect+proto' } });
  const server = Bun.serve({ hostname: '127.0.0.1', port: 0, maxRequestBodySize: 16 * 1024 * 1024, async fetch(request) {
    try {
      const pathname = new URL(request.url).pathname, bytes = Buffer.from(await request.arrayBuffer());
      if (requests.length >= 256 || requests.reduce((n, r) => n + r.bytes, bytes.length) > 64 * 1024 * 1024) { overflow = true; throw Error('Request capture bound'); }
      canaryLeak ||= bytes.includes('SYNTHETIC_ACCOUNT_CANARY') || bytes.includes('SYNTHETIC_WORKSPACE_CANARY');
      requests.push({ method: pathname.slice(pathname.lastIndexOf('/') + 1), bytes: bytes.length, sha256: hash(bytes) });
      if (pathname === '/should-not-fetch') unexpectedFetch = true;
      if (pathname.endsWith('/GetCliModelConfigs')) {
        // The exact runtime must negotiate a non-default model rather than
        // pass the fixture using its built-in default and an empty catalog.
        const model = (id: string, label: string) => field(1, concat(field(1, label), new Uint8Array([...varint(18 * 8), ...varint(262144)]), field(22, id)));
        return new Response(concat(model('swe-1-6-fast', 'Synthetic default'), model('xcb-fixture-model', 'Synthetic selected')), { headers: { 'content-type': 'application/proto' } });
      }
      if (pathname.endsWith('/GetChatMessage')) {
        const observed = inventory(bytes);
        // The pinned runtime also requests a session title with no tools. A
        // tool-free request receives static text and cannot advance this probe.
        if (observed.length === 0) {
          if (++toolFreeRequests > 8) throw Error('Too many tool-free model requests');
          return response(concat(field(1, 'synthetic-title'), field(3, 'Synthetic fixture'), new Uint8Array([40, 1])));
        }
        const selected = fields(bytes.subarray(5)).filter(f => f.number === 21).map(f => Buffer.from(f.bytes).toString());
        if (selected.length !== 1 || selected[0] !== 'xcb-fixture-model') throw Error('Selected model did not reach inference request');
        if (canonical(observed) !== canonical(expected.tools)) {
          // Candidate discovery never admits a changed inventory. Preserve
          // bounded synthetic observations for review, then fail the fixture.
          const diagnostic = JSON.stringify({ version: expected.version, provider_sha256: provider.digest, tools: observed }, null, 2);
          if (Buffer.byteLength(diagnostic) <= 1024 * 1024) await writeFile(join(run, 'observed-inventory.json'), diagnostic + '\n', { mode: 0o600, flag: 'wx' }).catch(() => {});
          throw Error('Effective native tool inventory changed');
        }
        inventoryHashes.add(hash(canonical(observed)));
        const call = calls[step++];
        if (step > calls.length + 1) throw Error('Unexpected extra inference request');
        return response(call ? concat(field(1, 'synthetic-message'), field(6, concat(field(1, `synthetic-call-${step}`), field(2, call.name), field(3, JSON.stringify(call.args)))), new Uint8Array([40, 2])) : concat(field(1, 'synthetic-message'), field(3, 'SYNTHETIC_COMPLETE'), new Uint8Array([40, 1])));
      }
      return new Response(new Uint8Array(pathname.endsWith('/GetCliTeamSettings') ? [8, 1] : []), { headers: { 'content-type': 'application/proto' } });
    } catch (error) { captureError = error instanceof Error ? error.message : 'capture failed'; return new Response(null, { status: 500 }); }
  } });
  negative.webfetch.args.url = `http://127.0.0.1:${server.port}/should-not-fetch`;
  const spec = { directory: run, provider: provider.path, helper: helper.path, helper_sha256: helper.digest, port: server.port, scenario, ...(candidate ? { candidate_sha256: expected.provider_sha256 } : {}) };
  const specPath = join(run, 'spec.json');
  await writeFile(specPath, JSON.stringify(spec), { mode: 0o600 });
  const child = spawn(test.path, ['--ignored', '--exact', 'devin::wire::tests::native_fixture::installed_runtime_uses_native_broker_under_production_profile', '--nocapture'], { env: { PATH: '/usr/bin:/bin', XCB_DEVIN_FIXTURE_SPEC: specPath }, stdio: ['ignore', 'pipe', 'pipe'] });
  let output = '', error = '';
  for (const [stream, isError] of [[child.stdout, false], [child.stderr, true]] as const) stream.on('data', bytes => {
    if (overflow) return;
    const text = bytes.toString();
    if (output.length + error.length + text.length > 262144) { overflow = true; child.kill('SIGTERM'); return; }
    if (isError) error += text; else output += text;
  });
  const timer = setTimeout(() => child.kill('SIGTERM'), 45000);
  const stopped = await new Promise<{ code: number | null; signal: string | null }>(resolve => child.once('close', (code, signal) => resolve({ code, signal })));
  clearTimeout(timer);
  await server.stop(true);
  const evidence = JSON.parse(await readFile(join(run, 'native-evidence.json'), 'utf8').catch(() => '{}'));
  const expectedSteps = scenario === 'broker' ? step === calls.length + 1 : step >= calls.length && step <= calls.length + 1;
  const passed = stopped.code === 0 && !overflow && !captureError && !canaryLeak && !unexpectedFetch && inventoryHashes.size === 1 && expectedSteps && evidence.process_joined && evidence.bridge_joined;
  const receipt = { scenario, passed, ...evidence, checks: { capture_error: captureError ?? null, no_canary_leak: !canaryLeak, no_native_webfetch: !unexpectedFetch, exact_inventory: !captureError && inventoryHashes.size === 1, steps: step, tool_free_requests: toolFreeRequests, expected_steps: expectedSteps, no_overflow: !overflow }, inventory_sha256: [...inventoryHashes], stdout_sha256: hash(output), stderr_sha256: hash(error), requests, stopped };
  if (!passed) { await writeFile(join(run, 'diagnostics.txt'), output + '\n' + error, { mode: 0o600 }); console.error(JSON.stringify({ scenario, passed, directory: run, captureError, stopped })); }
  return receipt;
}
const scenarios = [];
for (const [index, scenario] of (['broker', 'exec', 'write', 'config_write', 'webfetch'] as const).entries()) {
  const result = await fixture(scenario, index);
  scenarios.push(result);
  if (!result.process_joined || !result.bridge_joined) break;
}
const passed = scenarios.length === 5 && scenarios.every(s => s.passed) && harnessDigest === hash(await readFile(import.meta.path)) && hash(inventoryBytes) === hash(await readFile(inventoryPath));
const receipt = { schema: 2, passed, observed_at: new Date().toISOString(), host: { platform: process.platform, arch: arch(), release: release() }, credential_free: true, live_provider_qualification: false, candidate, runtime_version: expected.version, provider_sha256: provider.digest, inventory_sha256: hash(inventoryBytes), helper_sha256: helper.digest, fixture_binary_sha256: test.digest, harness_sha256: harnessDigest, scenarios };
await mkdir(dirname(options.get('--output')!), { recursive: true, mode: 0o700 });
await writeFile(options.get('--output')!, JSON.stringify(receipt, null, 2) + '\n', { mode: 0o600 });
console.log(JSON.stringify({ passed, provider_sha256: provider.digest, helper_sha256: helper.digest, scenarios: scenarios.map(s => ({ scenario: s.scenario, passed: s.passed, calls: s.calls?.length, steps: s.checks.steps })) }));
if (!passed) process.exitCode = 1;
