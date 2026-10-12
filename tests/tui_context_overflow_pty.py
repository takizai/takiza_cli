"""Learn a provider cap from an HTTP overflow and reuse it after restarting."""
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import sys
import tempfile
import threading
from tui_queue_pty import Terminal, Screen


if __name__ == '__main__':
    metadata_gate = threading.Event()
    requests = []
    gets = []

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_GET(self):
            gets.append(self.path)
            metadata_gate.wait(10)
            body = json.dumps({'data': [{'id': 'test', 'context_length': 1000000}]}).encode()
            self.send_response(200)
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            try:
                self.wfile.write(body)
            except (BrokenPipeError, ConnectionResetError):
                pass  # The first process exits while metadata is deliberately held.

        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            if not request.get('tools'):
                body = json.dumps({'choices': [{'message': {'content': 'Overflow test'}}]}).encode()
                self.send_response(200)
                self.send_header('Content-Length', str(len(body)))
                self.end_headers()
                self.wfile.write(body)
                return
            requests.append(request)
            message = ("This model's maximum context length is 131072 tokens. "
                       "However, you requested 140000 tokens (131808 input + 8192 output).")
            if request['model'] != 'test':
                message = 'Input is too long: requested 140000 tokens'
            body = json.dumps({'error': {'message': message, 'code': 'context_length_exceeded'}}).encode()
            self.send_response(400)
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/takiza').resolve()
    try:
        with tempfile.TemporaryDirectory(prefix='takiza-context-overflow-') as directory:
            workspace = Path(directory)
            (workspace / '.takiza').mkdir()
            (workspace / '.takiza/config.json').write_text(json.dumps({
                'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Monochrome',
                'mode': 'manual', 'model': 'test', 'api_key': 'test-secret',
                'base_url': f'http://127.0.0.1:{server.server_port}/v1'}))
            for run in range(2):
                terminal = Terminal(binary, workspace, 100, 30, initial_prompt=None)

                def screen():
                    return '\n'.join(Screen(terminal.output, 100, 30).text)

                try:
                    if run == 0:
                        terminal.wait(lambda: gets and '0/?' in screen(), 'initial maximum was not unknown')
                        terminal.send('trigger overflow\r')
                        terminal.wait(lambda: '/128K' in screen(), 'overflow cap did not update footer')
                        cache = list((workspace / '.takiza/context-limits').glob('*.json'))
                        assert len(cache) == 1, 'learned cap was not persisted'
                        assert json.loads(cache[0].read_text())['tokens'] == 131072
                        assert 'test-secret' not in cache[0].read_text(), 'cache exposed credentials'
                    else:
                        terminal.wait(lambda: '0/128K' in screen(), 'restart did not reuse learned cap')
                        metadata_gate.set()
                        terminal.pump(.5)
                        assert '0/128K' in screen(), 'larger metadata overwrote learned cap'
                        terminal.send('/model other\r')
                        terminal.wait(lambda: '0/?' in screen(), 'learned cap leaked to another model')
                        terminal.send('generic overflow\r')
                        terminal.wait(lambda: len(requests) >= 2, 'generic overflow request did not start')
                        terminal.pump(.5)
                        assert '/?' in screen(), 'requested token count was mistaken for a cap'
                        assert len(list((workspace / '.takiza/context-limits').glob('*.json'))) == 1
                    terminal.send('/exit\r')
                    terminal.wait(lambda: terminal.process.poll() is not None, 'exit stalled')
                    assert terminal.process.returncode == 0
                except Exception:
                    print('Screen:', screen())
                    raise
                finally:
                    terminal.close()
            print('PASS overflow cap: footer, persistence, restart, metadata race, model isolation, ambiguous error')
    finally:
        metadata_gate.set()
        server.shutdown()
        server.server_close()
