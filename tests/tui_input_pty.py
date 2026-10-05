"""Three-line input viewport and double Escape, during responses and while idle."""
import http.server
import json
from pathlib import Path
import sys
import tempfile
import threading
from tui_queue_pty import Terminal, Screen

requests = []
finish = threading.Event()


class API(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        self.send_response(200)
        if not body.get('stream'):
            self.send_header('Content-Type', 'application/json')
            self.end_headers()
            self.wfile.write(b'{"choices":[{"message":{"content":"Input test"}}]}')
            return
        requests.append(body)
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()
        try:
            self.wfile.write(b'data: {"choices":[{"delta":{"content":"MODEL_OUTPUT"}}]}\n\n')
            self.wfile.flush()
            finish.wait(20)
            self.wfile.write(b'data: [DONE]\n\n')
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass


def run(binary):
    with tempfile.TemporaryDirectory(prefix='takiza-input-') as directory:
        workspace = Path(directory)
        (workspace / '.takiza').mkdir()
        (workspace / '.takiza/config.json').write_text(json.dumps({
            'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Monochrome',
            'mode': 'manual', 'model': 'test-model', 'api_key': 'test-only',
            'base_url': f'http://127.0.0.1:{server.server_port}/v1',
        }))
        terminal = Terminal(binary, workspace, 80, 26)
        # Explicit rows keep viewport assertions independent of continuation
        # width: only the first visual row now reserves space for the prompt.
        text = '\n'.join(f'L{i:02d}' + 'x' * 71 for i in range(9))
        def screen():
            return Screen(terminal.output, 80, 26).text
        def paste(value):
            terminal.send('\x1b[200~' + value + '\x1b[201~')
        def field():
            lines = screen()
            top = next(y for y, line in enumerate(lines) if line.startswith('╭') and 'You' in line)
            assert len(lines) - top <= 5, 'input grew beyond three content rows'
            return lines[top:]
        def scroll(up):
            terminal.send(f'\x1b[<{64 if up else 65};10;24M')
        try:
            terminal.wait(lambda: 'MODEL_OUTPUT' in '\n'.join(screen()), 'response missing')
            paste(text)
            assert len(field()) == 5 and '7-9/9' in field()[0], field()
            assert 'L06' in field()[1] and 'L08' in field()[3], field()
            # The busy indicator above the editor animates independently.
            model = screen()[:-6]
            scroll(True)
            assert 'L03' in field()[1] and 'L05' in field()[3], field()
            scroll(True)
            assert 'L00' in field()[1] and 'L02' in field()[3], field()
            assert screen()[:-6] == model, 'scrolling input moved conversation'
            scroll(False)
            assert 'L03' in field()[1], field()
            terminal.send('\x1b[F')
            assert 'L08' in field()[3], 'End did not return to caret'
            terminal.send('\x1b')
            terminal.send('\x1b')
            terminal.wait(lambda: len(field()) == 3 and 'L08' not in '\n'.join(field()), 'double Escape did not clear busy draft')
            # First Escape only dismisses the command menu, then the second clears.
            paste('/t')
            terminal.send('\x1b')
            assert '/t' in '\n'.join(field()), 'single Escape cleared the draft'
            terminal.send('\x1b')
            assert '/t' not in '\n'.join(field()), 'second Escape did not clear command draft'
            paste(text)
            scroll(True)
            scroll(True)
            assert 'L00' in field()[1], 'idle input did not scroll to beginning'
            terminal.send('\x1b')
            terminal.pump(.6)
            terminal.send('\x1b')
            assert len(field()) == 5, 'separate Escape presses cleared the draft'
            terminal.send('\x1b')
            assert len(field()) == 3, 'double Escape did not clear idle draft'
            paste(text)
            scroll(True)
            terminal.send('\r')
            terminal.wait(lambda: len(requests) == 2, 'long draft did not submit')
            assert requests[-1]['messages'][-1]['content'] == text, 'scrolling truncated or changed the draft'
            terminal.send('/exit\r')
            terminal.wait(lambda: terminal.process.poll() is not None, 'exit failed')
            print('PASS input: three-line cap, wheel viewport busy/idle, caret following, double Escape/timing/menu, full draft submission')
        finally:
            finish.set()
            terminal.close()


if __name__ == '__main__':
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/takiza').resolve()
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), API)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        run(binary)
    finally:
        server.shutdown()
