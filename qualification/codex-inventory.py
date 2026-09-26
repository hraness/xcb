#!/usr/bin/python3
"""Exact Codex build under the production configuration and Seatbelt policy, with
egress narrowed to one owned loopback port: verifies the serialized tool manifest,
the permitted host callback and forged builtin rejection. No login, credentials or
real model requests."""
import argparse, datetime, hashlib, json, os, pathlib, re, selectors, signal, subprocess, sys, threading, time, uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--executable', required=True, type=pathlib.Path)
parser.add_argument('--output', required=True, type=pathlib.Path)
parser.add_argument('--inventory', type=pathlib.Path, help='write the inventory record here (default: OUTPUT/inventory.json)')
parser.add_argument('--wire-trace', type=pathlib.Path, help='also record one permitted-callback turn, normalized for the Rust replay fixture')
parser.add_argument('--expect-version', help='candidate version to verify instead of the baked config.rs VERSION (requires --expect-sha256)')
parser.add_argument('--expect-sha256', help='candidate binary digest to verify instead of the baked BINARY_SHA256 (requires --expect-version)')
args = parser.parse_args()
assert sys.platform == 'darwin', 'This fixture requires macOS Seatbelt'
REPO = pathlib.Path(__file__).resolve().parents[1]
ROOT = args.output.resolve(); ROOT.mkdir(mode=0o700, parents=True, exist_ok=True)
BIN = args.executable.resolve(strict=True)
sha = lambda b: hashlib.sha256(b).hexdigest()
config_source = (REPO/'crates/xcb-runtime/src/codex/config.rs').read_text()
const = lambda name: re.search(r'pub const '+name+r': &str = "([^"]+)"', config_source).group(1)
assert (args.expect_version is not None) == (args.expect_sha256 is not None), \
    '--expect-version and --expect-sha256 go together'
BAKED_VERSION, BAKED_SHA256 = const('VERSION'), const('BINARY_SHA256')
VERSION, EXPECTED = args.expect_version or BAKED_VERSION, args.expect_sha256 or BAKED_SHA256
strings = lambda block: [json.loads(x) for x in re.findall(r'"(?:[^"\\]|\\.)*"', block)]
array = lambda head: strings(config_source.split(head, 1)[1].split('];', 1)[0])
QUALIFIED = array('pub const QUALIFIED_MODELS: &[&str] = &[')
ACCOUNT_FEATURES = array('pub const ACCOUNT_FEATURES: &[&str] = &[')
EXTRA_FEATURES = array('const EXTRA_FEATURES: &[&str] = &[')
CONTROLS = dict(tool_mode='direct', shell_type='disabled', apply_patch_tool_type=None, experimental_supported_tools=[],
                supports_search_tool=False, supports_experimental_context=False, multi_agent_version='disabled', node_repl_disabled=True)
# Builtins a compromised or confused model might name; none may reach the host or run.
FORGED = [('exec_command', {'cmd': 'touch forged-exec'}), ('write_stdin', {'session_id': 1, 'chars': 'forged\n'}),
          ('apply_patch', {'input': '*** Begin Patch\n*** Add File: forged-patch.txt\n+forged\n*** End Patch\n'}),
          ('view_image', {'path': 'canary.png'}), ('update_plan', {'plan': [{'step': 'forged', 'status': 'pending'}]}),
          ('request_user_input', {'questions': [{'id': 'forged', 'header': 'Forged', 'question': 'Forged?', 'options': [{'label': 'Yes', 'description': 'Forged'}]}]}),
          ('spawn_agent', {'message': 'forged'}), ('context', {}), ('request_permissions', {'permissions': {'file_system': {'write': ['/']}}})]
ECHO = {'type': 'function', 'name': 'synthetic_echo', 'description': 'Synthetic host echo tool.',
        'inputSchema': {'type': 'object', 'properties': {'text': {'type': 'string'}}, 'required': ['text'], 'additionalProperties': False}}
EXPECTED_MANIFEST = [{'type': 'namespace', 'name': 'functions', 'description': '', 'tools': [
    {'type': 'function', 'name': ECHO['name'], 'description': ECHO['description'], 'strict': False, 'parameters': ECHO['inputSchema']}]}]
OVERRIDES = {'model_provider': 'qualification', 'requires_openai_auth': False, 'supports_websockets': False, 'features.enable_request_compression': False}

with BIN.open('rb') as f: binary = f.read(512 * 1024 * 1024 + 1)
assert len(binary) <= 512 * 1024 * 1024 and sha(binary) == EXPECTED, 'Provider is not the admitted executable'
marker = b'{\n  "models":'
assert binary.count(marker) == 1, 'Ambiguous bundled catalog'
catalog_rows, _ = json.JSONDecoder().raw_decode(binary[binary.index(marker):][:4 * 1024 * 1024].decode('utf-8', 'replace'))
del binary
version_output = subprocess.run([str(BIN), '--version'], capture_output=True, text=True, timeout=30, env={'PATH': '/usr/bin:/bin'}).stdout.strip()
assert version_output == 'codex-cli ' + VERSION, 'Provider version is not the admitted version: ' + version_output
SCHEMA_COMMAND = ['app-server', 'generate-json-schema', '--experimental', '--out']
def schema_digest():
    # Offline code generation from the checked executable with an empty, disposable home.
    home = ROOT/('schema-' + uuid.uuid4().hex); out = home/'schema'; out.mkdir(mode=0o700, parents=True)
    subprocess.run([str(BIN), *SCHEMA_COMMAND, str(out)], check=True, capture_output=True, timeout=120,
                   env={'HOME': str(home), 'CODEX_HOME': str(home), 'PATH': '/usr/bin:/bin', 'TMPDIR': str(home)})
    digest = hashlib.sha256()
    for path in sorted(p.relative_to(out).as_posix() for p in out.rglob('*.json')):
        digest.update(path.encode() + b'\0' + (out/path).read_bytes() + b'\0')
    return digest.hexdigest()
SCHEMA = schema_digest()
assert SCHEMA == const('SCHEMA_SHA256'), 'app-server schema digest differs from config.rs SCHEMA_SHA256: observed ' + SCHEMA

def configuration(catalog, port):
    body = config_source.split('pub fn configuration(', 1)[1].split('\npub fn thread_configuration(', 1)[0]
    blocks = re.findall(r'lines\.extend\(\s*\[(.*?)\]\s*\.map\(str::to_owned\)', body, re.S)
    assert len(blocks) == 2
    first, last = [strings(block) for block in blocks]
    assert first.count('model_provider = "openai"') == 1 and first[-1] == '[features]'
    first = ['model_provider = "qualification"' if line == 'model_provider = "openai"' else line for line in first]
    lines = ['model_catalog_json = ' + json.dumps(str(catalog))] + first + [name + ' = false' for name in ACCOUNT_FEATURES]
    lines += ['enable_request_compression = false'] + last
    return '\n'.join(lines + ['[model_providers.qualification]', 'name = "qualification"', f'base_url = "http://127.0.0.1:{port}/v1"',
                              'wire_api = "responses"', 'requires_openai_auth = false', 'supports_websockets = false', ''])

def thread_configuration(effort):
    body = config_source.split('pub fn thread_configuration(', 1)[1].split('\npub(crate) fn validate_config(', 1)[0]
    template = re.search(r'json!\((\{"features":features,.*?\})\);', body).group(1).replace('"features":features', '"features":null')
    value = json.loads(template)
    value['features'] = {name: False for name in ACCOUNT_FEATURES + EXTRA_FEATURES}
    value['model_reasoning_effort'] = effort
    return value

def thread_request(model, effort, cwd, tools):
    # Mirrors crates/xcb-runtime/src/codex.rs thread_request; only modelProvider names the loopback provider.
    body = (REPO/'crates/xcb-runtime/src/codex.rs').read_text().split('fn thread_request(', 1)[1].split('\n    fn thread_readback(', 1)[0]
    keys = re.findall(r'"([A-Za-z]+)":', re.search(r'json!\((\{"model".*?\})\)\n', body).group(1))
    request = {'model': model, 'modelProvider': 'qualification', 'config': thread_configuration(effort), 'cwd': str(cwd), 'approvalPolicy': 'never',
               'sandbox': 'read-only', 'ephemeral': True, 'environments': [], 'runtimeWorkspaceRoots': [], 'selectedCapabilityRoots': [],
               'dynamicTools': tools, 'baseInstructions': 'Synthetic base instructions.', 'developerInstructions': 'Synthetic developer instructions.',
               'allowProviderModelFallback': False}
    assert keys == list(request), 'xcb thread request shape changed: ' + ','.join(keys)
    return request

def sse(response_id, item):
    usage = {'input_tokens': 1, 'input_tokens_details': {'cached_tokens': 0}, 'output_tokens': 1, 'output_tokens_details': {'reasoning_tokens': 0}, 'total_tokens': 2}
    events = [('response.created', {'type': 'response.created', 'response': {'id': response_id, 'status': 'in_progress', 'output': []}}),
              ('response.output_item.done', {'type': 'response.output_item.done', 'output_index': 0, 'item': item}),
              ('response.completed', {'type': 'response.completed', 'response': {'id': response_id, 'status': 'completed', 'output': [item], 'usage': usage}})]
    return b''.join(f'event: {name}\ndata: {json.dumps(data)}\n\n'.encode() for name, data in events)
message = lambda text: {'id': 'msg_' + uuid.uuid4().hex, 'type': 'message', 'role': 'assistant', 'status': 'completed', 'phase': 'final_answer', 'content': [{'type': 'output_text', 'text': text, 'annotations': []}]}
call = lambda call_id, name, arguments: {'id': 'fc_' + call_id, 'type': 'function_call', 'status': 'completed', 'call_id': call_id, 'name': name, 'arguments': json.dumps(arguments)}

def wire_effort(model, effort):
    # "ultra" adds automatic task delegation, which the qualified configuration disables;
    # Codex then sends the model's non-delegating multi-agent effort, or max.
    row = next(row for row in catalog_rows['models'] if row['slug'] == model)
    return row.get('multi_agent_reasoning_effort') or 'max' if effort == 'ultra' else effort

def run_case(model, effort, trace=False):
    case = ROOT/('case-' + uuid.uuid4().hex); scratch = case/'scratch'; profile = scratch/'profile'; home = scratch/'home'; cwd = scratch/'work'
    for path in [case, scratch, profile, home, home/'tmp', cwd]: path.mkdir(mode=0o700)
    exe = case/'provider'; subprocess.run(['/bin/cp', '-c', str(BIN), str(exe)], check=True); exe.chmod(0o500)
    ca_bundle = case/'public-ca.pem'; ca_bundle.write_bytes(pathlib.Path('/private/etc/ssl/cert.pem').read_bytes()); ca_bundle.chmod(0o600)
    catalog = case/'models.json'
    rows = [dict(row, **CONTROLS) for row in catalog_rows['models'] if row['slug'] in QUALIFIED]
    catalog.write_text(json.dumps({'models': rows}, separators=(',', ':'), ensure_ascii=False, sort_keys=True)); catalog.chmod(0o600)
    evidence = {'model': model, 'reasoningEffort': effort, 'requests': [], 'frames': [], 'violations': [], 'rejections': []}
    lock = threading.Lock(); script = {'step': 0}
    steps = ['manifest', 'echo-output'] if trace else ['empty', 'manifest', 'echo-output'] + ['forged-%d' % i for i in range(len(FORGED))]

    def output_for(body, call_id):
        items = [item for item in body.get('input', []) if item.get('type') == 'function_call_output' and item.get('call_id') == call_id]
        return items[0].get('output') if len(items) == 1 else None
    def manifest(body):
        carriers = [item for item in body.get('input', []) if item.get('type') == 'additional_tools']
        return (carriers[0].get('tools') if len(carriers) == 1 else 'ambiguous'), body.get('tools')

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_): pass
        def do_GET(self):
            with lock: evidence['violations'].append('unexpected GET ' + self.path[:200])
            self.send_response(404); self.end_headers()
        def do_POST(self):
            size = int(self.headers.get('content-length') or 0)
            body = json.loads(self.rfile.read(min(size, 8 * 1024 * 1024)))
            with lock:
                step = steps[script['step']] if script['step'] < len(steps) else 'overflow'; script['step'] += 1
                evidence['requests'].append({'step': step, 'path': self.path, 'model': body.get('model'), 'reasoning': body.get('reasoning'),
                                             'manifest': manifest(body)[0], 'topLevelTools': body.get('tools'),
                                             'functionOutputs': [item for item in body.get('input', []) if item.get('type') == 'function_call_output']})
                bad = lambda why: evidence['violations'].append(step + ': ' + why)
                if self.path != '/v1/responses': bad('path ' + self.path)
                if body.get('model') != model or (body.get('reasoning') or {}).get('effort') != wire_effort(model, effort): bad('model or wire effort changed')
                tools, top = manifest(body)
                if top not in (None, []): bad('top-level tools present')
                if step == 'empty':
                    evidence['emptyManifestVerified'] = tools == []
                    reply = message('synthetic-empty-complete')
                elif step == 'manifest':
                    evidence['exactDynamicManifestVerified'] = tools == EXPECTED_MANIFEST
                    reply = call('call_echo', 'synthetic_echo', {'text': 'broker-echo'})
                elif step == 'echo-output':
                    output = output_for(body, 'call_echo')
                    evidence['permittedCallbackVerified'] = evidence.get('hostCallbacks') == 1 and isinstance(output, str) and 'broker-echo-ok' in output
                    reply = message('synthetic-complete') if trace else call('call_forged_0', *FORGED[0])
                elif step.startswith('forged-'):
                    index = int(step.split('-')[1]); output = output_for(body, 'call_forged_%d' % index)
                    rejected = isinstance(output, str) and output.startswith('unsupported call: ' + FORGED[index][0])
                    evidence['rejections'].append({'tool': FORGED[index][0], 'rejected': rejected, 'output': output[:200] if isinstance(output, str) else output})
                    reply = call('call_forged_%d' % (index + 1), *FORGED[index + 1]) if index + 1 < len(FORGED) else message('synthetic-complete')
                else:
                    bad('unexpected extra request'); reply = message('synthetic-overflow')
            data = sse('resp_' + uuid.uuid4().hex, reply)
            self.send_response(200); self.send_header('content-type', 'text/event-stream'); self.send_header('content-length', str(len(data))); self.end_headers()
            self.wfile.write(data); self.wfile.flush()

    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler); port = server.server_address[1]
    listener = threading.Thread(target=server.serve_forever, daemon=True); listener.start()
    config = profile/'config.toml'; config.write_text(configuration(catalog, port)); config.chmod(0o600)
    source = (REPO/'crates/xcb-runtime/src/sandbox.rs').read_text()
    function = source.split('pub fn codex_seatbelt(', 1)[1].split('\n/// Devin', 1)[0]
    policy = re.search(r'r#"(.*?)"#', function, re.S).group(1)
    assert policy.count('(remote tcp "*:443")') == 1
    policy = policy.replace('(remote tcp "*:443")', f'(remote tcp "localhost:{port}")')
    for name, path in {'exe': exe, 'work': scratch, 'profile': profile, 'config': config, 'catalog': catalog, 'ca_bundle': ca_bundle}.items():
        policy = policy.replace('{' + name + '}', json.dumps(str(path)))
    assert not re.search(r'\{(?:exe|work|profile|config|catalog|ca_bundle)\}', policy)
    (case/'inventory.sb').write_text(policy)
    env = {'HOME': str(home), 'CODEX_HOME': str(profile), 'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'LANG': 'en_US.UTF-8', 'NO_COLOR': '1',
           'XDG_CONFIG_HOME': str(home/'.config'), 'XDG_DATA_HOME': str(home/'.local/share'), 'XDG_CACHE_HOME': str(home/'.cache'),
           'TMPDIR': str(home/'tmp'), 'CODEX_INTERNAL_APP_SERVER_REMOTE_CONTROL_DISABLED': '1', 'SSL_CERT_FILE': str(ca_bundle)}
    p = subprocess.Popen(['/usr/bin/sandbox-exec', '-f', str(case/'inventory.sb'), str(exe), 'app-server', '--strict-config', '--listen', 'stdio://'],
                         cwd=cwd, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
    sel = selectors.DefaultSelector()
    for stream in [p.stdout, p.stderr]: os.set_blocking(stream.fileno(), False); sel.register(stream, selectors.EVENT_READ)
    state = {'buf': b'', 'ids': 0, 'total': 0}; stderr = bytearray(); frames = evidence['frames']; evidence['hostCallbacks'] = 0
    def send(value): p.stdin.write((json.dumps(value, separators=(',', ':')) + '\n').encode()); p.stdin.flush()
    def pump(deadline):
        for key, _ in sel.select(max(0, min(.1, deadline - time.monotonic()))):
            chunk = os.read(key.fileobj.fileno(), 65536)
            if not chunk: sel.unregister(key.fileobj); continue
            state['total'] += len(chunk)
            if state['total'] > 16 * 1024 * 1024: raise RuntimeError('output bound')
            if key.fileobj is p.stderr: stderr.extend(chunk); continue
            state['buf'] += chunk
            while b'\n' in state['buf']:
                line, state['buf'] = state['buf'].split(b'\n', 1); frame = json.loads(line); frames.append(frame)
                if 'method' in frame and 'id' in frame: serve(frame)
    def serve(frame):
        params = frame.get('params') or {}
        if frame['method'] == 'item/tool/call' and params.get('tool') == 'synthetic_echo' and params.get('namespace') is None \
                and params.get('arguments') == {'text': 'broker-echo'} and params.get('callId') == 'call_echo':
            evidence['hostCallbacks'] += 1
            # Byte-identical to xcb's serde_json tool_response for the same result.
            send({'id': frame['id'], 'result': {'success': True, 'contentItems': [{'type': 'inputText', 'text': json.dumps({'text': 'broker-echo-ok'}, separators=(',', ':'))}]}})
        else:
            evidence['violations'].append('provider request ' + frame['method'] + ' ' + json.dumps(params)[:300])
            send({'id': frame['id'], 'error': {'code': -32601, 'message': 'xcb denies native permission requests'}})
    def rpc(method, params):
        state['ids'] += 1; current = state['ids']; send({'id': current, 'method': method, 'params': params}); deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            for frame in frames:
                if frame.get('id') == current and 'method' not in frame:
                    if 'error' in frame: raise RuntimeError(method + ' failed: ' + json.dumps(frame['error'])[:300])
                    return frame['result']
            pump(deadline)
            if p.poll() is not None: raise RuntimeError('native exited before ' + method)
        raise RuntimeError('rpc timeout ' + method)
    def turn(tools):
        started = rpc('thread/start', thread_request(model, effort, cwd, tools)); thread = started['thread']['id']
        if started.get('reasoningEffort') != effort or started.get('model') != model: evidence['violations'].append('thread readback changed model or effort')
        turn_id = rpc('turn/start', {'threadId': thread, 'input': [{'type': 'text', 'text': 'Synthetic no-auth protocol test.', 'text_elements': []}],
                                     'model': model, 'effort': effort})['turn']['id']
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            for frame in frames:
                if frame.get('method') == 'turn/completed' and (frame.get('params') or {}).get('turn', {}).get('id') == turn_id:
                    return frame['params']['turn'].get('status')
            pump(deadline)
            if p.poll() is not None: raise RuntimeError('native exited during turn')
        raise RuntimeError('turn timeout')
    try:
        rpc('initialize', {'clientInfo': {'name': 'xcb', 'version': 'qualification'}, 'capabilities': {'experimentalApi': True, 'requestAttestation': False}})
        send({'method': 'initialized'})
        evidence['emptyTurnStatus'] = 'skipped' if trace else turn([])
        evidence['toolTurnStatus'] = turn([ECHO])
    except Exception as error: evidence['violations'].append('error: ' + str(error))
    finally:
        try: p.stdin.close()
        except BrokenPipeError: pass
        deadline = time.monotonic() + 5
        try:
            while p.poll() is None and time.monotonic() < deadline: pump(deadline)
        except Exception as error: evidence['violations'].append('drain: ' + str(error))
        if p.poll() is None: os.killpg(p.pid, signal.SIGKILL)
        p.wait(timeout=5)
        deadline = time.monotonic() + 3
        while sel.get_map() and time.monotonic() < deadline: pump(deadline)
        try: os.killpg(p.pid, 0); absent = False
        except ProcessLookupError: absent = True
        server.shutdown(); server.server_close(); listener.join(5)
    forged_effects = [name for name in ['forged-exec', 'forged-patch.txt'] if (cwd/name).exists() or (scratch/name).exists()]
    if forged_effects: evidence['violations'].append('forged effects present: ' + ','.join(forged_effects))
    evidence.update(rootExitCode=p.returncode, stdioJoined=not sel.get_map() and not state['buf'], processGroupAbsent=absent,
                    listenerJoined=not listener.is_alive(), binaryUnchanged=sha(exe.read_bytes()) == EXPECTED, stderr=stderr.decode('utf8', 'replace')[-4000:])
    record = json.dumps(evidence, indent=1, sort_keys=True).encode() + b'\n'
    (case/'evidence.json').write_bytes(record)
    rejections = sum(1 for row in evidence['rejections'] if row['rejected'])
    passed = (not evidence['violations'] and (trace or evidence.get('emptyManifestVerified') is True) and evidence.get('exactDynamicManifestVerified') is True
              and evidence.get('permittedCallbackVerified') is True and rejections == (0 if trace else len(FORGED))
              and evidence['emptyTurnStatus'] == ('skipped' if trace else 'completed')
              and evidence['toolTurnStatus'] == 'completed' and p.returncode == 0 and evidence['stdioJoined'] and absent
              and evidence['listenerJoined'] and evidence['binaryUnchanged'])
    return passed, {'model': model, 'reasoningEffort': effort, 'wireReasoningEffort': wire_effort(model, effort), 'requests': len(evidence['requests']),
                    'emptyManifestVerified': evidence.get('emptyManifestVerified') is True, 'exactDynamicManifestVerified': evidence.get('exactDynamicManifestVerified') is True,
                    'permittedCallbackVerified': evidence.get('permittedCallbackVerified') is True, 'forgedBuiltinRejections': rejections,
                    'rootExitCode': p.returncode, 'stdioJoined': evidence['stdioJoined'], 'listenerJoined': evidence['listenerJoined'],
                    'evidenceSha256': sha(record)}, evidence

cases, failures = [], []
for model in QUALIFIED:
    rows = [row for row in catalog_rows['models'] if row['slug'] == model]
    assert len(rows) == 1, 'qualified model missing from the bundled catalog: ' + model
    for effort in [level['effort'] for level in rows[0]['supported_reasoning_levels']]:
        passed, summary, evidence = run_case(model, effort)
        cases.append(summary)
        if not passed: failures.append({'case': summary, 'violations': evidence['violations'][:10], 'rejections': [r for r in evidence['rejections'] if not r['rejected']]})
        print(json.dumps({'passed': passed, **summary}), flush=True)
inventory = {
    'schema': 'xcb.codex-scripted-inventory.v1', 'observedDate': datetime.date.today().isoformat(), 'platform': 'macos-arm64',
    'version': VERSION, 'binarySha256': EXPECTED, 'harnessSha256': sha(pathlib.Path(__file__).read_bytes()),
    'candidate': VERSION != BAKED_VERSION or EXPECTED != BAKED_SHA256, 'bakedVersion': BAKED_VERSION, 'bakedBinarySha256': BAKED_SHA256,
    'schemaSha256': SCHEMA, 'schemaCommand': 'codex ' + ' '.join(SCHEMA_COMMAND) + ' DIR',
    'schemaDigestAlgorithm': 'sha256(sorted relative schema path + NUL + raw generated JSON bytes + NUL)',
    'authentication': 'none; fresh empty CODEX_HOME',
    'egress': 'owned loopback TCP port only; OS denies external network and process forks',
    'productionQualified': False, 'readWriteConfinementQualified': False, 'realProviderInference': False,
    'scope': 'exact native tool manifest serialization and callback rejection, not authenticated provider egress',
    'models': QUALIFIED, 'catalogSource': 'exact checked executable embedded ModelsResponse JSON', 'catalogControls': CONTROLS,
    'wire': 'Responses Lite additional_tools prefix', 'qualificationOverrides': OVERRIDES, 'forgedBuiltins': [name for name, _ in FORGED],
    'cases': cases,
    'limitations': ['No real credentials or existing sessions were accessed.',
                    'This fixture does not establish provider authentication, refresh persistence, TLS egress, or complete filesystem confinement.',
                    'Untested Codex builds and model slugs remain disabled.'],
}
target = args.inventory or ROOT/'inventory.json'
target.write_text(json.dumps(inventory, indent=2) + '\n')
if args.wire_trace:
    passed, _, evidence = run_case(QUALIFIED[0], 'low', trace=True)
    if not passed: failures.append({'case': 'wire trace', 'violations': evidence['violations'][:10]})
    frames = evidence['frames']
    start = max(i for i, f in enumerate(frames) if isinstance(f.get('result'), dict) and 'turn' in f['result'])
    end = next(i for i, f in enumerate(frames) if i > start and f.get('method') == 'turn/completed')
    thread, turn = frames[end]['params']['threadId'], frames[end]['params']['turn']['id']
    messages = sorted({item['id'] for f in frames[start:end + 1] for item in [(f.get('params') or {}).get('item') or {}] if item.get('type') == 'agentMessage'})
    text = json.dumps(frames[start:end + 1]).replace(thread, 'thread1').replace(turn, 'turn1').replace('"call_echo"', '"call1"')
    for index, identifier in enumerate(messages): text = text.replace(identifier, 'msg_%d' % (index + 1))
    # The Rust codec declares the broker tools; the synthetic echo maps onto workspace_read.
    text = text.replace('"synthetic_echo"', '"workspace_read"').replace('{"text": "broker-echo"}', '{"path": "note.txt"}')
    trace_frames = json.loads(text); trace_frames[0]['id'] = 7
    args.wire_trace.write_text(json.dumps(trace_frames, indent=2) + '\n')
print(json.dumps({'inventory': str(target), 'cases': len(cases), 'failures': failures}, indent=1))
raise SystemExit(bool(failures) or not cases)
