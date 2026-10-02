"""Reasoning must appear while the API response is still open.

Run: python3 tests/tui_reasoning_pty.py [binary]
"""
import http.server
import json
import re
from pathlib import Path
import sys
import tempfile
import threading
from tui_queue_pty import Terminal, Screen

advance = threading.Event()
finish = threading.Event()
tagged = False


class API(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        self.send_response(200)
        if not body.get('stream'):
            self.send_header('Content-Type', 'application/json')
            self.end_headers()
            self.wfile.write(b'{"choices":[{"message":{"content":"Reasoning test"}}]}')
            return
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()

        def token(field, text):
            packet = {'choices': [{'delta': {field: text}}]}
            self.wfile.write(('data: ' + json.dumps(packet) + '\n\n').encode())
            self.wfile.flush()

        try:
            token('content' if tagged else 'reasoning_content', '<think>FIRST_THOUGHT' if tagged else 'FIRST_THOUGHT')
            if not advance.wait(10):
                return
            token('content' if tagged else 'reasoning_content', ' SECOND_THOUGHT\n' + '\n'.join(f'Thought line {i}' for i in range(40)))
            if not finish.wait(10):
                return
            token('content', '</think>FINAL_ANSWER' if tagged else 'FINAL_ANSWER')
            self.wfile.write(b'data: [DONE]\n\n')
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass


if __name__ == '__main__':
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/takiza').resolve()
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), API)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        for tagged in [False, True]:
            advance.clear()
            finish.clear()
            with tempfile.TemporaryDirectory(prefix='takiza-reasoning-') as directory:
                workspace = Path(directory)
                (workspace / '.takiza').mkdir()
                (workspace / '.takiza/config.json').write_text(json.dumps({
                    'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Amber',
                    'mode': 'manual', 'model': 'test-model', 'api_key': 'test-only',
                    'base_url': f'http://127.0.0.1:{server.server_port}/v1',
                }))
                terminal = Terminal(binary, workspace, 80, 26)
                def screen():
                    return '\n'.join(Screen(terminal.output, 80, 26).text)
                def thinking_status():
                    line = Screen(terminal.output, 80, 26).text[-4].strip()
                    match = re.match(r'[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏] (.+?\.\.\.)', line)
                    return match[1] if match else None
                try:
                    terminal.wait(lambda: 'FIRST_THOUGHT' in screen(), 'reasoning hidden until response completion')
                    terminal.wait(lambda: thinking_status() is not None, 'thinking status disappeared during reasoning')
                    status = thinking_status()
                    assert status == 'Thinking...' or not status.startswith('Thinking'), status
                    assert 'FINAL_ANSWER' not in screen()
                    advance.set()
                    terminal.wait(lambda: 'FIRST_THOUGHT SECOND_THOUGHT' in screen(), 'reasoning did not update incrementally')
                    assert thinking_status() == status, 'thinking phrase changed between reasoning tokens'
                    terminal.send('\x0f')  # Expand enough reasoning to enable scrolling.
                    for key in ['\x1b[1;5H', '\x1b[1;5F', '\x1b[5~', '\x1b[<64;10;5M', '\x1b[1;5F']:
                        terminal.send(key)
                        assert thinking_status() == status, 'scroll hid or changed the activity status'
                    terminal.send('\x0f')
                    finish.set()
                    terminal.wait(lambda: 'FINAL_ANSWER' in screen(), 'final answer missing')
                    assert screen().count('FIRST_THOUGHT') == 1, 'reasoning duplicated after completion'
                    print(f'PASS streaming reasoning: {"think tags" if tagged else "reasoning_content"}')
                finally:
                    advance.set()
                    finish.set()
                    terminal.close()
    finally:
        server.shutdown()
