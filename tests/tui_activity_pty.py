"""Activity indicator and atomic terminal frames during tools and long answers."""
import http.server
import json
from pathlib import Path
import re
import sys
import tempfile
import threading
import time
from tui_queue_pty import Terminal, Screen

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
            self.wfile.write(b'{"choices":[{"message":{"content":"Activity test"}}]}')
            return
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()

        def token(delta):
            self.wfile.write(('data: ' + json.dumps({'choices': [{'delta': delta}]}) + '\n\n').encode())
            self.wfile.flush()

        try:
            if not any(m.get('role') == 'tool' for m in body['messages']):
                token({'tool_calls': [{'index': 0, 'id': 'slow', 'type': 'function', 'function': {
                    'name': 'run_command', 'arguments': json.dumps({'command':
                        "printf '\x1b[2J\x1b[24;1H\x1b[38;2;255;195;0mCOMMAND_STARTED\x1b[0m\x1b[?1049l\x1b[?2026h\x1b]0;CHILD_TITLE\x07\\n'; "
                        "sleep 2; printf 'COMMAND_DONE\\n'"})}}]})
            else:
                for i in range(40):
                    token({'reasoning_content': f'Inspecting {i}. '})
                    time.sleep(.01)
                for i in range(70):
                    token({'content': f'**Строка {i:03d}**: `cargo test` — проверка интерфейса.\n'})
                    time.sleep(.01)
                finish.wait(10)
                token({'content': 'ANSWER_COMPLETE'})
            usage = 300 if any(m.get('role') == 'tool' for m in body['messages']) else 200
            self.wfile.write(('data: ' + json.dumps({'choices': [], 'usage': {'total_tokens': usage}}) + '\n\n').encode())
            self.wfile.write(b'data: [DONE]\n\n')
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass


def run(binary, cols, rows):
    finish.clear()
    with tempfile.TemporaryDirectory(prefix='takiza-activity-') as directory:
        workspace = Path(directory)
        (workspace / '.takiza').mkdir()
        (workspace / '.takiza/config.json').write_text(json.dumps({
            'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Monochrome',
            'mode': 'manual', 'model': 'test-model', 'api_key': 'test-only',
            'base_url': f'http://127.0.0.1:{server.server_port}/v1',
        }))
        terminal = Terminal(binary, workspace, cols, rows,
                            {'TAKIZA_AUTO_APPROVE': 'true'})
        def screen():
            return Screen(terminal.output, cols, rows).text
        def verify():
            lines = screen()
            assert 'You' in lines[-3] and lines[-3].startswith('╭'), lines
            assert lines[-2].startswith('│ > ') and 'draft' in lines[-2], lines
            assert lines[-1].startswith('╰') and lines[-1].endswith('╯'), lines
            assert not any('You' in line or 'Ctrl+O: Expand' in line for line in lines[:-3]), lines
            assert not re.search(r'38;2;\d+|\[\?2026|\[\d+;\d+H', '\n'.join(lines)), lines
        try:
            terminal.wait(lambda: any(line.strip() == 'COMMAND_STARTED' for line in screen()), 'slow command missing')
            terminal.wait(lambda: 'Executing tools...' in '\n'.join(screen()), 'tool activity disappeared')
            assert 'Executing tools...' in screen()[-4], 'activity overwrote the transcript'
            assert any('command' in line and 'printf' in line for line in screen()[:-4]), screen()
            terminal.send('draft')
            verify()
            # Exercise editor redraws while the spinner and streamed reasoning write.
            for _ in range(8):
                terminal.send('x\x7f')
                verify()
            terminal.wait(lambda: 'Строка 069' in '\n'.join(screen()), 'long response missing', timeout=8)
            verify()
            assert 'Responding...' in screen()[-4], 'activity disappeared before generation finished'
            terminal.send('x\x7f')
            verify()
            assert 'Responding...' in screen()[-4], 'editor redraw removed response activity'
            terminal.send('\x1b[1;5H')
            verify()
            assert any(line.strip() == 'COMMAND_STARTED' for line in screen()), 'command text lost during scroll'
            assert 'CHILD_TITLE' not in '\n'.join(screen()), 'terminal title sequence leaked into text'
            assert b'\x1b]0;CHILD_TITLE' not in terminal.output, 'command changed the terminal title'
            assert b'\x1b[?1049l' not in terminal.output, 'command left the application screen'
            terminal.send('\x1b[1;5F')
            verify()
            assert 'Responding...' in screen()[-4], 'scrolling removed response activity'
            finish.set()
            terminal.wait(lambda: 'ANSWER_COMPLETE' in '\n'.join(screen()), 'completion missing')
            terminal.pump(.2)
            verify()
            assert 'Executing tools...' not in '\n'.join(screen())
            assert 'Responding...' not in '\n'.join(screen()), 'activity remained after completion'
            assert 'Completed in' in '\n'.join(screen()) and '500 tokens' in '\n'.join(screen()), screen()
            transcript = list((workspace / '.takiza/sessions').glob('*.json'))
            assert any('ResponseStats' in path.read_text() and '500' in path.read_text() for path in transcript)
            print(f'PASS {cols}x{rows}: tool activity, concurrent reasoning/input, long answer, fixed frame')
        finally:
            finish.set()
            terminal.close()


if __name__ == '__main__':
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/takiza').resolve()
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), API)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        for size in [(80, 26), (100, 34), (45, 14)]:
            run(binary, *size)
    finally:
        server.shutdown()
