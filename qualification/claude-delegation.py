#!/usr/bin/env python3
"""Observe a pinned Claude child under the production sandbox and a local endpoint.

Uses an empty disposable home, a synthetic API key, and no provider network.
The receipt is evidence for review, never a production activation record.
"""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import selectors
import signal
import subprocess
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


def digest(data):
    return hashlib.sha256(data).hexdigest()


parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--executable', type=Path, required=True)
parser.add_argument('--expect-version', required=True)
parser.add_argument('--expect-sha256', required=True)
parser.add_argument('--output', type=Path, required=True)
parser.add_argument('--scenario', choices=['delegation', 'root'], default='delegation')
parser.add_argument('--evidence', type=Path, help='normalized root-scenario report')
args = parser.parse_args()
root_only = args.scenario == 'root'
repo = Path(__file__).resolve().parents[1]
harness_source = Path(__file__).read_bytes()
binary = args.executable.resolve(strict=True)
assert digest(binary.read_bytes()) == args.expect_sha256, 'executable digest mismatch'
version = subprocess.run([str(binary), '--version'], capture_output=True, text=True,
                         timeout=20, env={'PATH': '/usr/bin:/bin'}).stdout.strip()
assert version.startswith(args.expect_version + ' '), 'executable version mismatch'
root = args.output.resolve()
root.mkdir(mode=0o700, parents=True, exist_ok=False)
scratch = root / 'scratch'
scratch.mkdir(mode=0o700)
for part in ['home', 'config', 'tmp', 'work']:
    (scratch / part).mkdir(mode=0o700)
outside = root / 'outside-canary.txt'
outside.write_text('XCB_PRIVATE_SYNTHETIC_CANARY')
os.chmod(outside, 0o600)
runner_source = (repo / 'crates/xcb-runtime/src/runner.rs').read_text()
launch = runner_source.split('fn provider_args(', 1)[1].split('\n    let mut args = vec![', 1)[1].split('\n    ];', 1)[0]
launch_args = [json.loads(x) for x in re.findall(r'"(?:[^"\\]|\\.)*"', launch)]
if root_only:
    assert launch_args[launch_args.index('--tools') + 1] == '', 'production native tools changed'
    assert launch_args[launch_args.index('--permission-mode') + 1] == 'auto', 'production permission mode changed'
else:
    launch_args[launch_args.index('--tools') + 1] = 'Agent'
launch_args[launch_args.index('--permission-mode') + 1] = 'auto'
settings = json.loads(re.search(r'args.push\(json!\((\{[^\n]+\})\)\.to_string\(\)\);', runner_source).group(1))
launch_args += ['--model', 'claude-fable-5-1', '--effort', 'max', '--settings', json.dumps(settings),
                '--allowedTools', 'mcp__xcb__synthetic_echo' if root_only else 'Agent(xcb-worker),mcp__xcb__synthetic_echo']
echo = {'name': 'synthetic_echo', 'description': 'Return a synthetic value.',
        'inputSchema': {'type': 'object', 'properties': {'text': {'type': 'string'}},
                        'required': ['text'], 'additionalProperties': False}}
requests, frames, callbacks, permission_requests = [], [], [], []
unsupported_requests, fixture_errors = [], []
lock = threading.Lock()
steps = {'root': 0, 'child': 0}


def response(model, call=None):
    events = [{'type': 'message_start', 'message': {'id': 'msg_fixture', 'type': 'message',
               'role': 'assistant', 'content': [], 'model': model, 'stop_reason': None,
               'stop_sequence': None, 'usage': {'input_tokens': 1, 'output_tokens': 0}}}]
    if call:
        name, data, identity = call
        events += [{'type': 'content_block_start', 'index': 0, 'content_block': {
            'type': 'tool_use', 'id': identity, 'name': name, 'input': {}}},
            {'type': 'content_block_delta', 'index': 0, 'delta': {
                'type': 'input_json_delta', 'partial_json': json.dumps(data)}}]
    else:
        events += [{'type': 'content_block_start', 'index': 0, 'content_block': {'type': 'text', 'text': ''}},
                   {'type': 'content_block_delta', 'index': 0, 'delta': {'type': 'text_delta', 'text': 'FIXTURE_DONE'}}]
    events += [{'type': 'content_block_stop', 'index': 0},
               {'type': 'message_delta', 'delta': {'stop_reason': 'tool_use' if call else 'end_turn',
                'stop_sequence': None}, 'usage': {'output_tokens': 1}}, {'type': 'message_stop'}]
    return ''.join('event: ' + e['type'] + '\ndata: ' + json.dumps(e) + '\n\n' for e in events).encode()


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        if self.path.split('?')[0] != '/v1/messages':
            self.send_error(404)
            return
        size = int(self.headers.get('content-length', '0'))
        if not 0 < size <= 2 * 1024 * 1024:
            self.send_error(413)
            return
        body = json.loads(self.rfile.read(size))
        texts = []
        for message in body.get('messages', []):
            if message.get('role') != 'user':
                continue
            content = message.get('content', [])
            texts += [content] if isinstance(content, str) else [b.get('text', '') for b in content if b.get('type') == 'text']
        kind = 'child' if any(text == 'XCB_CHILD_PROBE' for text in texts) else 'root'
        unsupported = root_only and (kind != 'root' or body.get('model') != 'claude-fable-5-1'
                                     or [t.get('name') for t in body.get('tools', [])] != ['mcp__xcb__synthetic_echo']
                                     or 'XCB_ROOT_PROBE' not in texts)
        with lock:
            step = steps[kind]
            steps[kind] += 1
            requests.append({'kind': kind, 'step': step, 'model': body.get('model'),
                             'tools': [t.get('name') for t in body.get('tools', [])],
                             'userText': texts,
                             'canaryLeaked': 'XCB_PRIVATE_SYNTHETIC_CANARY' in json.dumps(body),
                             **({'toolResults': [block for message in body.get('messages', [])
                                  for block in (message.get('content') if isinstance(message.get('content'), list) else [])
                                  if block.get('type') == 'tool_result']} if root_only else {})})
            if unsupported:
                unsupported_requests.append({'model': body.get('model'), 'tools': requests[-1]['tools'],
                                             'reason': 'The fixture does not implement classifier or other model requests.'})
            if len(requests) > 12:
                self.send_error(429)
                return
        if unsupported:
            self.send_error(400, 'Classifier and other model requests are not implemented by this fixture')
            return
        call = None
        if root_only and step == 0:
            call = ('mcp__xcb__synthetic_echo', {'text': 'root callback'}, 'toolu_echo')
        elif kind == 'root' and step == 0:
            call = ('Agent', {'description': 'Synthetic child', 'prompt': 'XCB_CHILD_PROBE',
                    'subagent_type': 'xcb-worker'}, 'toolu_child')
        elif kind == 'child' and step == 0:
            call = ('mcp__xcb__synthetic_echo', {'text': 'child callback'}, 'toolu_echo')
        payload = response(body.get('model', 'fixture'), call)
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.send_header('Content-Length', str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)


server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
server.daemon_threads = True
listener = threading.Thread(target=server.serve_forever, daemon=True)
listener.start()
port = server.server_address[1]
source = (repo / 'crates/xcb-runtime/src/sandbox.rs').read_text()
function = source.split('pub fn seatbelt(', 1)[1].split('\n}\n', 1)[0]
policy = re.search(r'r#"(.*?)"#', function, re.S).group(1)
policy = policy.replace('{exe}', json.dumps(str(binary))).replace('{work}', json.dumps(str(scratch)))
assert policy.count('(remote tcp "*:443")') == 1
policy = policy.replace('(remote tcp "*:443")', f'(remote tcp "localhost:{port}")')
policy_file = root / 'policy.sb'
policy_file.write_text(policy)
os.chmod(policy_file, 0o600)
env = {'PATH': '/usr/bin:/bin', 'HOME': str(scratch / 'home'), 'CLAUDE_CONFIG_DIR': str(scratch / 'config'),
       'TMPDIR': str(scratch / 'tmp'), 'CLAUDE_CODE_TMPDIR': str(scratch / 'tmp'),
       'LANG': 'en_US.UTF-8', 'NO_COLOR': '1',
       'ANTHROPIC_API_KEY': 'sk-ant-api03-synthetic-local-fixture',
       'ANTHROPIC_BASE_URL': f'http://127.0.0.1:{port}', 'CLAUDE_CODE_DISABLE_AUTO_MEMORY': '1',
       'CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC': '1', 'ENABLE_CLAUDEAI_MCP_SERVERS': 'false'}
process = subprocess.Popen(['/usr/bin/sandbox-exec', '-f', str(policy_file), str(binary), *launch_args],
                           cwd=scratch / 'work', env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                           stderr=subprocess.PIPE, start_new_session=True)


def send(value):
    process.stdin.write(json.dumps(value).encode() + b'\n')
    process.stdin.flush()


initialization = {'type': 'control_request', 'request_id': 'xcb_initialize', 'request': {
    'subtype': 'initialize', 'sdkMcpServers': ['xcb'], 'hooks': {}, 'agents': {'xcb-worker': {
        'description': 'Synthetic broker-only child', 'prompt': 'Complete the synthetic callback.',
        'tools': ['mcp__xcb__synthetic_echo'], 'model': 'inherit', 'maxTurns': 3,
        'permissionMode': 'auto', 'skills': [], 'omitClaudeMd': True}}, 'skills': [], 'plugins': [],
    'systemPrompt': ['Synthetic local qualification.'], 'supportedDialogKinds': []}}
if root_only:
    initialization['request']['agents'] = {}
send(initialization)
selector = selectors.DefaultSelector()
selector.register(process.stdout, selectors.EVENT_READ, 'stdout')
selector.register(process.stderr, selectors.EVENT_READ, 'stderr')
buffers = {'stdout': b'', 'stderr': b''}
result = None
deadline = time.monotonic() + 45
try:
    while time.monotonic() < deadline and result is None:
        for key, _ in selector.select(0.2):
            data = os.read(key.fileobj.fileno(), 65536)
            if not data:
                selector.unregister(key.fileobj)
                continue
            stream = key.data
            buffers[stream] += data
            assert len(buffers[stream]) < 2 * 1024 * 1024, 'output bound'
            if stream != 'stdout':
                continue
            while b'\n' in buffers[stream]:
                line, buffers[stream] = buffers[stream].split(b'\n', 1)
                frame = json.loads(line)
                frames.append(frame)
                assert len(frames) <= 512, 'frame bound'
                if frame.get('type') == 'control_response' and frame.get('response', {}).get('request_id') == 'xcb_initialize':
                    assert frame['response']['subtype'] == 'success', 'initialization failed'
                    send({'type': 'user', 'session_id': '', 'parent_tool_use_id': None,
                          'message': {'role': 'user', 'content': 'XCB_ROOT_PROBE'}})
                elif frame.get('type') == 'control_request':
                    request = frame['request']
                    if request['subtype'] == 'mcp_message':
                        message = request['message']
                        method = message['method']
                        if method == 'initialize':
                            reply = {'protocolVersion': message['params']['protocolVersion'], 'capabilities': {'tools': {}},
                                     'serverInfo': {'name': 'xcb', 'version': '0.0.0-fixture'}}
                        elif method == 'tools/list':
                            reply = {'tools': [echo]}
                        elif method == 'notifications/initialized':
                            reply = {}
                        elif method == 'tools/call':
                            assert message['params']['name'] == 'synthetic_echo'
                            assert message['params']['arguments'] == {'text': 'root callback' if root_only else 'child callback'}
                            if root_only:
                                assert request.get('server_name') == 'xcb', 'unexpected SDK broker'
                                assert not callbacks, 'duplicate broker callback'
                            callbacks.append(message['params'])
                            reply = {'content': [{'type': 'text', 'text': 'SYNTHETIC_ECHO'}]}
                        else:
                            raise AssertionError('unexpected MCP method')
                        response_body = {'mcp_response': {'jsonrpc': '2.0', 'id': message.get('id'), 'result': reply}}
                    elif request['subtype'] == 'can_use_tool':
                        permission_requests.append(request)
                        response_body = {'behavior': 'deny', 'message': 'Fixture does not override permission decisions.'}
                    else:
                        raise AssertionError('unexpected control request')
                    send({'type': 'control_response', 'response': {'subtype': 'success',
                         'request_id': frame['request_id'], 'response': response_body}})
                elif frame.get('type') == 'result':
                    result = frame
        if process.poll() is not None and not selector.get_map():
            break
except Exception as error:
    if not root_only:
        raise
    fixture_errors.append(type(error).__name__ + ': ' + str(error))
finally:
    def drain_root(deadline):
        while selector.get_map() and time.monotonic() < deadline:
            for key, _ in selector.select(0.1):
                data = os.read(key.fileobj.fileno(), 65536)
                if not data:
                    selector.unregister(key.fileobj)
                    continue
                buffers[key.data] += data
                if len(buffers[key.data]) > 2 * 1024 * 1024:
                    fixture_errors.append('cleanup output bound')
                    return
                if key.data == 'stdout':
                    while b'\n' in buffers['stdout']:
                        line, buffers['stdout'] = buffers['stdout'].split(b'\n', 1)
                        frames.append(json.loads(line))
    if root_only:
        try:
            process.stdin.close()
        except BrokenPipeError:
            pass
        drain_root(time.monotonic() + 3)
    if process.poll() is None:
        os.killpg(process.pid, signal.SIGTERM)
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait(timeout=5)
    if root_only:
        drain_root(time.monotonic() + 2)
    stdio_joined = not selector.get_map() and not buffers['stdout']
    try:
        os.killpg(process.pid, 0)
        group_absent = False
    except ProcessLookupError:
        group_absent = True
    selector.close()
    server.shutdown()
    server.server_close()
    listener.join(5)
    record = {'schema': 'xcb.claude-delegation-observation.v1', 'version': args.expect_version,
              'binarySha256': args.expect_sha256, 'runnerSourceSha256': digest(runner_source.encode()),
              'policySourceSha256': digest(source.encode()), 'realCredentialsUsed': False,
              'paidModelRequests': 0, 'productionQualificationIssued': False,
              'requests': requests, 'frames': frames, 'callbacks': callbacks,
              'permissionRequests': permission_requests, 'result': result,
              'stderr': buffers['stderr'].decode(errors='replace')[-8000:],
              'processExit': process.returncode, 'canaryUnchanged': outside.read_text() == 'XCB_PRIVATE_SYNTHETIC_CANARY'}
    if root_only:
        record.update(scenario='root', launchArgs=launch_args, initialization=initialization,
                      unsupportedRequests=unsupported_requests, fixtureErrors=fixture_errors,
                      stdioJoined=stdio_joined, processGroupAbsent=group_absent,
                      listenerJoined=not listener.is_alive(), binaryUnchanged=digest(binary.read_bytes()) == args.expect_sha256,
                      sourceUnchanged=runner_source == (repo/'crates/xcb-runtime/src/runner.rs').read_text()
                      and source == (repo/'crates/xcb-runtime/src/sandbox.rs').read_text()
                      and harness_source == Path(__file__).read_bytes())
    (root / 'observation.json').write_text(json.dumps(record, indent=2) + '\n')
    print(json.dumps({'evidence': str(root / 'observation.json'), 'requests': len(requests),
                      'children': steps['child'], 'callbacks': len(callbacks), 'result': result is not None,
                      'permissionRequests': len(permission_requests)}))
if root_only:
    init = [frame for frame in frames if frame.get('type') == 'system' and frame.get('subtype') == 'init']
    expected_callback = {'name': 'synthetic_echo', 'arguments': {'text': 'root callback'}}
    tool_results = [block for request in requests for block in request['toolResults']]
    terminal = result or {}
    checks = {
        'autoModeReadback': len(init) == 1 and init[0].get('permissionMode') == 'auto',
        'productionNativeToolsEmpty': launch_args[launch_args.index('--tools') + 1] == '',
        'brokerManifestExact': len(init) == 1 and init[0].get('tools') == ['mcp__xcb__synthetic_echo']
            and len(requests) == 2 and all(request['tools'] == ['mcp__xcb__synthetic_echo'] for request in requests),
        # MCP metadata carries the tool-use ID and progress token separately
        # from the broker call's name and arguments.
        'oneRootBrokerCallback': [{key: call.get(key) for key in ('name', 'arguments')}
                                 for call in callbacks] == [expected_callback] and steps['child'] == 0,
        'brokerResultObserved': len(tool_results) == 1 and tool_results[0].get('tool_use_id') == 'toolu_echo'
            and not tool_results[0].get('is_error', False) and 'SYNTHETIC_ECHO' in json.dumps(tool_results[0].get('content')),
        'terminalSuccess': terminal.get('subtype') == 'success' and terminal.get('is_error') is False
            and terminal.get('result') == 'FIXTURE_DONE',
        'noPermissionDecisionSimulated': not permission_requests and not terminal.get('permission_denials') and not unsupported_requests,
        'canaryUnchanged': record['canaryUnchanged'],
        'canaryNotLeaked': not any(request['canaryLeaked'] for request in requests)
            and 'XCB_PRIVATE_SYNTHETIC_CANARY' not in json.dumps([frames, record['stderr']]),
        'processAndListenerJoined': process.returncode == 0 and stdio_joined and group_absent and not listener.is_alive(),
        'binaryAndSourcesUnchanged': record['binaryUnchanged'] and record['sourceUnchanged'],
        'noFixtureErrors': not fixture_errors,
    }
    normalized = json.dumps(record, sort_keys=True).replace(str(root), '/synthetic').replace(str(binary), '/synthetic/claude')
    identifiers = {}
    normalized = re.sub(r'[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}',
                        lambda match: identifiers.setdefault(match[0], 'uuid_' + str(len(identifiers) + 1)), normalized)
    normalized = re.sub(r'http://127\.0\.0\.1:[0-9]+', 'http://127.0.0.1:0', normalized)
    report = {'schema': 'xcb.claude-auto-root-observation.v1', 'observedDate': datetime.date.today().isoformat(),
              'version': args.expect_version, 'binarySha256': args.expect_sha256,
              'harnessSha256': digest(harness_source), 'runnerSourceSha256': digest(runner_source.encode()),
              'policySourceSha256': digest(source.encode()), 'rawObservationSha256': digest((root/'observation.json').read_bytes()),
              'realCredentialsUsed': False, 'realProviderInference': False, 'realClassifierDecisionObserved': False,
              'productionQualificationIssued': False, 'nativeDelegationEnabled': False,
              'egress': 'Production Seatbelt narrowed to one owned loopback endpoint; synthetic API key and empty profile.',
              'scope': 'Root Auto mode and one explicitly allowed synthetic SDK broker callback; no native tools or agents.',
              'limitations': ['Does not establish real model inference, authentication, or Auto classifier decisions.']
                  + (['The CLI requested an unsupported classifier or other model response; no approval was simulated.'] if unsupported_requests else []),
              'passed': all(checks.values()), 'checks': checks, 'evidence': json.loads(normalized)}
    target = args.evidence or root/'auto-root.json'
    target.write_text(json.dumps(report, indent=2, sort_keys=True) + '\n')
    print(json.dumps({'scenario': 'root', 'passed': report['passed'], 'checks': checks, 'evidence': str(target)}))
    raise SystemExit(not report['passed'])
if result is None:
    raise SystemExit(1)
