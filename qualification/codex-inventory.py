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
probe_flags = parser.add_mutually_exclusive_group()
probe_flags.add_argument('--delegation-probe', action='store_true', help='run the separate offline experimental delegation probe; never qualifies production delegation')
probe_flags.add_argument('--root-trace-probe', action='store_true', help='record one root broker callback and current config readback without running the complete inventory')
parser.add_argument('--delegation-fork-turns', choices=('none', 'all'), default='none', help='synthetic delegation fork mode (used only with --delegation-probe)')
parser.add_argument('--delegation-scenario', choices=('concurrency', 'depth'), default='concurrency', help='test concurrency with two live children or depth with spare thread slots')
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
VERDICT = ROOT/'verdict.json'; VERDICT.unlink(missing_ok=True)
def verdict(outcome, **detail):
    # One machine-readable result beside the evidence: `passed`, `failed` (the
    # inventory ran and a case failed) or `incompatible` (only an xcb release can
    # adopt this build). No verdict file means the run itself broke.
    VERDICT.write_text(json.dumps({'schema': 'xcb.codex-inventory-verdict.v1', 'outcome': outcome, 'version': VERSION,
                                   'binarySha256': EXPECTED, **detail}, indent=2, sort_keys=True) + '\n')
def incompatible(reason, message, **detail):
    verdict('incompatible', reason=reason, **detail)
    print(message, file=sys.stderr)
    raise SystemExit(3)
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
DELEGATION_LIMITS = {'enabled': True, 'max_threads': 4 if args.delegation_scenario == 'depth' else 2, 'max_depth': 1}

with BIN.open('rb') as f: binary = f.read(512 * 1024 * 1024 + 1)
if len(binary) > 512 * 1024 * 1024 or sha(binary) != EXPECTED: raise SystemExit('Provider is not the admitted executable')
marker = b'{\n  "models":'
if binary.count(marker) != 1: incompatible('bundled-catalog', 'Ambiguous bundled catalog')
catalog_rows, _ = json.JSONDecoder().raw_decode(binary[binary.index(marker):][:4 * 1024 * 1024].decode('utf-8', 'replace'))
del binary
version_output = subprocess.run([str(BIN), '--version'], capture_output=True, text=True, timeout=30, env={'PATH': '/usr/bin:/bin'}).stdout.strip()
if version_output != 'codex-cli ' + VERSION: raise SystemExit('Provider version is not the admitted version: ' + version_output)
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
# A changed wire protocol is never admitted through the catalog; it ships in an xcb release.
reviewed = [(BAKED_VERSION, BAKED_SHA256, const('SCHEMA_SHA256'))]
reviewed_literals = strings(config_source.split('pub const REVIEWED_BUILDS: &[(&str, &str, &str)] = &[', 1)[1].split('];', 1)[0])
assert len(reviewed_literals) % 3 == 0, 'invalid reviewed Codex schema bindings'
reviewed += list(zip(*[iter(reviewed_literals)] * 3))
expected_schema = next((schema for version, digest, schema in reviewed if (version, digest) == (VERSION, EXPECTED)), const('SCHEMA_SHA256'))
if SCHEMA != expected_schema:
    incompatible('schema-drift', 'app-server schema digest differs from config.rs SCHEMA_SHA256: observed ' + SCHEMA,
                 observedSchemaSha256=SCHEMA, expectedSchemaSha256=expected_schema, bakedSchemaSha256=const('SCHEMA_SHA256'), bakedVersion=BAKED_VERSION)

def configuration(catalog, port, delegation=False):
    body = config_source.split('pub fn configuration(', 1)[1].split('\npub fn thread_configuration(', 1)[0]
    blocks = re.findall(r'lines\.extend\(\s*\[(.*?)\]\s*\.map\(str::to_owned\)', body, re.S)
    assert len(blocks) == 2
    first, last = [strings(block) for block in blocks]
    assert first.count('model_provider = "openai"') == 1 and first[-1] == '[features]'
    first = ['model_provider = "qualification"' if line == 'model_provider = "openai"' else line for line in first]
    if delegation:
        first = ['approval_policy = "on-request"' if line == 'approval_policy = "never"' else line for line in first]
        if 'approvals_reviewer = "auto_review"' not in first: first.insert(0, 'approvals_reviewer = "auto_review"')
    lines = ['model_catalog_json = ' + json.dumps(str(catalog))] + first + [json.dumps(name) + ' = ' + ('true' if delegation and name in ('multi_agent', 'multi_agent_v2') else 'false') for name in ACCOUNT_FEATURES]
    lines += ['enable_request_compression = false'] + last
    if delegation: lines += ['[agents]', 'enabled = true', 'max_threads = ' + str(DELEGATION_LIMITS['max_threads']), 'max_depth = 1']
    return '\n'.join(lines + ['[model_providers.qualification]', 'name = "qualification"', f'base_url = "http://127.0.0.1:{port}/v1"',
                              'wire_api = "responses"', 'requires_openai_auth = false', 'supports_websockets = false', ''])

def thread_configuration(effort, delegation=False):
    body = config_source.split('pub fn thread_configuration(', 1)[1].split('\npub(crate) fn validate_config(', 1)[0]
    template = re.search(r'json!\((\{"features":features,.*?\})\);', body).group(1).replace('"features":features', '"features":null')
    value = json.loads(template)
    value['features'] = {name: False for name in ACCOUNT_FEATURES + EXTRA_FEATURES}
    value['model_reasoning_effort'] = effort
    if delegation:
        value['features'].update(multi_agent=True, multi_agent_v2=True)
        value['agents'] = dict(DELEGATION_LIMITS)
        value['approvals_reviewer'] = 'auto_review'
    return value

def thread_request(model, effort, cwd, tools, delegation=False):
    # Mirrors crates/xcb-runtime/src/codex.rs thread_request; only modelProvider names the loopback provider.
    body = (REPO/'crates/xcb-runtime/src/codex.rs').read_text().split('fn thread_request(', 1)[1].split('\n    fn thread_readback(', 1)[0]
    keys = re.findall(r'"([A-Za-z]+)":', re.search(r'json!\((\{"model".*?\})\)\n', body).group(1))
    policy = re.search(r'"approvalPolicy":"([^"]+)"', body).group(1)
    request = {'model': model, 'modelProvider': 'qualification', 'config': thread_configuration(effort, delegation), 'cwd': str(cwd), 'approvalPolicy': policy,
               'sandbox': 'read-only', 'ephemeral': True, 'environments': [], 'runtimeWorkspaceRoots': [], 'selectedCapabilityRoots': [],
               'dynamicTools': tools, 'baseInstructions': 'Synthetic base instructions.', 'developerInstructions': 'Synthetic developer instructions.',
               'allowProviderModelFallback': False}
    assert keys == list(request), 'xcb thread request shape changed: ' + ','.join(keys)
    if delegation: request['approvalPolicy'] = 'on-request'
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

def run_case(model, effort, trace=False, delegation=False):
    case = ROOT/('case-' + uuid.uuid4().hex); scratch = case/'scratch'; profile = scratch/'profile'; home = scratch/'home'; cwd = scratch/'work'
    for path in [case, scratch, profile, home, home/'tmp', cwd]: path.mkdir(mode=0o700)
    exe = case/'provider'; subprocess.run(['/bin/cp', '-c', str(BIN), str(exe)], check=True); exe.chmod(0o500)
    ca_bundle = case/'public-ca.pem'; ca_bundle.write_bytes(pathlib.Path('/private/etc/ssl/cert.pem').read_bytes()); ca_bundle.chmod(0o600)
    catalog = case/'models.json'
    rows = [dict(row, **CONTROLS) for row in catalog_rows['models'] if row['slug'] in QUALIFIED]
    if delegation:
        for row in rows: row['multi_agent_version'] = 'v2'
    catalog.write_text(json.dumps({'models': rows}, separators=(',', ':'), ensure_ascii=False, sort_keys=True)); catalog.chmod(0o600)
    evidence = {'model': model, 'reasoningEffort': effort, 'requests': [], 'frames': [], 'violations': [], 'rejections': []}
    lock = threading.Lock(); script = {'step': 0}
    delegation_steps = {}
    children_release = threading.Event()
    delegation_forged = [(name, arguments) for name, arguments in FORGED if name != 'spawn_agent']
    spawn_names = ['child_one'] if args.delegation_scenario == 'depth' else ['child_one', 'child_two', 'overflow']
    expected_children = 1 if args.delegation_scenario == 'depth' else 2
    def collaboration(call_id, name, arguments): return dict(call(call_id, name, arguments), namespace='collaboration')
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
            if size < 1 or size > 8 * 1024 * 1024:
                evidence['violations'].append('request body bound'); self.send_error(413); return
            body = json.loads(self.rfile.read(min(size, 8 * 1024 * 1024)))
            role = 'root'
            if delegation:
                recipients = [item.get('recipient') for item in body.get('input', []) if item.get('type') == 'agent_message' and item.get('recipient') != '/root']
                if recipients: role = recipients[-1].rsplit('/', 1)[-1]
                if role != 'root' and not children_release.wait(15): evidence['violations'].append('child release timeout')
            with lock:
                step = steps[script['step']] if script['step'] < len(steps) else 'overflow'; script['step'] += 1
                if delegation:
                    count = delegation_steps.get(role, 0); delegation_steps[role] = count + 1
                    step = role + '-' + str(count)
                evidence['requests'].append({'step': step, 'path': self.path, 'model': body.get('model'), 'reasoning': body.get('reasoning'),
                                             'manifest': manifest(body)[0], 'topLevelTools': body.get('tools'),
                                             'functionOutputs': [item for item in body.get('input', []) if item.get('type') == 'function_call_output'],
                                             **({'input': body.get('input'), 'requestKey': body.get('prompt_cache_key'), 'bodyKeys': sorted(body)} if delegation else {})})
                bad = lambda why: evidence['violations'].append(step + ': ' + why)
                if self.path != '/v1/responses': bad('path ' + self.path)
                if body.get('model') != model or (body.get('reasoning') or {}).get('effort') != wire_effort(model, effort): bad('model or wire effort changed')
                tools, top = manifest(body)
                if top not in (None, []): bad('top-level tools present')
                if delegation:
                    if script['step'] > 64:
                        bad('delegation request bound'); children_release.set(); reply = message('synthetic-bound-stop')
                    elif role not in ('root', 'child_one', 'child_two'):
                        bad('unexpected delegated role ' + role); reply = message('synthetic-unexpected-child-stop')
                    elif count == 0: reply = call('shared_echo', 'synthetic_echo', {'text': 'broker-' + role})
                    elif count <= len(delegation_forged): reply = call('forged_' + str(count - 1), *delegation_forged[count - 1])
                    elif role == 'root':
                        index = count - len(delegation_forged) - 1
                        if index < len(spawn_names):
                            name = spawn_names[index]
                            reply = collaboration('spawn_' + name, 'spawn_agent', {'task_name': name, 'message': 'Synthetic delegation child.', 'fork_turns': args.delegation_fork_turns})
                        elif index == len(spawn_names):
                            children_release.set(); reply = collaboration('list_after_spawn', 'list_agents', {})
                        elif all(isinstance(output_for(body, 'spawn_' + name), str) and output_for(body, 'spawn_' + name).startswith('collab spawn failed:')
                                 for name in spawn_names if name != 'overflow'):
                            reply = message('synthetic-root-spawn-unavailable')
                        elif sum(1 for f in evidence['frames'] if f.get('method') == 'turn/completed' and (f.get('params') or {}).get('threadId') != evidence.get('rootThreadId')) < expected_children:
                            reply = collaboration('wait_' + str(index), 'wait_agent', {'timeout_ms': 10000})
                        else: reply = message('synthetic-root-complete')
                    elif count == len(delegation_forged) + 1 and args.delegation_scenario == 'depth':
                        reply = collaboration('spawn_depth', 'spawn_agent', {'task_name': 'grandchild', 'message': 'Synthetic forbidden grandchild.', 'fork_turns': 'none'})
                    else: reply = message('synthetic-' + role + '-complete')
                elif step == 'empty':
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
    config = profile/'config.toml'; config.write_text(configuration(catalog, port, delegation)); config.chmod(0o600)
    source = (REPO/'crates/xcb-runtime/src/sandbox.rs').read_text()
    function = source.split('pub fn codex_seatbelt(', 1)[1].split('\n/// Devin', 1)[0]
    policy = re.search(r'r#"(.*?)"#', function, re.S).group(1)
    assert policy.count('(remote tcp "*:443")') == 1
    policy = policy.replace('(remote tcp "*:443")', f'(remote tcp "localhost:{port}")')
    for name, path in {'exe': exe, 'work': scratch, 'profile': profile, 'config': config, 'catalog': catalog, 'ca_bundle': ca_bundle}.items():
        policy = policy.replace('{' + name + '}', json.dumps(str(path)))
    # The native CUA proxy lane belongs to the Rust launcher; this inventory
    # always runs the plain broker profile, so the placeholder is empty.
    policy = policy.replace('{native_rules}', '')
    assert not re.search(r'\{(?:exe|work|profile|config|catalog|ca_bundle|native_rules)\}', policy)
    (case/'inventory.sb').write_text(policy)
    env = {'HOME': str(home), 'CODEX_HOME': str(profile), 'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'LANG': 'en_US.UTF-8', 'NO_COLOR': '1',
           'XDG_CONFIG_HOME': str(home/'.config'), 'XDG_DATA_HOME': str(home/'.local/share'), 'XDG_CACHE_HOME': str(home/'.cache'),
           'TMPDIR': str(home/'tmp'), 'CODEX_INTERNAL_APP_SERVER_REMOTE_CONTROL_DISABLED': '1', 'SSL_CERT_FILE': str(ca_bundle)}
    p = subprocess.Popen(['/usr/bin/sandbox-exec', '-f', str(case/'inventory.sb'), str(exe), 'app-server', '--strict-config', '--listen', 'stdio://'],
                         cwd=cwd, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
    sel = selectors.DefaultSelector()
    for stream in [p.stdout, p.stderr]: os.set_blocking(stream.fileno(), False); sel.register(stream, selectors.EVENT_READ)
    state = {'buf': b'', 'ids': 0, 'total': 0}; stderr = bytearray(); frames = evidence['frames']; evidence['hostCallbacks'] = 0
    child_read_ids = {}
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
                if delegation:
                    item = (frame.get('params') or {}).get('item') or {}
                    if frame.get('method') == 'item/started' and item.get('type') == 'subAgentActivity' and item.get('kind') == 'started':
                        state['ids'] += 1; child_read_ids[state['ids']] = item['agentThreadId']
                        send({'id': state['ids'], 'method': 'thread/read', 'params': {'threadId': item['agentThreadId'], 'includeTurns': False}})
                    if frame.get('id') in child_read_ids and 'method' not in frame:
                        evidence.setdefault('childReadbacks', {})[child_read_ids[frame['id']]] = frame.get('result', {'error': frame.get('error')})
                if 'method' in frame and 'id' in frame: serve(frame)
    def serve(frame):
        params = frame.get('params') or {}
        expected_callback = (params.get('arguments') == {'text': 'broker-echo'} and params.get('callId') == 'call_echo') if not delegation else \
            (params.get('arguments') in [{'text': 'broker-' + role} for role in ('root', 'child_one', 'child_two')] and params.get('callId') == 'shared_echo')
        if frame['method'] == 'item/tool/call' and params.get('tool') == 'synthetic_echo' and params.get('namespace') is None and expected_callback:
            evidence['hostCallbacks'] += 1
            # Byte-identical to xcb's serde_json tool_response for the same result.
            echo_text = params['arguments']['text'] + '-ok' if delegation else 'broker-echo-ok'
            send({'id': frame['id'], 'result': {'success': True, 'contentItems': [{'type': 'inputText', 'text': json.dumps({'text': echo_text}, separators=(',', ':'))}]}})
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
        requested = thread_request(model, effort, cwd, tools, delegation)
        started = rpc('thread/start', requested); thread = started['thread']['id']
        evidence['threadStartReadback'] = started
        if delegation: evidence['rootThreadId'] = thread
        if started.get('reasoningEffort') != effort or started.get('model') != model: evidence['violations'].append('thread readback changed model or effort')
        if started.get('approvalPolicy') != requested['approvalPolicy']: evidence['violations'].append('thread approval policy readback changed')
        if started.get('approvalsReviewer') != requested['config'].get('approvals_reviewer'): evidence['violations'].append('thread approvals reviewer readback changed')
        if started.get('sandbox') != {'type': 'readOnly', 'networkAccess': False}: evidence['violations'].append('thread native sandbox readback changed')
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
        rpc('initialize', {'clientInfo': {'name': 'xcb', 'version': 'qualification'}, 'capabilities': {'experimentalApi': True, 'requestAttestation': False, 'explicitGatewayOauth': True}})
        send({'method': 'initialized'})
        if trace: evidence['configReadback'] = rpc('config/read', {'includeLayers': True, 'cwd': str(cwd)})
        evidence['emptyTurnStatus'] = 'skipped' if trace else turn([])
        evidence['toolTurnStatus'] = turn([ECHO])
    except Exception as error: evidence['violations'].append('error: ' + str(error))
    finally:
        children_release.set()
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

def normalize_observation(evidence):
    """Keep wire order and relationships, without machine names or disposable paths."""
    text = json.dumps(evidence, sort_keys=True)
    start = evidence.get('threadStartReadback') or {}
    if start.get('cwd'):
        case = str(pathlib.Path(start['cwd']).parents[1])
        text = text.replace(case + '/models.json', '/synthetic/catalog.json').replace(case, '/synthetic')
    root = evidence.get('rootThreadId') or (start.get('thread') or {}).get('id')
    if root: text = text.replace(root, 'thread_root')
    for identifier, row in (evidence.get('childReadbacks') or {}).items():
        path = ((((row.get('thread') or {}).get('source') or {}).get('subAgent') or {}).get('thread_spawn') or {}).get('agent_path')
        if path: text = text.replace(identifier, 'thread_' + path.rsplit('/', 1)[-1])
    identifiers = {}
    text = re.sub(r'[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}',
                  lambda match: identifiers.setdefault(match[0], 'uuid_' + str(len(identifiers) + 1)), text)
    text = re.sub(r'msg_[0-9a-f]{32}', lambda match: identifiers.setdefault(match[0], 'message_' + str(len(identifiers) + 1)), text)
    text = re.sub(r'http://127\.0\.0\.1:[0-9]+/v1', 'http://127.0.0.1:0/v1', text)
    value = json.loads(text)
    def scrub(item):
        if isinstance(item, dict):
            return {key: (0 if key in ('emittedAtMs', 'startedAtMs', 'completedAtMs', 'createdAt', 'updatedAt', 'recencyAt', 'startedAt', 'completedAt', 'durationMs') and val is not None
                          else 'synthetic-host' if key == 'serverName' else scrub(val)) for key, val in item.items()}
        if isinstance(item, list): return [scrub(row) for row in item]
        if isinstance(item, str):
            item = re.sub(r'\d{4}-\d{2}-\d{2}T[0-9:.]+Z', 'SYNTHETIC_TIME', item)
            item = re.sub(r'<current_date>.*?</current_date>', '<current_date>SYNTHETIC_DATE</current_date>', item)
        return item
    return scrub(value)

def delegation_assessment(evidence):
    roles = ('root', 'child_one') if args.delegation_scenario == 'depth' else ('root', 'child_one', 'child_two')
    requests = {role: [r for r in evidence['requests'] if r['step'].rsplit('-', 1)[0] == role] for role in roles}
    outputs = {role: {item['call_id']: item.get('output') for row in requests[role] for item in row['functionOutputs']} for role in roles}
    collab_names = ['followup_task', 'interrupt_agent', 'list_agents', 'send_message', 'spawn_agent', 'wait_agent']
    def bounded_manifest(row):
        names = row.get('manifest')
        return isinstance(names, list) and all(
            isinstance(ns, dict) and (ns == EXPECTED_MANIFEST[0] if ns.get('name') == 'functions' else
            ns.get('name') == 'collaboration' and [tool.get('name') for tool in ns.get('tools', [])] == collab_names) for ns in names)
    root_id = evidence.get('rootThreadId')
    start = evidence.get('threadStartReadback') or {}
    readbacks = evidence.get('childReadbacks') or {}
    children = {}
    for identifier, row in readbacks.items():
        thread = row.get('thread') or {}; source = (((thread.get('source') or {}).get('subAgent') or {}).get('thread_spawn') or {})
        children[source.get('agent_path', '').rsplit('/', 1)[-1]] = identifier
    inherited_identity = len(children) == len(roles) - 1 and set(children) == set(roles[1:])
    for role, identifier in children.items():
        thread = readbacks[identifier].get('thread') or {}; source = (((thread.get('source') or {}).get('subAgent') or {}).get('thread_spawn') or {})
        inherited_identity &= all(thread.get(key) == expected for key, expected in {
            'id': identifier, 'parentThreadId': root_id, 'sessionId': root_id, 'model': evidence['model'], 'modelProvider': 'qualification',
            'reasoningEffort': evidence['reasoningEffort'], 'cwd': start.get('cwd'), 'ephemeral': True, 'threadSource': 'subagent',
        }.items()) and source.get('depth') == 1 and source.get('parent_thread_id') == root_id
    forged = [(name, arguments) for name, arguments in FORGED if name != 'spawn_agent']
    rejections = {role: [{'tool': name, 'rejected': outputs[role].get('forged_' + str(index)) == 'unsupported call: ' + name}
                         for index, (name, _) in enumerate(forged)] for role in roles}
    completions = [f['params'] for f in evidence['frames'] if f.get('method') == 'turn/completed']
    final_usage = {f['params']['threadId']: f['params']['tokenUsage']['total'] for f in evidence['frames'] if f.get('method') == 'thread/tokenUsage/updated'}
    echo = {role: outputs[role].get('shared_echo') == json.dumps({'text': 'broker-' + role + '-ok'}, separators=(',', ':')) for role in roles}
    controls = {}
    for role in roles[1:]:
        first = requests[role][0] if requests[role] else {}
        instructions = json.dumps([item for item in first.get('input', []) if item.get('role') == 'developer'])
        controls[role] = 'read-only' in instructions and 'auto_review' in instructions and all(
            r.get('model') == evidence['model'] and (r.get('reasoning') or {}).get('effort') == wire_effort(evidence['model'], evidence['reasoningEffort']) for r in requests[role])
    checks = {
        'rootBrokerManifestExact': bool(requests['root']) and all(isinstance(row['manifest'], list) and len(row['manifest']) == 2
                                       and row['manifest'][1:] == EXPECTED_MANIFEST and row['manifest'][0].get('name') == 'collaboration' for row in requests['root']),
        'nativeToolManifestClosed': all(bounded_manifest(row) and row.get('topLevelTools') in (None, []) for row in evidence['requests']),
        'childIdentityInherited': bool(inherited_identity),
        'childVisibleControlsInherited': all(controls.values()) and len(controls) == len(roles) - 1,
        'rootBrokerCallbackVerified': echo['root'],
        'childBrokerManifestInherited': all(requests[role] and all(isinstance(row['manifest'], list) and EXPECTED_MANIFEST[0] in row['manifest'] for row in requests[role]) for role in roles[1:]),
        'childBrokerCallbacksVerified': all(echo[role] for role in roles[1:]),
        'forgedBuiltinRejectionsVerified': all(row['rejected'] for role, rows in rejections.items() if requests[role] for row in rows),
        'rootCompletedAfterChildren': len(completions) == 1 + len(children) and completions[-1]['threadId'] == root_id
                                      and {c['threadId'] for c in completions} == {root_id, *children.values()}
                                      and all(c['turn']['status'] == 'completed' for c in completions),
        'perThreadUsageAccounted': len(final_usage) == 1 + len(children) and sum(row['totalTokens'] for row in final_usage.values()) == 2 * len(evidence['requests']),
        'processAndListenerJoined': evidence.get('rootExitCode') == 0 and all(evidence.get(key) is True for key in ('stdioJoined', 'processGroupAbsent', 'listenerJoined', 'binaryUnchanged')),
    }
    if args.delegation_scenario == 'concurrency': checks['concurrencyLimitVerified'] = outputs['root'].get('spawn_overflow') == 'collab spawn failed: agent thread limit reached'
    else:
        checks['depthLimitVerified'] = all(isinstance(outputs[role].get('spawn_depth'), str) and 'maximum depth' in outputs[role]['spawn_depth'].lower() for role in roles[1:])
    return {'checks': checks, 'blockers': [key for key, passed in checks.items() if not passed], 'forgedRejections': rejections,
            'brokerEchoResults': {role: outputs[role].get('shared_echo') for role in roles},
            'spawnLimitResults': {role: {key: value for key, value in outputs[role].items() if key.startswith('spawn_')} for role in roles},
            'finalUsageByThread': final_usage}

if args.delegation_probe or args.root_trace_probe:
    passed, summary, evidence = run_case(QUALIFIED[0], 'low', trace=True, delegation=args.delegation_probe)
    probe_name = 'delegation' if args.delegation_probe else 'root-trace'
    (ROOT/(probe_name + '-observation.json')).write_text(json.dumps(evidence, indent=2, sort_keys=True) + '\n')
    if args.delegation_probe:
        assessment = delegation_assessment(evidence)
        observed = normalize_observation(evidence)
        # Keep one exact manifest and developer-context snapshot per role; retain
        # every request's tool outputs and every app-server frame in wire order.
        for row in observed['requests']:
            if row['step'].endswith('-0'):
                row['input'] = [item for item in row['input'] if item.get('type') != 'additional_tools']
            else: row.pop('input', None)
        report = {'schema': 'xcb.codex-delegation-observation.v1', 'observedDate': datetime.date.today().isoformat(),
                  'version': VERSION, 'binarySha256': EXPECTED, 'schemaSha256': SCHEMA,
                  'harnessSha256': sha(pathlib.Path(__file__).read_bytes()), 'runtimeConfigSourceSha256': sha(config_source.encode()),
                  'authentication': 'none; fresh empty HOME and CODEX_HOME', 'realProviderInference': False, 'productionQualified': False,
                  'egress': 'one owned loopback endpoint; production Seatbelt otherwise unchanged; process forks denied',
                  'forkTurns': args.delegation_fork_turns, 'scenario': args.delegation_scenario, 'limits': DELEGATION_LIMITS,
                  'qualificationOverrides': {**OVERRIDES, 'features.multi_agent': True, 'features.multi_agent_v2': True,
                                             'multi_agent_version': 'v2', 'approval_policy': 'on-request', 'approvals_reviewer': 'auto_review'},
                  'outcome': 'failed' if evidence['violations'] else 'incompatible' if assessment['blockers'] else 'passed',
                  'delegationAdmitted': False, 'limitations': ['Offline observation only; does not activate production delegation.',
                      'Cancellation and late callbacks require separate qualification before production activation.'],
                  **normalize_observation({**assessment, 'threadStartReadback': evidence.get('threadStartReadback'), 'rootThreadId': evidence.get('rootThreadId'), 'childReadbacks': evidence.get('childReadbacks')}),
                  'evidence': observed}
        target = args.inventory or ROOT/'delegation.json'
        target.write_text(json.dumps(report, indent=2, sort_keys=True) + '\n')
        verdict(report['outcome'], probe='delegation', blockers=assessment['blockers'], violations=evidence['violations'], inventory=target.name, inventorySha256=sha(target.read_bytes()))
        print(json.dumps({'probe': probe_name, 'outcome': report['outcome'], 'checks': assessment['checks'], 'violations': evidence['violations']}))
        raise SystemExit(1 if report['outcome'] == 'failed' else 3 if report['outcome'] == 'incompatible' else 0)
    verdict('passed' if passed else 'failed', probe=probe_name, violations=evidence['violations'])
    print(json.dumps({'probe': probe_name, 'passed': passed, 'summary': summary, 'violations': evidence['violations']}))
    raise SystemExit(not passed)

cases, failures = [], []
for model in QUALIFIED:
    rows = [row for row in catalog_rows['models'] if row['slug'] == model]
    if len(rows) != 1: incompatible('qualified-model', 'qualified model missing from the bundled catalog: ' + model, model=model)
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
verdict('passed' if cases and not failures else 'failed', cases=len(cases), failures=failures,
        inventory=target.name, inventorySha256=sha(target.read_bytes()), schemaSha256=SCHEMA)
print(json.dumps({'inventory': str(target), 'cases': len(cases), 'failures': failures}, indent=1))
raise SystemExit(bool(failures) or not cases)
