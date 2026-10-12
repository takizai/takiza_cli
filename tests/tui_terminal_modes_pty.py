"""Mouse reports must never be echoed during TUI transitions or leak on exit."""
import http.server
import json
import os
from pathlib import Path
import sys
import tempfile
import termios
import threading
import time
from tui_queue_pty import Terminal, Screen

release = threading.Event()


class API(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        self.send_response(200)
        self.end_headers()
        if not body.get('stream'):
            self.wfile.write(b'{"choices":[{"message":{"content":"Terminal test"}}]}')
            return
        try:
            self.wfile.write(b'data: {"choices":[{"delta":{"content":"STREAM_READY"}}]}\n\n')
            self.wfile.flush()
            release.wait(10)
            self.wfile.write(b'data: {"choices":[{"delta":{"content":" STREAM_DONE"}}]}\n\n')
            self.wfile.write(b'data: {"choices":[],"usage":{"total_tokens":10}}\n\n')
            self.wfile.write(b'data: [DONE]\n\n')
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass


def screen(terminal):
    return '\n'.join(Screen(terminal.output, 100, 30).text)


def raw(terminal):
    flags = termios.tcgetattr(terminal.fd)[3]
    assert not flags & (termios.ECHO | termios.ICANON | termios.ISIG), \
        f'TUI released raw mode while mouse capture was active: {flags:#x}'


def mouse_burst(terminal, duration=.3):
    end = time.monotonic() + duration
    while time.monotonic() < end:
        raw(terminal)
        os.write(terminal.fd, b'\x1b[<35;41;11M')
        terminal.pump(.01)
    raw(terminal)
    assert b'^[[<35;' not in terminal.output, 'terminal echoed a mouse report'
    assert '[<35;41;11M' not in screen(terminal), 'mouse report entered the transcript or editor'


if __name__ == '__main__':
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/takiza').resolve()
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), API)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix='takiza-terminal-modes-') as directory:
            workspace = Path(directory)
            (workspace / '.takiza').mkdir()
            (workspace / '.takiza/config.json').write_text(json.dumps({
                'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Amber',
                'mode': 'manual', 'model': 'test-model', 'api_key': 'test-only',
                'base_url': f'http://127.0.0.1:{server.server_port}/v1',
            }))
            terminal = Terminal(binary, workspace, 100, 30, initial_prompt=None)
            try:
                terminal.wait(lambda: 'You' in screen(terminal), 'editor missing')
                mouse_burst(terminal)
                os.write(terminal.fd, b'check terminal\r')
                mouse_burst(terminal)
                terminal.wait(lambda: 'STREAM_READY' in screen(terminal), 'stream missing')
                mouse_burst(terminal)
                terminal.send('/help\r')
                mouse_burst(terminal)
                release.set()
                terminal.wait(lambda: '10 tokens' in screen(terminal), 'completion missing')
                mouse_burst(terminal)
                os.write(terminal.fd, b'!sleep 10\r')
                mouse_burst(terminal)
                terminal.wait(lambda: 'Bash' in screen(terminal), 'direct command missing')
                terminal.send('\x03')
                terminal.wait(lambda: 'cancelled' in screen(terminal) or 'interrupted' in screen(terminal),
                              'Ctrl+C did not cancel direct command', timeout=5)
                mouse_burst(terminal)
                # Reports arriving together with exit must not be echoed or
                # remain in the kernel input queue after returning to the shell.
                exit_offset = len(terminal.output)
                late_report_sent = threading.Event()

                def delayed_mouse_report():
                    deadline = time.monotonic() + 3
                    while b'\x1b[?1003l' not in terminal.output[exit_offset:]:
                        if time.monotonic() >= deadline:
                            return
                        time.sleep(.001)
                    # Emulate reports already in flight when the terminal
                    # receives DisableMouseCapture, including a split packet.
                    time.sleep(.015)
                    os.write(terminal.fd, b'\x1b[<51;51;')
                    time.sleep(.01)
                    os.write(terminal.fd, b'14M')
                    late_report_sent.set()

                late_mouse = threading.Thread(target=delayed_mouse_report)
                late_mouse.start()
                os.write(terminal.fd, b'\x03' + b'\x1b[<35;41;11M' * 100)
                terminal.wait(lambda: terminal.process.poll() is not None, 'Ctrl+C did not exit', timeout=5)
                late_mouse.join(timeout=3)
                terminal.pump(.1)
                assert late_report_sent.is_set(), 'late mouse report was not injected'
                assert terminal.process.returncode == 0
                assert b'^[[<35;' not in terminal.output
                assert b'^[[<51;' not in terminal.output, 'in-flight mouse report was echoed after exit'
                flags = termios.tcgetattr(terminal.fd)[3]
                assert flags & termios.ECHO and flags & termios.ICANON and flags & termios.ISIG, \
                    'terminal modes were not restored'
                output = terminal.output
                assert output.rfind(b'\x1b[?1003l') < output.rfind(b'Goodbye!')
                for session in (workspace / '.takiza/sessions').glob('*.json'):
                    assert '[<35;41;11M' not in session.read_text(), 'mouse report persisted in chat'
            finally:
                terminal.close()
        print('PASS terminal modes: idle, preparation, streaming, slash command, completion, shell cancellation, exit')
    finally:
        release.set()
        server.shutdown()
