"""Model metadata updates the idle header without blocking or losing a draft."""
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import sys
import tempfile
import threading
from tui_queue_pty import Terminal, Screen


if __name__ == '__main__':
    gate = threading.Event()
    requests = []
    stream_gates = [threading.Event(), threading.Event(), threading.Event()]
    chat_requests = []

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_GET(self):
            requests.append((self.path, self.headers.get('Authorization')))
            gate.wait(5)
            data = {'data': [
                {'id': 'small', 'context_length': 131072},
                {'id': 'large', 'context_window': 2000000},
                {'id': 'unknown'},
            ]} if self.path == '/v1/models' else {'id': 'unknown'}
            body = json.dumps(data).encode()
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            if not request.get('tools'):
                body = json.dumps({'choices': [{'message': {'content': 'Context test'}}]}).encode()
                self.send_response(200)
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(body)))
                self.end_headers()
                self.wfile.write(body)
                return
            chat_requests.append(request)
            self.send_response(200)
            self.send_header('Content-Type', 'text/event-stream')
            self.send_header('Connection', 'close')
            self.end_headers()

            def event(value):
                self.wfile.write(('data: ' + json.dumps(value) + '\n\n').encode())
                self.wfile.flush()

            if len(chat_requests) == 1:
                for tokens, stream_gate in zip([8192, 65536, 131072], stream_gates):
                    event({'choices': [], 'usage': {'total_tokens': tokens}})
                    stream_gate.wait(10)
            else:
                event({'choices': [], 'usage': {'total_tokens': 32768}})
            event({'choices': [{'delta': {'content': 'context response'}, 'finish_reason': 'stop'}]})
            self.wfile.write(b'data: [DONE]\n\n')
            self.wfile.flush()

    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/takiza').resolve()
    try:
        with tempfile.TemporaryDirectory(prefix='takiza-model-context-') as directory:
            workspace = Path(directory)
            (workspace / '.takiza').mkdir()
            (workspace / '.takiza/config.json').write_text(json.dumps({
                'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Monochrome',
                'mode': 'manual', 'model': 'small', 'api_key': 'test-only',
                'base_url': f'http://127.0.0.1:{server.server_port}/v1'}))
            terminal = Terminal(binary, workspace, 100, 30, initial_prompt=None)

            def screen(cols=100, rows=30):
                return '\n'.join(Screen(terminal.output, cols, rows).text)

            try:
                terminal.wait(lambda: requests and '0/?' in screen(), 'metadata lookup did not start')
                terminal.send('preserved draft')
                terminal.wait(lambda: 'preserved draft' in screen(), 'metadata request blocked typing')
                gate.set()
                terminal.wait(lambda: '0/128K' in screen(), 'idle header did not update')
                assert 'preserved draft' in screen(), 'metadata update lost the draft'
                terminal.send('\x15/model large\r')
                terminal.wait(lambda: '0/2M' in screen(), 'switching model kept the old context limit')
                terminal.send('/model unknown\r')
                terminal.wait(lambda: len(requests) == 9 and '0/?' in screen(), 'missing metadata did not show ?')
                terminal.send('/model small\r')
                terminal.wait(lambda: '0/128K' in screen(), 'cached model limit did not return')
                assert len(requests) == 9, f'metadata was not cached: {requests}'
                assert all(auth == 'Bearer test-only' for _, auth in requests), 'metadata did not use provider credentials'
                terminal.resize(55, 20)
                terminal.wait(lambda: '0/128K' in screen(55, 20), 'compact header lost context')
                terminal.resize(100, 30)
                terminal.send('inspect context\r')
                terminal.wait(lambda: '8K/128K' in screen(), 'stream usage did not update context')
                assert '~88% to compact' in screen(), 'headroom did not reserve output tokens'
                terminal.send('preserved streaming draft')
                stream_gates[0].set()
                terminal.wait(lambda: '64K/128K' in screen(), 'half-full context did not update')
                assert '~44% to compact' in screen(), 'headroom did not decrease with usage'
                assert b'38;2;240;200;70m' in terminal.output, 'half-full context was not yellow'
                assert 'preserved streaming draft' in screen(), 'usage update lost active draft'
                stream_gates[1].set()
                terminal.wait(lambda: '128K/128K' in screen(), 'full context did not update')
                assert '~0% to compact' in screen(), 'full context did not clamp headroom to zero'
                assert b'38;2;245;80;80m' in terminal.output, 'full context was not red'
                stream_gates[2].set()
                terminal.wait(lambda: 'context response' in screen(), 'response did not finish')
                terminal.pump(.3)
                footer = screen().splitlines()[-1]
                assert footer.index('128K/128K') < footer.index('💬'), 'meter was not left of the title'
                terminal.send('\x15next request\r')
                terminal.wait(lambda: '32K/128K' in screen(), 'context usage accumulated across requests')
                assert '~69% to compact' in screen(), 'headroom did not recover for smaller context'
                terminal.pump(.3)
                terminal.send('/exit\r')
                terminal.wait(lambda: terminal.process.poll() is not None, 'exit stalled')
                assert terminal.process.returncode == 0
                saved = [json.loads(path.read_text()) for path in (workspace / '.takiza/sessions').glob('*.json')]
                assert any(session.get('context_tokens') == 32768 for session in saved), 'current context was not saved'
                print('PASS model context: lookup, cache, model switch, compact footer, streamed usage, gradient, draft, persistence')
            except Exception:
                print('Requests:', requests)
                print('Screen:', screen())
                raise
            finally:
                gate.set()
                for stream_gate in stream_gates:
                    stream_gate.set()
                terminal.close()
    finally:
        server.shutdown()
        server.server_close()
