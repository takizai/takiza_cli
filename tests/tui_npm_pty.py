"""npm launcher: first Enter, cancellable preparation and terminal cleanup.

Uses an isolated package containing the supplied local Rust binary and a mock API.
Run: python3 tests/tui_npm_pty.py [target/debug/takiza]
"""
import http.server
import json
from pathlib import Path
import shutil
import signal
import sys
import tempfile
import termios
import threading
from tui_queue_pty import Terminal, Screen

requests = []


class API(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        self.send_response(200)
        self.end_headers()
        if body.get('stream'):
            requests.append(body)
            self.wfile.write(b'data: {"choices":[{"delta":{"content":"NPM_RESPONSE_OK"}}]}\n\ndata: [DONE]\n\n')
        else:
            self.wfile.write(b'{"choices":[{"message":{"content":"npm test"}}]}')


def run(binary):
    with tempfile.TemporaryDirectory(prefix='takiza-npm-pty-') as directory:
        root = Path(directory)
        launcher = root / 'package/bin/takiza.cjs'
        launcher.parent.mkdir(parents=True)
        shutil.copy2(Path(__file__).resolve().parent.parent / 'npm/bin/takiza.cjs', launcher)
        for target in ['x86_64-unknown-linux-gnu', 'x86_64-unknown-linux-musl',
                       'aarch64-unknown-linux-gnu', 'x86_64-apple-darwin', 'aarch64-apple-darwin']:
            path = root / 'package/vendor' / target / 'takiza'
            path.parent.mkdir(parents=True)
            path.symlink_to(binary)

        def launch(name):
            workspace = root / name
            (workspace / '.takiza').mkdir(parents=True)
            (workspace / '.takiza/config.json').write_text(json.dumps({
                'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Monochrome',
                'mode': 'manual', 'model': 'test-model', 'api_key': 'test-only',
                'base_url': f'http://127.0.0.1:{server.server_port}/v1',
            }))
            terminal = Terminal(launcher, workspace, 80, 26, initial_prompt=None, controlling_tty=True)
            terminal.wait(lambda: 'You' in screen(terminal), 'initial input missing')
            return workspace, terminal

        def screen(terminal):
            return '\n'.join(Screen(terminal.output, 80, 26).text)

        def clean_exit(terminal, expected):
            terminal.wait(lambda: terminal.process.poll() is not None, 'launcher failed to exit')
            assert terminal.process.returncode == expected, terminal.process.returncode
            terminal.pump(.1)
            attrs = termios.tcgetattr(terminal.fd)
            assert attrs[3] & termios.ICANON and attrs[3] & termios.ECHO, 'shell left in raw mode'
            assert terminal.output.rfind(b'\x1b[?1006l') > terminal.output.rfind(b'\x1b[?1006h'), 'mouse capture left enabled'
            assert terminal.output.rfind(b'\x1b[?2004l') > terminal.output.rfind(b'\x1b[?2004h'), 'bracketed paste left enabled'
            assert terminal.output.rfind(b'\x1b[?1049l') > terminal.output.rfind(b'\x1b[?1049h'), 'alternate screen left enabled'

        workspace, terminal = launch('normal')
        try:
            terminal.send('first typed prompt\r')
            terminal.wait(lambda: 'NPM_RESPONSE_OK' in screen(terminal), 'first Enter hung')
            terminal.send('second typed prompt\r')
            terminal.wait(lambda: len(requests) == 2, 'second Enter hung')
            assert requests[0]['messages'][-1]['content'] == 'first typed prompt'
            assert requests[1]['messages'][-1]['content'] == 'second typed prompt'
            terminal.send('/exit\r')
            clean_exit(terminal, 0)
        finally:
            terminal.close()

        requests.clear()
        workspace, terminal = launch('large')
        try:
            # A sparse file costs almost no fixture disk space but prevents the
            # cancellable snapshot copy from finishing before input is exercised.
            with (workspace / 'large.bin').open('wb') as file:
                file.truncate(16 * 1024 ** 3)
            terminal.send('prepare a large workspace\r')
            terminal.send('editable while preparing')
            terminal.wait(lambda: 'editable while preparing' in screen(terminal), 'preparation blocked input', timeout=2)
            assert not requests, 'model request raced ahead of the snapshot'
            terminal.send('\x03')
            terminal.wait(lambda: 'Stopped by user' in screen(terminal), 'preparation could not be cancelled', timeout=2)
            assert not requests, 'cancelled preparation called the model'
            points = workspace / '.takiza/checkpoints'
            assert not points.exists() or not list(points.iterdir()), 'incomplete checkpoint remained'
            terminal.send('\x1b')  # Clear the draft before quitting.
            terminal.send('\x1b')
            terminal.send('/exit\r')
            clean_exit(terminal, 0)
        finally:
            terminal.close()

        for sig, code in [(signal.SIGINT, 130), (signal.SIGTERM, 143), (signal.SIGHUP, 129)]:
            _, terminal = launch(f'signal-{sig}')
            try:
                terminal.process.send_signal(sig)
                clean_exit(terminal, code)
            finally:
                terminal.close()
        print('PASS npm: idle first/second Enter, input/cancel during snapshot, no partial checkpoint, normal/SIGINT/SIGTERM/SIGHUP terminal cleanup')


if __name__ == '__main__':
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/takiza').resolve()
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), API)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        run(binary)
    finally:
        server.shutdown()
