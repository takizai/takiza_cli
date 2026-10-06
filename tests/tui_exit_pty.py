"""Exit summary must survive screen cleanup and resume the exact saved chat."""
import http.server
import json
from pathlib import Path
import re
import sys
import tempfile
import threading
from tui_queue_pty import Terminal, Screen

requests = []
missing_usage = False
busy = False
release = threading.Event()


class API(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        self.send_response(200)
        self.end_headers()
        if not body.get('stream'):
            self.wfile.write(b'{"choices":[{"message":{"content":"Exit test"}}]}')
            return
        requests.append(body)
        try:
            self.wfile.write(b'data: {"choices":[{"delta":{"content":"EXIT_REPLY_OK"}}]}\n\n')
            self.wfile.flush()
            if busy:
                release.wait(10)
            if not missing_usage:
                self.wfile.write(b'data: {"choices":[],"usage":{"total_tokens":120}}\n\n')
            self.wfile.write(b'data: [DONE]\n\n')
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass


def screen(terminal):
    return '\n'.join(Screen(terminal.output, 100, 26).text)


def summary(terminal, expected, started=True):
    terminal.wait(lambda: terminal.process.poll() is not None, 'exit did not finish')
    terminal.pump(.1)
    assert terminal.process.returncode == 0
    output = terminal.output
    goodbye = output.rfind(b'Goodbye!')
    assert goodbye > output.rfind(b'\x1b[?1049l'), 'summary was erased with the alternate screen'
    assert output.count(b'Goodbye!') == 1, 'duplicate goodbye'
    text = output[goodbye:].decode()
    assert expected in text, text
    match = re.search(r'takiza --session ([0-9_]+)', text)
    if not started:
        assert match is None and 'Resume this session' not in text, text
        return None
    assert match, text
    return match[1]


if __name__ == '__main__':
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/takiza').resolve()
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), API)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix='takiza-exit-') as directory:
            workspace = Path(directory)
            (workspace / '.takiza').mkdir()
            (workspace / '.takiza/config.json').write_text(json.dumps({
                'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Amber',
                'mode': 'manual', 'model': 'test-model', 'api_key': 'test-only',
                'base_url': f'http://127.0.0.1:{server.server_port}/v1',
            }))
            terminal = Terminal(binary, workspace, 100, 26)
            try:
                terminal.wait(lambda: '120 tokens' in screen(terminal), 'initial usage missing')
                terminal.send('/exit\r')
                session_id = summary(terminal, 'Reported model usage: 120 tokens')
                saved = json.loads((workspace / '.takiza/sessions' / f'{session_id}.json').read_text())
                assert saved['token_usage'] == {'total': 120, 'complete': True}
            finally:
                terminal.close()
            # Opening and closing a fresh chat must not create a session or
            # change the latest-session pointer.
            sessions_before = set((workspace / '.takiza/sessions').iterdir())
            other = Terminal(binary, workspace, 100, 26, initial_prompt=None)
            try:
                other.wait(lambda: 'You' in screen(other), 'empty chat input not ready')
                other.send('\x03')
                summary(other, '0 tokens', started=False)
                assert set((workspace / '.takiza/sessions').iterdir()) == sessions_before
                assert (workspace / '.takiza/latest_session').read_text() == session_id
            finally:
                other.close()
            for reset in [False, True]:
                other = Terminal(binary, workspace, 100, 26, initial_prompt=None)
                try:
                    other.wait(lambda: 'You' in screen(other), 'empty chat input not ready')
                    if reset:
                        other.send('/new\r')
                        other.pump(.2)
                    other.send('/exit\r')
                    summary(other, '0 tokens', started=False)
                    assert set((workspace / '.takiza/sessions').iterdir()) == sessions_before
                    assert (workspace / '.takiza/latest_session').read_text() == session_id
                finally:
                    other.close()
            terminal = Terminal(binary, workspace, 100, 26, initial_prompt=None, cli_args=['--session', session_id])
            try:
                terminal.wait(lambda: 'EXIT_REPLY_OK' in screen(terminal), 'specific session did not resume')
                terminal.send('second prompt\r')
                terminal.wait(lambda: len(requests) == 2 and screen(terminal).count('EXIT_REPLY_OK') == 2, 'second reply missing')
                terminal.pump(.2)
                terminal.send('\x03')
                assert summary(terminal, 'Reported model usage: 240 tokens') == session_id
            finally:
                terminal.close()
            missing_usage = True
            terminal = Terminal(binary, workspace, 100, 26)
            try:
                terminal.wait(lambda: 'tokens unavailable' in screen(terminal), 'unknown usage missing')
                terminal.send('/exit\r')
                summary(terminal, 'Reported model usage: tokens unavailable')
            finally:
                terminal.close()
            busy = True
            terminal = Terminal(binary, workspace, 100, 26)
            try:
                terminal.wait(lambda: 'EXIT_REPLY_OK' in screen(terminal), 'busy response missing')
                points = list((workspace / '.takiza/checkpoints').glob('*.json'))
                newest = max(points, key=lambda path: path.stat().st_mtime_ns)
                busy_id = json.loads(newest.read_text())['session_id']
                assert (workspace / '.takiza/sessions' / f'{busy_id}.json').exists(), 'first prompt was not saved until model completion'
                terminal.send('/exit\r')
                session_id = summary(terminal, 'Resume this session')
                saved = json.loads((workspace / '.takiza/sessions' / f'{session_id}.json').read_text())
                assert any(message['role'] == 'user' for message in saved['messages']), 'busy exit did not save conversation'
            finally:
                release.set()
                terminal.close()
            print('PASS exit summary: /exit, Ctrl+C, primary screen, token totals, exact resume, unknown usage, busy exit')
    finally:
        release.set()
        server.shutdown()
