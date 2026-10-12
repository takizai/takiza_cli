"""Deterministic MCP/LSP/OAuth fixtures; no external services or credentials."""
import argparse
import json
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

parser = argparse.ArgumentParser()
parser.add_argument('mode', choices=['mcp', 'lsp', 'http'])
parser.add_argument('--log')
parser.add_argument('--pull-diagnostics', choices=['retry', 'always', 'no-retry', 'error'])
args = parser.parse_args()
documents = {}
calls = 0

def log(message):
    if args.log:
        with open(args.log, 'a') as file:
            file.write(json.dumps(message) + '\n')

def reply(message, result):
    return {'jsonrpc': '2.0', 'id': message['id'], 'result': result}

def handle(message):
    global calls
    log(message)
    method = message.get('method', '')
    params = message.get('params', {})
    if args.mode == 'lsp':
        if method == 'initialize':
            capabilities = {'positionEncoding': 'utf-16', 'textDocumentSync': 2,
                'hoverProvider': True, 'definitionProvider': True, 'referencesProvider': True,
                'documentSymbolProvider': True, 'workspaceSymbolProvider': True}
            if args.pull_diagnostics:
                capabilities['diagnosticProvider'] = {'interFileDependencies': True, 'workspaceDiagnostics': False}
            return reply(message, {'capabilities': capabilities})
        if method == 'textDocument/diagnostic':
            calls += 1
            if args.pull_diagnostics == 'retry' and calls > 2:
                return reply(message, {'kind': 'full', 'items': []})
            return {'jsonrpc': '2.0', 'id': message['id'], 'error': {
                'code': -32603 if args.pull_diagnostics == 'error' else -32802,
                'message': 'server cancelled the request',
                'data': {'retriggerRequest': args.pull_diagnostics != 'no-retry'}}}
        if method == 'shutdown':
            return reply(message, None)
        if method == 'exit':
            sys.exit(0)
        if method in ('textDocument/didOpen', 'textDocument/didChange'):
            doc = params['textDocument']
            uri = doc['uri']
            text = doc.get('text', params.get('contentChanges', [{}])[0].get('text', ''))
            documents[uri] = (text, doc['version'])
            send({'jsonrpc': '2.0', 'method': 'textDocument/publishDiagnostics', 'params': {
                'uri': uri, 'version': doc['version'], 'diagnostics': [] if 'broken' not in text else [{
                    'range': {'start': {'line': 0, 'character': 0}, 'end': {'line': 0, 'character': 1}},
                    'severity': 1, 'message': 'fixture error'}]}})
            return
        if method == 'textDocument/didClose':
            documents.pop(params['textDocument']['uri'], None)
            return
        if method == 'textDocument/hover':
            text, version = documents[params['textDocument']['uri']]
            return reply(message, {'contents': {'kind': 'plaintext', 'value': f'{version}:{text}'},
                'range': {'start': params['position'], 'end': params['position']}})
        if method in ('textDocument/definition', 'textDocument/references'):
            return reply(message, [{'uri': params['textDocument']['uri'], 'range': {
                'start': params['position'], 'end': params['position']}}])
        if method == 'textDocument/documentSymbol':
            return reply(message, [{'name': 'example', 'kind': 12, 'range': {
                'start': {'line': 0, 'character': 0}, 'end': {'line': 0, 'character': 1}}}])
        if method == 'workspace/symbol':
            return reply(message, [])
    else:
        if method == 'initialize':
            return reply(message, {'protocolVersion': params['protocolVersion'],
                'capabilities': {'tools': {'listChanged': True}}, 'serverInfo': {'name': 'fixture', 'version': '1'}})
        if method == 'tools/list':
            page = params.get('cursor')
            result = {'tools': [{'name': 'echo' if not page else 'extra', 'description': 'fixture echo',
                'inputSchema': {'type': 'object', 'properties': {'text': {'type': 'string'}}, 'required': ['text']}}]}
            if not page:
                result['nextCursor'] = 'second'
            return reply(message, result)
        if method == 'tools/call':
            calls += 1
            text = params['arguments']['text']
            if text == 'disconnect':
                sys.exit(0)
            if text == 'wait' and args.mode == 'mcp':
                import threading
                threading.Timer(2, lambda: send(reply(message, {'content': [{'type': 'text', 'text': 'delayed'}]}))).start()
                return None
            if text == 'wait':
                time.sleep(2)
            return reply(message, {'content': [{'type': 'text', 'text': text}],
                'structuredContent': {'calls': calls}, 'isError': text == 'error'})
    return None

def send(message):
    body = json.dumps(message).encode()
    if args.mode == 'mcp':
        sys.stdout.buffer.write(body + b'\n')
    else:
        sys.stdout.buffer.write(f'Content-Length: {len(body)}\r\n\r\n'.encode() + body)
    sys.stdout.buffer.flush()

if args.mode == 'http':
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass
        def write_json(self, status, body):
            body = json.dumps(body).encode()
            self.send_response(status)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        def do_GET(self):
            issuer = f'http://127.0.0.1:{self.server.server_port}'
            if self.path.startswith('/.well-known/oauth-protected-resource'):
                self.write_json(200, {'resource': issuer + '/mcp', 'authorization_servers': [issuer], 'scopes_supported': ['tools']})
            elif self.path.startswith('/.well-known/'):
                self.write_json(200, {'issuer': issuer, 'authorization_endpoint': issuer + '/authorize',
                    'token_endpoint': issuer + '/token', 'registration_endpoint': issuer + '/register',
                    'response_types_supported': ['code'], 'code_challenge_methods_supported': ['S256'],
                    'grant_types_supported': ['authorization_code', 'refresh_token'], 'scopes_supported': ['tools']})
            else:
                self.send_response(405)
                self.end_headers()
        def do_POST(self):
            body = self.rfile.read(int(self.headers.get('Content-Length', 0)))
            if self.path == '/register':
                self.write_json(201, {'client_id': 'fixture-client', 'redirect_uris': json.loads(body)['redirect_uris']})
            elif self.path == '/token':
                log({'tokenRequest': body.decode()})
                self.write_json(200, {'access_token': 'fixture-access', 'token_type': 'Bearer',
                    'refresh_token': 'fixture-refresh', 'expires_in': 3600})
            else:
                message = json.loads(body)
                response = handle(message)
                if response:
                    self.write_json(200, response)
                else:
                    self.send_response(202)
                    self.send_header('Content-Length', '0')
                    self.end_headers()
        def do_DELETE(self):
            self.send_response(200)
            self.end_headers()
    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    print(server.server_port, flush=True)
    server.serve_forever()
else:
    while True:
        if args.mode == 'mcp':
            line = sys.stdin.buffer.readline()
            if not line:
                break
            message = json.loads(line)
        else:
            headers = {}
            while True:
                line = sys.stdin.buffer.readline()
                if not line:
                    sys.exit(0)
                if line == b'\r\n':
                    break
                key, value = line.decode().split(':', 1)
                headers[key.lower()] = value.strip()
            message = json.loads(sys.stdin.buffer.read(int(headers['content-length'])))
        result = handle(message)
        if result is not None:
            send(result)
