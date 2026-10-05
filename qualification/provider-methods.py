import argparse
import hashlib
import json
import pathlib
import re


def codex_inventory(root):
    digest = hashlib.sha256()
    for path in sorted(root.rglob('*.json')):
        digest.update(path.relative_to(root).as_posix().encode() + b'\0' + path.read_bytes() + b'\0')
    groups = {}
    for name in ('ClientRequest', 'ClientNotification', 'ServerRequest', 'ServerNotification'):
        schema = json.loads((root / (name + '.json')).read_text())
        methods = [variant['properties']['method']['enum'][0] for variant in schema['oneOf']]
        if len(methods) != len(set(methods)):
            raise ValueError('duplicate Codex method')
        groups[name] = sorted(methods)
    return {'schemaSha256': digest.hexdigest(), 'methods': groups}


def claude_inventory(path):
    source = path.read_text()
    query = source.split('export declare interface Query extends ', 1)[1].split('\n}', 1)[0]
    query = re.sub(r'/\*.*?\*/', '', query, flags=re.S)
    return {'sdkVersion': json.loads((path.parent / 'package.json').read_text())['version'],
            'Query': sorted(re.findall(r'^    (\w+)\(', query, flags=re.M))}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--codex-schema', required=True, type=pathlib.Path)
    parser.add_argument('--claude-sdk', required=True, type=pathlib.Path)
    parser.add_argument('--manifest', type=pathlib.Path)
    args = parser.parse_args()
    codex = codex_inventory(args.codex_schema)
    claude = claude_inventory(args.claude_sdk)
    if args.manifest:
        manifest = json.loads(args.manifest.read_text())
        if manifest['codex'] != codex or manifest['claude'] != claude:
            raise ValueError('provider method coverage drift; review the protocol before changing the manifest')
        print(json.dumps({'passed': True, 'codexSchemaSha256': codex['schemaSha256'],
                          'codexCounts': {name: len(methods) for name, methods in codex['methods'].items()},
                          'claudeQueryCount': len(claude['Query'])}))
    else:
        print(json.dumps({'codex': codex, 'claude': claude}, indent=2))


if __name__ == '__main__':
    main()
