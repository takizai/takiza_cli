"""Scrollback regression: python3 tests/tui_scroll_pty.py [binary]."""
import http.server
import json
from pathlib import Path
import sys
import tempfile
import threading
import time
from tui_queue_pty import Terminal, Screen

finish = threading.Event()
pulse = threading.Event()
record_count = 70
custom_content = None


class API(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        self.send_response(200)
        if not body.get('stream'):
            self.send_header('Content-Type', 'application/json')
            self.end_headers()
            self.wfile.write(b'{"choices":[{"message":{"content":"Scroll test"}}]}')
            return
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()

        def token(text):
            self.wfile.write(('data: ' + json.dumps({'choices': [{'delta': {'content': text}}]}) + '\n\n').encode())
            self.wfile.flush()

        try:
            token(custom_content if custom_content is not None else ''.join(f'RECORD {i:03d}\n' for i in range(record_count)))
            end = time.monotonic() + 20
            while not finish.wait(.02):
                assert time.monotonic() < end
                if pulse.is_set():
                    pulse.clear()
                    token('NEW_RECORD\n')
            token('ANSWER_COMPLETE\n')
            self.wfile.write(b'data: [DONE]\n\n')
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass


def run(binary, cols, rows):
    finish.clear()
    pulse.clear()
    with tempfile.TemporaryDirectory(prefix='takiza-scroll-') as directory:
        workspace = Path(directory)
        (workspace / '.takiza').mkdir()
        (workspace / '.takiza/config.json').write_text(json.dumps({
            'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Amber',
            'mode': 'manual', 'model': 'test-model', 'api_key': 'test-only',
            'base_url': f'http://127.0.0.1:{server.server_port}/v1',
        }))
        terminal = Terminal(binary, workspace, cols, rows)
        try:
            terminal.wait(lambda: b'RECORD 069' in terminal.output, 'initial stream missing')
            terminal.send('/t')
            terminal.send('\x1b[B')
            before_menu = Screen(terminal.output, cols, rows)
            menu_start = next(y for y, line in enumerate(before_menu.text) if '│ > /t' in line) - 1
            menu_rows = before_menu.text[menu_start:]
            packet_start = len(terminal.output)
            terminal.send('\x1b[5~')
            assert Screen(terminal.output, cols, rows).text[menu_start:] == menu_rows, 'scroll repainted or moved command menu'
            assert b'\x1b[2J' not in terminal.output[packet_start:], 'scroll cleared the whole screen'
            assert b'/tools' not in terminal.output[packet_start:], 'scroll redrew fixed menu rows'
            terminal.send('\x1b')
            terminal.send('\x15draft')
            terminal.send('\x1b[5~')  # PageUp during streaming
            screen = Screen(terminal.output, cols, rows)
            records = [line.strip() for line in screen.text if 'RECORD' in line]
            assert records and 'RECORD 069' not in '\n'.join(records), 'PageUp did not reveal older content'
            assert any('draft' in line for line in screen.text), 'scroll lost editable draft'
            pulse.set()
            terminal.pump(.35)
            updated = Screen(terminal.output, cols, rows)
            assert [line.strip() for line in updated.text if 'RECORD' in line] == records, 'incoming output moved the scroll position'
            terminal.send('\x1b[1;5F')  # Ctrl+End
            assert any('NEW_RECORD' in line for line in Screen(terminal.output, cols, rows).text), 'Ctrl+End did not restore latest output'
            # Mouse wheel must work after response completion too.
            finish.set()
            terminal.wait(lambda: b'ANSWER_COMPLETE' in terminal.output, 'response did not finish')
            terminal.pump(.25)
            terminal.send('\x1b[<64;10;5M')
            wheel_up = '\n'.join(Screen(terminal.output, cols, rows).text)
            assert 'ANSWER_COMPLETE' not in wheel_up, 'mouse wheel did not scroll completed history'
            terminal.send('\x1b[<65;10;5M')
            assert any('ANSWER_COMPLETE' in line for line in Screen(terminal.output, cols, rows).text), 'wheel down did not return to latest'
            terminal.send('\x1b[1;5H')  # Ctrl+Home
            assert any('RECORD 000' in line for line in Screen(terminal.output, cols, rows).text), 'Ctrl+Home did not reach the beginning'
            terminal.resize(max(25, cols - 10), max(10, rows - 4))
            terminal.send('\x1b[1;5F')
            resized = Screen(terminal.output, max(25, cols - 10), max(10, rows - 4))
            assert any('ANSWER_COMPLETE' in line for line in resized.text), 'resize lost latest output'
            terminal.send('\x7f' * len('draft') + '/exit\r')
            terminal.wait(lambda: terminal.process.poll() is not None, 'exit failed')
            assert terminal.output.count(b'\x1b[?1049h') == 1 and terminal.output.count(b'\x1b[?1049l') == 1
            print(f'PASS {cols}x{rows}: keyboard/mouse scroll, streaming anchor, draft, resize, one terminal screen')
        except AssertionError:
            print('\n'.join(Screen(terminal.output, cols, rows).text))
            raise
        finally:
            finish.set()
            terminal.close()


def short_chat(binary):
    global record_count
    record_count = 1
    finish.set()
    with tempfile.TemporaryDirectory(prefix='takiza-scroll-short-') as directory:
        workspace = Path(directory)
        (workspace / '.takiza').mkdir()
        (workspace / '.takiza/config.json').write_text(json.dumps({
            'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Amber',
            'mode': 'manual', 'model': 'test-model', 'api_key': 'test-only',
            'base_url': f'http://127.0.0.1:{server.server_port}/v1',
        }))
        terminal = Terminal(binary, workspace, 100, 34)
        try:
            terminal.wait(lambda: b'ANSWER_COMPLETE' in terminal.output, 'short response missing')
            terminal.pump(.25)
            before = Screen(terminal.output, 100, 34).text
            start = len(terminal.output)
            terminal.send('\x1b[<64;10;5M')
            after = Screen(terminal.output, 100, 34).text
            assert after == before, 'scroll changed layout of short chat/banner'
            assert b'\x1b[2J' not in terminal.output[start:], 'scroll cleared short chat'
            terminal.send('/exit\r')
            terminal.wait(lambda: terminal.process.poll() is not None, 'short chat exit failed')
            print('PASS short chat: unchanged banner, content, input and layout on wheel')
        except AssertionError:
            print('\n'.join(Screen(terminal.output, 100, 34).text))
            raise
        finally:
            terminal.close()
    record_count = 70


def markdown_scroll(binary, cancel=False):
    global custom_content
    custom_content = ('intro\n' * 60 + '```rust\n'
        + ''.join(f'let row_{i} = {i};\n' for i in range(8))
        + '```\n\n| Пакет | Цена | MoA |\n|---|---:|---:|\n|10M|490 ₽|270 ₽|\n\n## Summary\n**Markdown stable**\n')
    finish.clear()
    pulse.clear()
    with tempfile.TemporaryDirectory(prefix='takiza-markdown-scroll-') as directory:
        workspace = Path(directory)
        (workspace / '.takiza').mkdir()
        (workspace / '.takiza/config.json').write_text(json.dumps({
            'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Amber',
            'mode': 'manual', 'model': 'test-model', 'api_key': 'test-only',
            'base_url': f'http://127.0.0.1:{server.server_port}/v1',
        }))
        terminal = Terminal(binary, workspace, 80, 26)
        try:
            terminal.wait(lambda: b'Markdown stable' in terminal.output, 'Markdown stream missing')
            terminal.pump(.15)
            streamed = Screen(terminal.output, 80, 26).text
            assert any('[rust]' in line for line in streamed), streamed
            assert not any('```' in line for line in streamed), streamed
            assert any('┬' in line for line in streamed), streamed
            assert any('490 ₽' in line and '270 ₽' in line for line in streamed), streamed
            assert not any('|---' in line for line in streamed), streamed
            terminal.send('\x1b[5~\x1b[1;5F')
            terminal.pump(.2)
            assert Screen(terminal.output, 80, 26).text == streamed, 'stream changed after scrolling round trip'
            if not cancel:
                finish.set()
                terminal.wait(lambda: b'ANSWER_COMPLETE' in terminal.output, 'Markdown response did not finish')
                terminal.pump(.2)
            before = Screen(terminal.output, 80, 26).text
            packet_start = len(terminal.output)
            # A burst repeatedly hits both scroll boundaries. It must not replay
            # the answer, scroll the terminal screen, or starve cancellation.
            terminal.send(('\x1b[<64;10;5M' * 100 + '\x1b[<65;10;5M' * 100) * 2
                + '\x1b[1;5F' + (cancel if cancel else ''))
            if cancel:
                terminal.wait(lambda: b'Stopped by user' in terminal.output, 'scroll burst starved cancellation', timeout=5)
            else:
                terminal.pump(.5)
                after = Screen(terminal.output, 80, 26).text
                if after != before:
                    print('Changed rows:', [(i, a, b) for i, (a, b) in enumerate(zip(before, after)) if a != b])
                assert after == before, 'scroll burst changed final Markdown/layout'
            packet = terminal.output[packet_start:]
            assert b'\x1b[2J' not in packet and b'\x1b[1S' not in packet, 'scroll burst scrolled/cleared terminal'
            terminal.send('/exit\r')
            try:
                terminal.wait(lambda: terminal.process.poll() is not None, 'exit stalled after scroll burst')
            except AssertionError:
                Path('/tmp/takiza-scroll-burst-output').write_bytes(terminal.output)
                print('\n'.join(Screen(terminal.output, 80, 26).text))
                raise
            print(f'PASS Markdown: consistent code fences and layout, 400 wheel events, cancel={cancel!r}')
        finally:
            finish.set()
            terminal.close()
            custom_content = None


if __name__ == '__main__':
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else 'target/release/takiza').resolve()
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), API)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        run(binary, 80, 26)
        run(binary, 40, 16)
        short_chat(binary)
        markdown_scroll(binary)
        markdown_scroll(binary, cancel='\x1b')
        markdown_scroll(binary, cancel='\x03')
    finally:
        server.shutdown()
