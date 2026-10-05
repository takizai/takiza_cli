"""End-to-end busy-input regression: python3 tests/tui_queue_pty.py [binary].

Uses a local mock API and an isolated workspace; no real credentials or API calls.
"""
import fcntl
import http.server
import json
import os
from pathlib import Path
import pty
import re
import unicodedata
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time

requests = []
gates = [threading.Event() for _ in range(8)]
pulses = [threading.Event() for _ in range(8)]


class API(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        data = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        self.send_response(200)
        if not data.get('stream'):
            self.send_header('Content-Type', 'application/json')
            self.end_headers()
            self.wfile.write(json.dumps({'choices': [{'message': {'content': 'Queue test'}}]}).encode())
            return
        index = len(requests)
        requests.append(data)
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()
        try:
            chunk = {'choices': [{'delta': {'content': f'RESPONSE_{index} '}}]}
            self.wfile.write(('data: ' + json.dumps(chunk) + '\n\n').encode())
            self.wfile.flush()
            end = time.monotonic() + 15
            while not gates[index].wait(.02):
                assert time.monotonic() < end, 'mock response timed out'
                if pulses[index].is_set():
                    pulses[index].clear()
                    chunk = {'choices': [{'delta': {'content': 'STREAM_DURING_MENU **bold** `code` '}}]}
                    self.wfile.write(('data: ' + json.dumps(chunk) + '\n\n').encode())
                    self.wfile.flush()
            self.wfile.write(b'data: {"choices":[{"delta":{"content":"finished"},"finish_reason":"stop"}]}\n\ndata: [DONE]\n\n')
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass


class Screen:
    """Minimal VT screen for verifying dropdown placement alongside streamed output."""
    def __init__(self, data, cols, rows):
        self.lines = [[' '] * cols for _ in range(rows)]
        self.x = self.y = 0
        saved_cursor = (0, 0)
        for token in re.findall(r'\x1b\[[0-?]*[ -/]*[@-~]|\x1b[78]|[^\x1b]', data.decode('utf8', errors='replace')):
            if token == '\x1b7':
                saved_cursor = (self.x, self.y)
                continue
            if token == '\x1b8':
                self.x, self.y = saved_cursor
                continue
            if token.startswith('\x1b['):
                code = token[-1]
                params = token[2:-1]
                if params.startswith('?') or code == 'm':
                    continue
                numbers = [int(v or 0) for v in params.split(';')]
                n = numbers[0] or 1
                if code in ('H', 'f'):
                    self.y = min(rows - 1, n - 1)
                    self.x = min(cols - 1, (numbers[1] or 1) - 1 if len(numbers) > 1 else 0)
                elif code == 'G': self.x = min(cols - 1, n - 1)
                elif code == 'A': self.y = max(0, self.y - n)
                elif code == 'B': self.y = min(rows - 1, self.y + n)
                elif code == 'C': self.x = min(cols - 1, self.x + n)
                elif code == 'D': self.x = max(0, self.x - n)
                elif code == 'J':
                    if numbers[0] == 2: self.lines = [[' '] * cols for _ in range(rows)]
                    elif numbers[0] == 0:
                        self.lines[self.y][self.x:] = [' '] * (cols - self.x)
                        for y in range(self.y + 1, rows): self.lines[y] = [' '] * cols
                elif code == 'K':
                    start = 0 if numbers[0] == 2 else self.x
                    self.lines[self.y][start:] = [' '] * (cols - start)
                elif code == 'S':
                    self.lines = self.lines[min(n, rows):] + [[' '] * cols for _ in range(min(n, rows))]
            elif token == '\r': self.x = 0
            elif token == '\n':
                self.y += 1
                if self.y == rows:
                    self.lines.pop(0)
                    self.lines.append([' '] * cols)
                    self.y -= 1
            elif token >= ' ':
                width = 2 if unicodedata.east_asian_width(token) in ('W', 'F') else 1
                if unicodedata.combining(token): continue
                if self.x + width > cols:
                    self.x = 0
                    self.y = min(rows - 1, self.y + 1)
                self.lines[self.y][self.x] = token
                if width == 2: self.lines[self.y][self.x + 1] = ''
                self.x += width
        self.text = [''.join(line) for line in self.lines]


class Terminal:
    def __init__(self, binary, workspace, cols, rows, env_overrides=None, initial_prompt='first', controlling_tty=False):
        self.fd, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', rows, cols, 0, 0))
        env = os.environ.copy()
        for name in ['NO_COLOR', 'TAKIZA_PROXY', 'HTTP_PROXY', 'HTTPS_PROXY', 'ALL_PROXY', 'http_proxy', 'https_proxy', 'all_proxy']:
            env.pop(name, None)
        env['TAKIZA_AUTO_APPROVE'] = 'false'
        env.update(env_overrides or {})
        args = [str(binary)] + ([initial_prompt] if initial_prompt is not None else [])
        self.process = subprocess.Popen(args, cwd=workspace, stdin=slave, stdout=slave, stderr=slave, env=env,
            start_new_session=True, preexec_fn=(lambda: fcntl.ioctl(0, termios.TIOCSCTTY, 0)) if controlling_tty else None)
        os.close(slave)
        self.output = b''

    def pump(self, seconds=.05):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            readable, _, _ = select.select([self.fd], [], [], min(.02, end - time.monotonic()))
            if readable:
                try:
                    self.output += os.read(self.fd, 65536)
                except OSError:
                    break

    def wait(self, predicate, message, timeout=4):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            self.pump()
            if predicate():
                return
            if self.process.poll() is not None:
                # Exit can occur between the first predicate check and poll.
                if predicate():
                    return
                raise AssertionError(f'{message}: application exited {self.process.returncode}')
        raise AssertionError(message)

    def send(self, text):
        # Keep each escape sequence intact while bounding input bursts. Splitting
        # an escape across delayed writes can make terminals parse it as Esc.
        chunk = b''
        tokens = re.findall(r'\x1b\[[0-?]*[ -/]*[@-~]|.', text, re.DOTALL)
        for token in tokens:
            data = token.encode()
            if len(chunk) + len(data) > 512:
                os.write(self.fd, chunk)
                self.pump(.01)
                chunk = b''
            chunk += data
        if chunk:
            os.write(self.fd, chunk)
        self.pump(.15)

    def resize(self, cols, rows):
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack('HHHH', rows, cols, 0, 0))
        os.killpg(self.process.pid, signal.SIGWINCH)
        self.pump(.2)

    def close(self):
        if self.process.poll() is None:
            self.process.terminate()
            self.process.wait(timeout=3)
        os.close(self.fd)


def run(binary, cols, rows):
    requests.clear()
    for gate in gates:
        gate.clear()
    for pulse in pulses:
        pulse.clear()
    with tempfile.TemporaryDirectory(prefix='takiza-queue-') as directory:
        workspace = Path(directory)
        (workspace / '.takiza').mkdir()
        (workspace / '.takiza/config.json').write_text(json.dumps({
            'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Amber' if cols == 80 else 'Cyberpunk',
            'model': 'test-model', 'mode': 'manual', 'api_key': 'test-only',
            'base_url': f'http://127.0.0.1:{server.server_port}/v1',
        }))
        terminal = Terminal(binary, workspace, cols, rows)
        try:
            terminal.wait(lambda: len(requests) == 1, 'first request did not start')
            if cols >= 55:
                terminal.wait(lambda: 'Queue test' in '\n'.join(Screen(terminal.output, cols, rows).text), 'title missing while response is running')
            terminal.send('second\rthird\r')
            terminal.wait(lambda: b'2 queued' in terminal.output, 'queue count not visible')
            assert len(requests) == 1, 'queued requests started before the first response finished'
            # Open and navigate the dropdown while the first response is blocked.
            terminal.send('/t')
            terminal.wait(lambda: 'Switch visual theme' in '\n'.join(Screen(terminal.output, cols, rows).text) if cols >= 70 else '/theme' in '\n'.join(Screen(terminal.output, cols, rows).text), 'command dropdown unavailable while busy')
            terminal.send('\x1b[B')
            screen = Screen(terminal.output, cols, rows)
            assert any('> /tools' in line for line in screen.text), 'Down did not select /tools'
            # Resize with the dropdown open, then restore before streaming another chunk.
            terminal.resize(max(25, cols - 10), max(10, rows - 4))
            resized = Screen(terminal.output, max(25, cols - 10), max(10, rows - 4))
            assert any('> /tools' in line for line in resized.text), 'resize lost dropdown selection'
            terminal.resize(cols, rows)
            pulses[0].set()
            terminal.wait(lambda: b'STREAM_DURING_MENU' in terminal.output, 'stream did not continue while dropdown was open')
            screen = Screen(terminal.output, cols, rows)
            assert any('> /tools' in line for line in screen.text), 'stream overwrote the selected command'
            field_row = next(y for y, line in enumerate(screen.text) if '│ > /t' in line)
            assert screen.y == field_row, 'stream moved the caret out of the input field'
            assert all('STREAM_DURING_MENU' not in line for line in screen.text[field_row:]), 'response leaked into the input/menu'
            terminal.send('\t')
            screen = Screen(terminal.output, cols, rows)
            assert any('│ > /tools' in line for line in screen.text), 'Tab did not complete selected command'
            assert not any('commands)' in line for line in screen.text), 'Tab left stale dropdown rows'
            terminal.send('\x15/usa\r')
            terminal.wait(lambda: b'Token & Quota Usage' in terminal.output, '/usage was delayed by the active response')
            assert len(requests) == 1, '/usage interrupted or submitted a model request'
            terminal.send('\r')
            terminal.send('черновик')
            terminal.resize(max(25, cols - 10), max(10, rows - 4))
            gates[0].set()
            terminal.wait(lambda: len(requests) == 2, 'second prompt did not start')
            assert requests[1]['messages'][-1]['content'] == 'second'
            gates[1].set()
            terminal.wait(lambda: len(requests) == 3, 'third prompt did not start')
            assert requests[2]['messages'][-1]['content'] == 'third'
            assert any(str(m.get('content', '')).startswith('RESPONSE_0 STREAM_DURING_MENU') for m in requests[2]['messages']), 'previous response missing from conversation'
            gates[2].set()
            terminal.wait(lambda: b'RESPONSE_2 finished' in terminal.output, 'third response not rendered')
            terminal.pump(.25)
            terminal.send('\r')
            terminal.wait(lambda: len(requests) == 4, 'unfinished draft was lost after response completion')
            assert requests[3]['messages'][-1]['content'] == 'черновик'
            # A new session must apply immediately, without waiting on the mock gate.
            terminal.send('/clear\r')
            terminal.wait(lambda: b'Conversation reset' in terminal.output, '/clear blocked on the active agent lock')
            cleared = '\n'.join(Screen(terminal.output, max(25, cols - 10), max(10, rows - 4)).text)
            assert 'Queue test' not in cleared and 'черновик' not in cleared, '/clear kept previous title or conversation'
            terminal.send('new-session\r')
            terminal.wait(lambda: len(requests) == 5, 'new session could not submit a prompt')
            users = [m['content'] for m in requests[4]['messages'] if m['role'] == 'user']
            assert users == ['new-session'], f'old session leaked into new session: {users}'
            terminal.resize(cols, rows)
            terminal.send('/t')
            terminal.send('\x1b')
            screen = Screen(terminal.output, cols, rows)
            assert not any('> /theme' in line for line in screen.text), 'Esc failed to close the dropdown'
            assert b'Stopped by user' not in terminal.output, 'first Esc cancelled instead of dismissing menu'
            pulses[4].set()
            terminal.wait(lambda: 'STREAM_DURING_MENU' in '\n'.join(Screen(terminal.output, cols, rows).text), 'first Esc stopped the model response')
            terminal.send('\x15сохранённый черновик')
            terminal.send('\x1b')
            terminal.wait(lambda: b'Stopped by user' in terminal.output, 'Esc did not stop the active response')
            assert b'Error:' not in terminal.output and b'Interrupted' not in terminal.output, 'Esc rendered cancellation as an error'
            screen = Screen(terminal.output, cols, rows)
            assert any('сохранённый черновик' in line for line in screen.text), 'Esc lost the draft'
            terminal.send('\r')
            terminal.wait(lambda: len(requests) == 6, 'could not submit the saved draft after Esc')
            assert requests[5]['messages'][-1]['content'] == 'сохранённый черновик'
            stopped_before = terminal.output.count(b'Stopped by user')
            terminal.send('\x03')
            terminal.wait(lambda: terminal.output.count(b'Stopped by user') > stopped_before, 'Ctrl+C did not stop the response normally')
            assert b'Error:' not in terminal.output and b'Interrupted' not in terminal.output, 'Ctrl+C rendered cancellation as an error'
            sessions = list((workspace / '.takiza/sessions').glob('*.json'))
            for path in sessions:
                saved = json.loads(path.read_text())
                assert all('Error' not in item for item in saved.get('history', [])), 'cancelled response persisted an error'
            terminal.send('after-stop\r')
            terminal.wait(lambda: len(requests) == 7, 'agent unusable after cancellation')
            terminal.send('/exit\r')
            terminal.wait(lambda: terminal.process.poll() is not None, '/exit did not stop the busy agent')
            assert terminal.process.returncode == 0
            assert terminal.output.count(b'\x1b[?1049h') == 1, 'terminal screen recreated'
            assert terminal.output.count(b'\x1b[?1049l') == 1, 'original terminal not restored'
            assert '🤖'.encode() not in terminal.output, 'assistant robot icon still rendered'
            primary = b'\x1b[38;2;255;195;0m' if cols == 80 else b'\x1b[38;2;255;45;149m'
            code_background = b'\x1b[48;2;38;38;44m' if cols == 80 else b'\x1b[48;2;35;20;36m'
            assert primary + b'first' in terminal.output or primary + '❯ first'.encode() in terminal.output, 'user text does not follow theme'
            assert primary + b'\x1b[1mbold' in terminal.output, 'markdown bold does not follow theme'
            assert code_background + primary in terminal.output, 'markdown code does not follow theme'
            print(f'PASS {cols}x{rows}: busy dropdown, arrows/Tab/Enter, streaming caret, FIFO, /usage, resize/draft, /new, Esc/Ctrl+C without errors, /exit')
        finally:
            for gate in gates:
                gate.set()
            terminal.close()


def run_skills(binary, cols, rows):
    requests.clear()
    for gate in gates:
        gate.clear()
    with tempfile.TemporaryDirectory(prefix='takiza-skills-tui-') as directory:
        workspace = Path(directory)
        (workspace / '.takiza').mkdir()
        (workspace / '.takiza/config.json').write_text(json.dumps({
            'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Nord',
            'model': 'test-model', 'mode': 'manual', 'api_key': 'test-only',
            'base_url': f'http://127.0.0.1:{server.server_port}/v1',
        }))
        codex_home = workspace / 'global-test'
        def skill(root, folder, name, description):
            path = root / folder / 'SKILL.md'
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(f'---\nname: {name}\ndescription: {description}\n---\nFollow these test instructions.')
        skill(workspace / '.takiza/skills', 'nested/shared', 'shared-test-skill', 'Local priority description')
        skill(codex_home / 'skills', 'shared', 'shared-test-skill', 'Global shadow description')
        skill(codex_home / 'skills', 'nested/global', 'global-test-skill', 'Global available description')
        terminal = Terminal(binary, workspace, cols, rows, {'CODEX_HOME': str(codex_home)})
        try:
            terminal.wait(lambda: len(requests) == 1, 'first skills request missing')
            system = requests[0]['messages'][0]['content']
            assert 'Local priority description' in system and 'Global available description' in system
            assert 'Global shadow description' not in system, 'global skill overrode local skill'
            terminal.send('/skills\r')
            terminal.wait(lambda: b'Search:' in terminal.output, 'skills menu unavailable while busy')
            assert len(requests) == 1, '/skills became a model prompt'
            terminal.send('shared-test')
            screen = '\n'.join(Screen(terminal.output, cols, rows).text)
            assert '> local  shared-test-skill' in screen and 'Local priority description' in screen, screen
            lines = Screen(terminal.output, cols, rows).text
            assert lines[-1].startswith('╰') and any(line.startswith('╭') and 'Skills' in line for line in lines), screen
            assert any('Search:' in line for line in lines[rows // 2:]), 'skills panel was not at the bottom'
            terminal.resize(cols - 5, rows - 2)
            terminal.resize(cols, rows)
            assert 'shared-test-skill' in '\n'.join(Screen(terminal.output, cols, rows).text)
            skill(codex_home / 'skills', 'fresh', 'fresh-test-skill', 'Refreshed skill description')
            terminal.send('\x1b[15~' + '\x7f' * len('shared-test') + 'fresh-test')
            assert 'fresh-test-skill' in '\n'.join(Screen(terminal.output, cols, rows).text), 'F5 did not discover new skill'
            terminal.send('\r')
            screen = '\n'.join(Screen(terminal.output, cols, rows).text)
            assert '$fresh-test-skill' in screen, 'Enter did not insert selected skill in draft'
            assert len(requests) == 1, 'selecting skill submitted a model request'
            terminal.send('$global-test-skill next prompt\r')
            gates[0].set()
            terminal.wait(lambda: len(requests) == 2, 'queued prompt did not start')
            system = requests[1]['messages'][0]['content']
            assert 'Refreshed skill description' in system, 'new skill unavailable to model'
            requested = system.split('<requested_skills>')[1].split('</requested_skills>')[0]
            assert 'fresh-test-skill' in requested and 'global-test-skill' in requested, 'selected/typed references not resolved'
            assert requests[1]['messages'][-1]['content'] == '$fresh-test-skill $global-test-skill next prompt'
            gates[1].set()
            terminal.wait(lambda: b'RESPONSE_1' in terminal.output, 'second response missing')
            terminal.pump(.3)
            terminal.send('/skills\rshared-test\r')
            screen = '\n'.join(Screen(terminal.output, cols, rows).text)
            assert '$shared-test-skill' in screen, 'idle selection did not prefill prompt'
            assert len(requests) == 2, 'idle selection sent a prompt automatically'
            terminal.send('\x7f' * len('$shared-test-skill ') + '/exit\r')
            terminal.wait(lambda: terminal.process.poll() is not None, 'skills exit failed')
            print(f'PASS skills {cols}x{rows}: local/global, precedence, filter, F5, resize, busy command, model refresh')
        finally:
            for gate in gates:
                gate.set()
            terminal.close()


def run_skill_completions(binary, cols, rows):
    requests.clear()
    for gate in gates:
        gate.clear()
    for pulse in pulses:
        pulse.clear()
    with tempfile.TemporaryDirectory(prefix='takiza-skill-menu-') as directory:
        workspace = Path(directory)
        (workspace / '.takiza').mkdir()
        (workspace / '.takiza/config.json').write_text(json.dumps({
            'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Amber',
            'model': 'test-model', 'mode': 'manual', 'api_key': 'test-only',
            'base_url': f'http://127.0.0.1:{server.server_port}/v1',
        }))
        for suffix in 'abcdefg':
            path = workspace / '.takiza/skills' / f'demo-{suffix}' / 'SKILL.md'
            path.parent.mkdir(parents=True)
            path.write_text(f'---\nname: demo-{suffix}\ndescription: Demo skill {suffix}\n---\nInstructions')
        terminal = Terminal(binary, workspace, cols, rows)
        try:
            terminal.wait(lambda: b'RESPONSE_0' in terminal.output, 'completion initial stream missing')
            terminal.send('Привет $')
            screen = '\n'.join(Screen(terminal.output, cols, rows).text)
            assert '$demo-a' in screen and 'skills' in screen, 'dollar did not open inline skill menu'
            assert 'RESPONSE_0' in screen, 'skill menu hid all chat content'
            terminal.send('demo-')
            terminal.send('\x1b[B')
            assert '> $demo-b' in '\n'.join(Screen(terminal.output, cols, rows).text)
            terminal.resize(cols - 5, rows - 2)
            terminal.resize(cols, rows)
            assert '> $demo-b' in '\n'.join(Screen(terminal.output, cols, rows).text), 'resize lost skill selection'
            pulses[0].set()
            terminal.wait(lambda: b'STREAM_DURING_MENU' in terminal.output, 'stream stopped behind completion menu')
            screen = Screen(terminal.output, cols, rows)
            field_row = next(y for y, line in enumerate(screen.text) if '│ > Привет $demo-' in line)
            assert any('> $demo-b' in line for line in screen.text), 'stream lost selected skill'
            assert all('STREAM_DURING_MENU' not in line for line in screen.text[field_row:]), 'stream overwrote skill menu'
            packet_start = len(terminal.output)
            terminal.send('\r')
            packet = terminal.output[packet_start:]
            assert packet.count(b'\x1b[?2026h') == 1 and packet.count(b'\x1b[?2026l') == 1, 'busy selection painted intermediate frames'
            screen = '\n'.join(Screen(terminal.output, cols, rows).text)
            assert 'Привет $demo-b' in screen and 'skills)' not in screen
            assert len(requests) == 1, 'Enter on completion submitted prompt'
            terminal.send('и $demo-c\tзадача\r')
            gates[0].set()
            terminal.wait(lambda: len(requests) == 2, 'completed skill prompt did not reach model')
            assert requests[1]['messages'][-1]['content'] == 'Привет $demo-b и $demo-c задача'
            assert '<requested_skills>' in requests[1]['messages'][0]['content']
            gates[1].set()
            terminal.wait(lambda: b'RESPONSE_1' in terminal.output, 'completion response missing')
            terminal.pump(.3)
            terminal.send('добавь $demo-')
            screen = '\n'.join(Screen(terminal.output, cols, rows).text)
            assert '$demo-a' in screen and 'RESPONSE_1' in screen, 'idle inline completion hid chat'
            terminal.send('\x1b')
            assert len(requests) == 2, 'Esc submitted or interrupted idle completion'
            assert 'добавь $demo-' in '\n'.join(Screen(terminal.output, cols, rows).text), 'Esc lost draft'
            terminal.send('a')
            packet_start = len(terminal.output)
            terminal.send('\t')
            packet = terminal.output[packet_start:]
            assert packet.count(b'\x1b[?2026h') == 1 and packet.count(b'\x1b[?2026l') == 1, 'idle selection painted intermediate frames'
            assert 'добавь $demo-a' in '\n'.join(Screen(terminal.output, cols, rows).text), 'idle Tab did not insert skill'
            terminal.send('\x7f' * len('добавь $demo-a ') + '/exit\r')
            terminal.wait(lambda: terminal.process.poll() is not None, 'completion exit failed')
            print(f'PASS inline skills {cols}x{rows}: filter, arrows, Enter/Tab, chat visible, streaming, resize, Esc, Unicode draft')
        finally:
            for gate in gates:
                gate.set()
            terminal.close()


if __name__ == '__main__':
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/takiza').resolve()
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), API)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        run(binary, 80, 26)
        run(binary, 40, 16)
        run_skills(binary, 80, 26)
        run_skills(binary, 40, 16)
        run_skill_completions(binary, 80, 26)
        run_skill_completions(binary, 40, 16)
    finally:
        server.shutdown()
