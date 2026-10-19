"""Saved provider isolation, live catalogs and native tool turns via the actual TUI."""
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import sys
import tempfile
import threading
from tui_queue_pty import Terminal, Screen

requests = []
turns = []

class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def reply(self, value):
        data = json.dumps(value).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        requests.append((self.path, dict(self.headers)))
        if self.path.startswith('/b/v1/models'):
            assert self.headers.get('x-api-key') == 'key-b-only'
            assert self.headers.get('Authorization') is None
            assert self.headers.get('x-route') == 'b-route'
            self.reply({'data': [{'id': 'b-deployment', 'display_name': 'Friendly Claude'}]})
        elif self.path.startswith('/a/v1/models'):
            assert self.headers.get('Authorization') == 'Bearer key-a-only'
            self.reply({'data': [{'id': 'a-deployment', 'name': 'Friendly GPT'}]})
        elif self.path.startswith('/c/v1/models'):
            assert self.headers.get('Authorization') == 'Bearer key-c-secret'
            self.reply({'data': [{'id': 'c-deployment', 'name': 'Friendly Responses'}]})
        elif self.path.startswith('/public/v1/models'):
            assert self.headers.get('Authorization') is None, 'another provider key leaked into public catalog'
            assert self.headers.get('x-api-key') is None
            self.reply({'data': [{'id': 'public-model', 'name': 'Public Model'}]})
        elif self.path.startswith('/env/v1/models'):
            assert self.headers.get('Authorization') == 'Bearer env-catalog-secret'
            self.reply({'data': [{'id': 'env-model', 'name': 'Environment Model'}]})
        elif self.path.startswith('/locked/v1/models'):
            assert self.headers.get('Authorization') is None
            self.send_response(401)
            self.end_headers()
        elif self.path.startswith('/missing/v1/models'):
            self.send_response(404)
            self.end_headers()
        else:
            self.reply({})

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        assert self.path == '/b/v1/messages', (self.path, body)
        assert self.headers.get('x-api-key') == 'key-b-only'
        assert self.headers.get('Authorization') is None
        assert body['model'] == 'b-deployment'
        if not body.get('tools'):
            self.reply({'content': [{'type': 'text', 'text': 'Provider test'}], 'stop_reason': 'end_turn'})
            return
        assert any(tool['name'] == 'web_search' for tool in body['tools']), 'Takiza search was omitted from provider request'
        turns.append(body)
        events = [
            {'type': 'message_start', 'message': {'usage': {'input_tokens': 100, 'output_tokens': 0}}},
        ]
        if len(turns) == 1:
            events += [
                {'type': 'content_block_start', 'index': 0, 'content_block': {'type': 'thinking', 'thinking': '', 'signature': ''}},
                {'type': 'content_block_delta', 'index': 0, 'delta': {'type': 'thinking_delta', 'thinking': 'Checking the file.'}},
                {'type': 'content_block_delta', 'index': 0, 'delta': {'type': 'signature_delta', 'signature': 'opaque-signature'}},
                {'type': 'content_block_stop', 'index': 0},
                {'type': 'content_block_start', 'index': 1, 'content_block': {'type': 'tool_use', 'id': 'file-call', 'name': 'read_file', 'input': {}}},
                {'type': 'content_block_delta', 'index': 1, 'delta': {'type': 'input_json_delta', 'partial_json': '{"path":"fixture.txt"}'}},
                {'type': 'content_block_stop', 'index': 1},
                {'type': 'message_delta', 'delta': {'stop_reason': 'tool_use'}, 'usage': {'output_tokens': 15}},
            ]
        else:
            assistant = next(m for m in body['messages'] if m['role'] == 'assistant')
            assert assistant['content'][0]['signature'] == 'opaque-signature'
            assert body['messages'][-1]['content'][0]['tool_use_id'] == 'file-call'
            assert 'fixture contents' in body['messages'][-1]['content'][0]['content']
            events += [
                {'type': 'content_block_start', 'index': 0, 'content_block': {'type': 'text', 'text': ''}},
                {'type': 'content_block_delta', 'index': 0, 'delta': {'type': 'text_delta', 'text': 'NATIVE_OK'}},
                {'type': 'content_block_stop', 'index': 0},
                {'type': 'message_delta', 'delta': {'stop_reason': 'end_turn'}, 'usage': {'output_tokens': 5}},
            ]
        events.append({'type': 'message_stop'})
        data = ''.join('data: ' + json.dumps(e) + '\n\n' for e in events).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)

if __name__ == '__main__':
    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/takiza').resolve()
    try:
        with tempfile.TemporaryDirectory(prefix='takiza-providers-') as directory:
            workspace = Path(directory)
            (workspace / '.takiza').mkdir()
            (workspace / 'fixture.txt').write_text('fixture contents')
            def profile(name, prefix, key, api, model):
                return {'name': name, 'api_type': api, 'base_url': f'http://127.0.0.1:{server.server_port}/{prefix}/v1',
                        'api_key': key, 'model': model, 'models': [], 'headers': {'x-route': prefix + '-route'}}
            config = {'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Monochrome',
                      'mode': 'manual', 'execution_mode': 'allow-all', 'active_provider': 'a', 'providers': {
                          'a': profile('Connection A', 'a', 'key-a-only', 'chat_completions', 'a-deployment'),
                          'b': profile('Connection B', 'b', 'key-b-only', 'anthropic_messages', 'b-initial')},
                      'api_key': 'stale-key-must-not-be-used'}
            config_path = workspace / '.takiza/config.json'
            config_path.write_text(json.dumps(config))
            terminal = Terminal(binary, workspace, 110, 30, initial_prompt=None,
                                env_overrides={'TAKIZA_TEST_CATALOG_KEY': 'env-catalog-secret'})
            def screen():
                return '\n'.join(Screen(terminal.output, 110, 30).text)
            def saved():
                return json.loads(config_path.read_text())
            try:
                terminal.wait(lambda: 'a-deployment' in screen(), 'initial saved provider was not loaded')
                terminal.send('/provider Connection B\r')
                terminal.wait(lambda: saved()['active_provider'] == 'b' and 'b-initial' in screen(), 'provider switch failed')
                terminal.send('/model\r')
                terminal.wait(lambda: 'Friendly Claude' in screen() and 'b-deployment' in screen(), 'native model catalog missing')
                assert 'Friendly GPT' not in screen(), 'models from the previous provider leaked'
                terminal.send('Friendly\r')
                terminal.wait(lambda: saved()['providers']['b']['model'] == 'b-deployment', 'readable-name search selected the wrong ID')
                terminal.send('read the fixture file\r')
                terminal.wait(lambda: 'NATIVE_OK' in screen(), 'native tool roundtrip did not finish', timeout=20)
                terminal.send('/provider\r')
                terminal.wait(lambda: 'Saved providers' in screen(), 'saved provider manager missing')
                terminal.send('\x1b[B\x1b[B\r')
                terminal.wait(lambda: 'Connection name:' in screen(), 'add provider form missing')
                terminal.send('Connection C\r')
                terminal.wait(lambda: 'API type' in screen(), 'API type selector missing')
                terminal.send('\x1b[B\r')  # OpenAI Responses
                terminal.wait(lambda: 'Base URL:' in screen(), 'endpoint input missing')
                terminal.send('\x15' + f'http://127.0.0.1:{server.server_port}/c/v1' + '\r')
                terminal.wait(lambda: 'API key (optional):' in screen(), 'optional secret input missing')
                terminal.send('key-c-secret')
                terminal.wait(lambda: '************' in screen(), 'API key was not masked')
                assert 'key-c-secret' not in screen(), 'API key was displayed'
                terminal.send('\r')
                terminal.wait(lambda: 'environment variable' in screen(), 'env key input missing')
                terminal.send('\r')
                terminal.wait(lambda: 'Friendly Responses' in screen(), 'creation did not load the model catalog')
                assert len(saved()['providers']) == 2, 'unfinished connection was saved before model selection'
                terminal.send('Friendly Responses\r')
                terminal.wait(lambda: saved()['active_provider'].startswith('provider-'), 'new connection was not saved')
                prefs = saved()
                assert len(prefs['providers']) == 3
                current = prefs['providers'][prefs['active_provider']]
                assert current['api_type'] == 'openai_responses'
                assert current['api_key'] == 'key-c-secret'
                assert prefs['providers']['a']['model'] == 'a-deployment'
                assert prefs['providers']['b']['model'] == 'b-deployment'
                # Editing without changing fields must retain the API type and key.
                terminal.send('/provider\r')
                terminal.wait(lambda: 'Saved providers' in screen(), 'manager did not reopen')
                terminal.send('\r')  # Active connection is first.
                terminal.wait(lambda: 'Edit connection' in screen(), 'connection actions missing')
                terminal.send('\x1b[B\r')
                terminal.wait(lambda: 'Connection name:' in screen(), 'edit form missing')
                terminal.send('\r')
                terminal.wait(lambda: 'API type' in screen(), 'edit API type missing')
                terminal.send('\r')  # Keep existing Responses API.
                terminal.wait(lambda: 'Base URL:' in screen(), 'edit endpoint missing')
                terminal.send('\r')
                terminal.wait(lambda: 'API key (empty:' in screen(), 'edit key missing')
                terminal.send('\r')
                terminal.wait(lambda: 'environment variable' in screen(), 'edit env key missing')
                terminal.send('\r')
                terminal.wait(lambda: 'Friendly Responses' in screen(), 'edit model selector missing')
                terminal.send('Friendly Responses\r')
                terminal.wait(lambda: 'Switched provider:' in screen(), 'edited connection not activated')
                current = saved()['providers'][saved()['active_provider']]
                assert current['api_type'] == 'openai_responses', 'editing reset the API type'
                assert current['api_key'] == 'key-c-secret', 'editing cleared the API key'
                assert current['model'] == 'c-deployment'
                assert current['models'][0]['name'] == 'Friendly Responses', 'draft catalog was not saved'

                def add_draft(name, prefix, key_env=''):
                    count = len(saved()['providers'])
                    terminal.send('/provider\r')
                    terminal.wait(lambda: 'Saved providers' in screen(), 'manager did not reopen for draft')
                    terminal.send('\x1b[B' * count + '\r')
                    terminal.wait(lambda: 'Connection name:' in screen(), 'draft name input missing')
                    terminal.send(name + '\r')
                    terminal.wait(lambda: 'API type' in screen(), 'draft API selector missing')
                    terminal.send('\r')
                    terminal.wait(lambda: 'Base URL:' in screen(), 'draft endpoint input missing')
                    terminal.send('\x15' + f'http://127.0.0.1:{server.server_port}/{prefix}/v1' + '\r')
                    terminal.wait(lambda: 'API key (optional):' in screen(), 'draft optional key input missing')
                    terminal.send('\r')
                    terminal.wait(lambda: 'environment variable' in screen(), 'draft key environment input missing')
                    terminal.send(key_env + '\r')
                    return count

                count = add_draft('Public Connection', 'public')
                terminal.wait(lambda: 'Public Model' in screen(), 'anonymous catalog missing')
                assert len(saved()['providers']) == count, 'public draft saved too early'
                terminal.send('Public Model\r')
                terminal.wait(lambda: len(saved()['providers']) == count + 1, 'public connection not saved')
                public = saved()['providers'][saved()['active_provider']]
                assert public['api_key'] == '' and public['model'] == 'public-model'

                count = add_draft('Environment Connection', 'env', 'TAKIZA_TEST_CATALOG_KEY')
                terminal.wait(lambda: 'Environment Model' in screen(), 'environment-authenticated catalog missing')
                terminal.send('Environment Model\r')
                terminal.wait(lambda: len(saved()['providers']) == count + 1, 'environment-key connection not saved')
                env_provider = saved()['providers'][saved()['active_provider']]
                assert env_provider['api_key'] == ''
                assert env_provider['api_key_env'] == 'TAKIZA_TEST_CATALOG_KEY'
                assert env_provider['model'] == 'env-model'

                count = add_draft('Locked Connection', 'locked')
                terminal.wait(lambda: 'Catalog requires authorization.' in screen(), 'missing catalog auth diagnostic')
                terminal.send('manual-model\r')
                terminal.wait(lambda: 'Model ID:' in screen(), 'manual fallback missing')
                terminal.send('\r')
                terminal.wait(lambda: 'Display name:' in screen(), 'manual display name missing')
                terminal.send('\r')
                terminal.wait(lambda: len(saved()['providers']) == count + 1, 'manual fallback not saved')
                locked = saved()['providers'][saved()['active_provider']]
                assert locked['model'] == 'manual-model'

                before = saved()
                add_draft('Cancelled Connection', 'missing')
                terminal.wait(lambda: 'Catalog unavailable.' in screen(), 'generic catalog error missing')
                terminal.send('\x1b')
                terminal.wait(lambda: 'Saved providers' in screen(), 'cancel did not return to providers')
                assert saved() == before, 'cancelled draft changed saved configuration'
                terminal.send('\x1b')
                terminal.pump(.2)
                terminal.send('/provider Connection A\r')
                terminal.wait(lambda: saved()['active_provider'] == 'a', 'switch back failed')
                terminal.send('/model\r')
                terminal.wait(lambda: 'Friendly GPT' in screen(), 'first provider catalog missing')
                assert 'Friendly Claude' not in screen(), 'native provider models leaked into compatible catalog'
                terminal.send('\x1b')
                terminal.pump(.2)
                terminal.send('/exit\r')
                terminal.wait(lambda: terminal.process.poll() is not None, 'exit stalled')
                assert terminal.process.returncode == 0
                sessions = [json.loads(p.read_text()) for p in (workspace / '.takiza/sessions').glob('*.json')]
                assert any(any(m.get('provider_data') for m in s['messages']) for s in sessions), 'native continuation data not persisted'
                assert len(turns) == 2
                print('PASS providers: profiles, credentials, live catalogs, names/IDs, native tools, signed history, wizard selection, public/auth catalogs, manual fallback, draft cancellation, masking, persistence')
            except Exception:
                print(screen())
                raise
            finally:
                terminal.close()
    finally:
        server.shutdown()
        server.server_close()
