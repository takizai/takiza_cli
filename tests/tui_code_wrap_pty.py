"""Long prompt/code lines remain complete while streaming, resizing and saving."""
import http.server
import json
from pathlib import Path
import sys
import tempfile
import threading
from tui_queue_pty import Terminal, Screen

release = threading.Event()
literal = ('Create a premium cinematic motion graphics website hero video for Takiza Code. '
           'Preserve every word, including this long token: ' + 'abcdefghij' * 8 + ' FULL_PROMPT_END')


class API(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_): pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        self.send_response(200); self.end_headers()
        if not body.get('stream'):
            self.wfile.write(b'{"choices":[{"message":{"content":"Code wrap test"}}]}'); return
        def token(text):
            self.wfile.write(('data: ' + json.dumps({'choices': [{'delta': {'content': text}}]}) + '\n\n').encode())
            self.wfile.flush()
        try:
            token('```text\n' + literal + '\n')
            release.wait(10)
            token('```\nWRAP_DONE')
            self.wfile.write(b'data: [DONE]\n\n'); self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError): pass


def check(terminal, cols):
    screen = Screen(terminal.output, cols, 40).text
    code = [line for line in screen if line.startswith('    │ ')]
    restored = ''.join(line.partition('│')[2].rpartition('│')[0].strip() for line in code)
    assert restored.replace(' ', '') == literal.replace(' ', ''), '\n'.join(screen)
    assert all(line.rstrip().endswith('│') for line in code), 'code border clipped'
    assert not any('…' in line for line in code), 'code content truncated'


if __name__ == '__main__':
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/takiza').resolve()
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), API)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        for cols in [80, 40]:
            release.clear()
            with tempfile.TemporaryDirectory(prefix='takiza-code-wrap-') as directory:
                workspace = Path(directory); (workspace / '.takiza').mkdir()
                (workspace / '.takiza/config.json').write_text(json.dumps({
                    'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Amber', 'mode': 'manual',
                    'model': 'test-model', 'api_key': 'test-only',
                    'base_url': f'http://127.0.0.1:{server.server_port}/v1'}))
                terminal = Terminal(binary, workspace, cols, 40)
                try:
                    terminal.wait(lambda: b'FULL_PROMPT_END' in terminal.output, 'stream tail missing')
                    check(terminal, cols)
                    release.set()
                    terminal.wait(lambda: 'WRAP_DONE' in '\n'.join(Screen(terminal.output, cols, 40).text), 'final answer missing')
                    check(terminal, cols)
                    resized = 40 if cols == 80 else 80
                    terminal.resize(resized, 40)
                    check(terminal, resized)
                    terminal.send('\x0f'); check(terminal, resized)
                    terminal.send('/exit\r')
                    terminal.wait(lambda: terminal.process.poll() is not None, 'exit failed')
                    sid = (workspace / '.takiza/latest_session').read_text()
                    saved = json.loads((workspace / '.takiza/sessions' / (sid + '.json')).read_text())
                    assert any(item.get('type') == 'AssistantMessage' and literal in item.get('payload', '') for item in saved['history'])
                    print(f'PASS code wrapping: {cols} columns, stream, complete answer, resize, Ctrl+O, original saved text')
                finally: release.set(); terminal.close()
    finally: release.set(); server.shutdown()
