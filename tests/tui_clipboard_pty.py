"""Mouse selection, right-click and bracketed paste using an isolated clipboard."""
import http.server
import json
import os
from pathlib import Path
import sys
import tempfile
import threading
import time
from tui_queue_pty import Terminal, Screen

finish = threading.Event()
pulse = threading.Event()
requests = []


class API(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        self.send_response(200)
        if not body.get('stream'):
            self.send_header('Content-Type', 'application/json')
            self.end_headers()
            self.wfile.write(b'{"choices":[{"message":{"content":"Clipboard test"}}]}')
            return
        requests.append(body)
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()
        def token(text):
            self.wfile.write(('data: ' + json.dumps({'choices': [{'delta': {'content': text}}]}) + '\n\n').encode())
            self.wfile.flush()
        try:
            token('COPY_ME Привет\nNEXT_LINE\n')
            end = time.monotonic() + 20
            while not finish.wait(.02):
                assert time.monotonic() < end
                if pulse.is_set():
                    pulse.clear()
                    token('\n' + 'streamed line\n' * 40 + 'NEW_OUTPUT\n')
            token('COMPLETE')
            self.wfile.write(b'data: [DONE]\n\n')
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass


def run(binary):
    with tempfile.TemporaryDirectory(prefix='takiza-clipboard-') as directory:
        workspace = Path(directory)
        (workspace / '.takiza').mkdir()
        (workspace / '.takiza/config.json').write_text(json.dumps({
            'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Monochrome',
            'mode': 'manual', 'model': 'test-model', 'api_key': 'test-only',
            'base_url': f'http://127.0.0.1:{server.server_port}/v1',
        }))
        tools = workspace / 'test-bin'
        tools.mkdir()
        clipboard = workspace / 'clipboard.txt'
        for name, command in [('wl-copy', 'cat > "$TAKIZA_TEST_CLIPBOARD"'),
                              ('wl-paste', 'cat "$TAKIZA_TEST_CLIPBOARD"')]:
            path = tools / name
            path.write_text('#!/bin/sh\n' + command + '\n')
            path.chmod(0o755)
        terminal = Terminal(binary, workspace, 80, 26, {
            'PATH': str(tools) + os.pathsep + os.environ['PATH'],
            'TAKIZA_TEST_CLIPBOARD': str(clipboard),
        })
        def screen():
            return Screen(terminal.output, 80, 26).text
        def mouse(button, x, y, release=False):
            terminal.send(f'\x1b[<{button};{x + 1};{y + 1}{"m" if release else "M"}')
        def locate(text):
            y = next(y for y, line in enumerate(screen()) if text in line)
            return screen()[y].index(text), y
        def select(text, reverse=False):
            x, y = locate(text)
            first, last = (x + len(text) - 1, x) if reverse else (x, x + len(text) - 1)
            mouse(0, first, y)
            mouse(32, last, y)
            mouse(0, last, y, True)
            terminal.wait(lambda: clipboard.exists() and clipboard.read_text() == text, 'selection did not copy exact text')
        def gui_editing():
            terminal.send('\x15\x1b[200~alpha beta\nz\nalpha beta\x1b[201~')
            terminal.send('\x1b[A\x1b[A\x1b[1;5DTOP_\x1b[HB:\x1b[F:END')
            assert 'B:alpha TOP_beta:END' in '\n'.join(screen()), 'vertical movement or line Home/End changed the wrong line'
            x, y = locate('TOP_')
            mouse(0, x, y)
            mouse(0, x, y, True)
            terminal.send('CLICK_\x1b[1;2C\x1b[1;2C\x1b[1;2Cnew')
            assert 'B:alpha CLICK_new_beta:END' in '\n'.join(screen()), 'mouse caret or Shift selection replacement failed'
            x, y = locate('CLICK_')
            mouse(0, x, y)
            mouse(32, x + len('CLICK_'), y)
            mouse(0, x + len('CLICK_'), y, True)
            assert clipboard.read_text() == 'CLICK_', 'input drag did not copy selection'
            terminal.send('\x18')
            assert 'B:alpha new_beta:END' in '\n'.join(screen()), 'Ctrl+X did not cut the selected input'
            terminal.send('\x16')
            assert 'B:alpha CLICK_new_beta:END' in '\n'.join(screen()), 'Ctrl+V did not paste at the cut position'
            terminal.send('\x01\x03')
            assert clipboard.read_text() == 'B:alpha CLICK_new_beta:END\nz\nalpha beta', 'Ctrl+A/C did not copy the multiline draft'
            assert b'Stopped by user' not in terminal.output, 'copying selected input stopped the model'
            terminal.send('replace\x1b\rsecond')  # Alt+Enter inserts a real newline.
            lines = screen()
            assert any('replace' in line for line in lines) and any('second' in line for line in lines), 'select-all replacement or Alt+Enter failed'
            assert len(requests) == 1, 'editing submitted a request'
        try:
            terminal.wait(lambda: any('COPY_ME' in line for line in screen()), 'response missing')
            # Incoming tokens must not move the text while dragging.
            x, y = locate('COPY_ME')
            mouse(0, x, y)
            mouse(32, x + 6, y)
            assert b'\x1b[7mCOPY_ME' in terminal.output, 'selection not highlighted'
            pulse.set()
            terminal.pump(.3)
            assert 'COPY_ME' in screen()[y], 'stream moved text under selection'
            mouse(0, x + 6, y, True)
            terminal.wait(lambda: clipboard.exists() and clipboard.read_text() == 'COPY_ME', 'busy selection not copied')
            terminal.send('ab\x1b[D')
            mouse(2, 5, 24)
            assert 'aCOPY_MEb' in '\n'.join(screen()), 'right-click did not paste at caret while busy'
            terminal.send('\x15')
            clipboard.write_text('one\n/two')
            mouse(2, 5, 24)
            assert any('one' in line for line in screen()) and any('/two' in line for line in screen()), 'multiline clipboard paste corrupted the editor'
            assert len(requests) == 1, 'paste submitted a prompt or command'
            terminal.send('\x15\x1b[200~line one\nline two\x1b[201~')
            assert any('line one' in line for line in screen()) and any('line two' in line for line in screen()), 'bracketed paste missing while busy'
            gui_editing()
            terminal.send('\x15\x1b[200~line one\nline two\x1b[201~')
            finish.set()
            terminal.wait(lambda: any('COMPLETE' in line for line in screen()), 'response did not finish')
            terminal.pump(.3)
            assert any('line one' in line for line in screen()) and any('line two' in line for line in screen()), 'draft lost at response boundary'
            terminal.send('\x15\x1b[1;5H')
            select('Привет', reverse=True)
            x, y = locate('COPY_ME')
            last_x, last_y = locate('NEXT_LINE')
            mouse(0, x, y)
            mouse(32, last_x + len('NEXT_LINE') - 1, last_y)
            mouse(0, last_x + len('NEXT_LINE') - 1, last_y, True)
            expected = 'COPY_ME Привет\n  NEXT_LINE'
            terminal.wait(lambda: clipboard.read_text() == expected, 'multiline selection missing')
            mouse(0, x, y)
            mouse(0, x, y, True)
            assert clipboard.read_text() == expected, 'single click overwrote clipboard'
            clipboard.write_text('IDLE_PASTE')
            mouse(2, 5, 24)
            assert 'IDLE_PASTE' in '\n'.join(screen()), 'right-click paste missing while idle'
            terminal.send('\x15\x1b[200~idle\npaste\x1b[201~')
            assert any('idle' in line for line in screen()) and any('paste' in line for line in screen()), 'bracketed paste missing while idle'
            gui_editing()
            assert len(requests) == 1
            terminal.send('\x15/exit\r')
            terminal.wait(lambda: terminal.process.poll() is not None, 'exit failed')
            assert terminal.output.count(b'\x1b[?2004h') == 1 and terminal.output.count(b'\x1b[?2004l') == 1
            print('PASS clipboard: selection copies, stream anchor, Unicode/reverse drag, right-click at caret, multiline and bracketed paste, busy/idle')
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
