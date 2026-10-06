"""Tool transcript regression: python3 tests/tui_tools_pty.py [binary]."""
import http.server
import json
import os
import re
import subprocess
from pathlib import Path
import sys
import tempfile
import threading
import time
from tui_queue_pty import Terminal, Screen

step = 0
empty_mode = None
empty_requests = []
title_attempts = 0
rewind_pause = False
rewind_waiting = threading.Event()
rewind_release = threading.Event()
TOOLS = [
    ('list_dir', {'path': '.'}),
    ('read_file', {'path': 'example.txt'}),
    ('run_command', {'command': "printf 'one\\ntwo\\nthree\\nfour\\nfive\\n'"}),
    ('read_file', {'path': 'missing.txt'}),
]


class API(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        global step, title_attempts
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        self.send_response(200)
        if not body.get('stream'):
            title_attempts += 1
            assert body['max_tokens'] >= 2048, 'reasoning budget too small for title'
            if empty_mode == 'long_reasoning':
                time.sleep(1)
            if empty_mode == 'untitled' or (empty_mode == 'title_retry' and title_attempts == 1):
                self.send_header('Content-Type', 'application/json')
                self.end_headers()
                self.wfile.write(b'{"choices":[{"message":{"content":""}}]}')
                return
            self.send_header('Content-Type', 'application/json')
            self.end_headers()
            self.wfile.write(b'{"choices":[{"message":{"content":"Tools test"}}]}')
            return
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()
        if empty_mode is not None:
            empty_requests.append(body)
            step += 1
            if empty_mode == 'eof':
                payload = ('data: ' + json.dumps({'choices': [{'delta': {'content': 'Ответ восстановлен'}, 'finish_reason': 'stop'}]}, ensure_ascii=False)).encode()
                split = payload.index('Ответ'.encode()) + 1
                self.wfile.write(payload[:split])
                self.wfile.flush()
                time.sleep(.02)
                self.wfile.write(payload[split:])
                self.wfile.flush()
                return
            if empty_mode in ['recover', 'reasoning'] and step > 1:
                delta = {'content': 'RECOVERED_RESPONSE'}
            elif empty_mode in ['untitled', 'title_retry']:
                time.sleep(.3)
                delta = {'content': 'RECOVERED_RESPONSE'}
            elif empty_mode == 'long_reasoning':
                delta = {'reasoning_content': '\n'.join(f'Thought line {i}' for i in range(1, 9)), 'content': 'RECOVERED_RESPONSE'}
            elif empty_mode == 'reasoning':
                delta = {'reasoning_content': 'Thinking without a final answer'}
            else:
                delta = {'content': '  '}
            chunk = {'choices': [{'delta': delta, 'finish_reason': 'stop'}]}
            self.wfile.write(('data: ' + json.dumps(chunk) + '\n\ndata: [DONE]\n\n').encode())
            self.wfile.flush()
            return
        if step < len(TOOLS):
            name, arguments = TOOLS[step]
            delta = {'tool_calls': [{'index': 0, 'id': f'call_{step}', 'type': 'function',
                'function': {'name': name, 'arguments': json.dumps(arguments)}}]}
            finish = 'tool_calls'
        else:
            if rewind_pause:
                rewind_waiting.set()
                rewind_release.wait(10)
            delta = {'content': 'Tool sequence finished.'}
            finish = 'stop'
        step += 1
        chunk = {'choices': [{'delta': delta, 'finish_reason': finish}]}
        self.wfile.write(('data: ' + json.dumps(chunk) + '\n\ndata: [DONE]\n\n').encode())
        self.wfile.flush()


def run(binary, cols, rows):
    global step
    step = 0
    with tempfile.TemporaryDirectory(prefix='takiza-tools-') as directory:
        workspace = Path(directory)
        (workspace / '.takiza').mkdir()
        (workspace / '.takiza/config.json').write_text(json.dumps({
            'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Nord', 'mode': 'manual',
            'model': 'test-model', 'api_key': 'test-only',
            'base_url': f'http://127.0.0.1:{server.server_port}/v1',
        }))
        (workspace / 'example.txt').write_text('first line\nsecond line\n')
        terminal = Terminal(binary, workspace, cols, rows)
        try:
            terminal.wait(lambda: b'Permission' in terminal.output, 'command approval did not appear', timeout=8)
            terminal.pump(.1)
            menu = '\n'.join(Screen(terminal.output, cols, rows).text)
            assert '> Allow once' in menu and 'Allow for session' in menu, menu
            assert '[y] Yes' not in menu
            menu_rows = [line for line in menu.splitlines() if any(label in line for label in ['[y]', '[n]', '[a]'])]
            assert len(menu_rows) == 3, menu
            assert len({line.index('[') for line in menu_rows}) == 1, menu
            terminal.send('\x1b[B')
            terminal.pump(.1)
            assert '> Deny' in '\n'.join(Screen(terminal.output, cols, rows).text)
            terminal.resize(cols - 1, rows)
            terminal.pump(.1)
            terminal.resize(cols, rows)
            terminal.pump(.1)
            assert '> Deny' in '\n'.join(Screen(terminal.output, cols, rows).text)
            terminal.send('\x1b[A')
            terminal.pump(.1)
            assert '> Allow once' in '\n'.join(Screen(terminal.output, cols, rows).text)
            terminal.send('\r' if cols == 100 else 'y')
            terminal.wait(lambda: b'Tool sequence finished.' in terminal.output, 'tool sequence did not finish')
            terminal.pump(.2)
            screen = Screen(terminal.output, cols, rows)
            text = '\n'.join(screen.text)
            assert text.count('example.txt') == 1, f'read header duplicated: {text}'
            assert text.count('printf') == 1, f'command repeated: {text}'
            assert 'approval allowed' in text
            assert 'one' in text and 'two' in text and 'three' in text
            assert '+2 lines' in text, f'collapsed count incorrect: {text}'
            assert 'failed' in text and 'Failed to read file' in text, f'error missing: {text}'
            assert '2 lines' in text
            assert all(icon not in text for icon in ['📂', '🔍', '✔', '✖', '⚠', '(done)', 'Bash :', 'Error:'])
            assert all(len(line) <= cols for line in screen.text)
            terminal.send('\x0f')
            terminal.pump(.2)
            expanded = '\n'.join(Screen(terminal.output, cols, rows).text)
            assert 'four' in expanded and 'five' in expanded, 'Ctrl+O did not reveal hidden output'
            terminal.send('/exit\r')
            terminal.wait(lambda: terminal.process.poll() is not None, 'exit failed')
            print(f'PASS {cols}x{rows}: single headers, approval, collapsed/expanded output, error detail, no emoji')
        except AssertionError:
            print('\n'.join(Screen(terminal.output, cols, rows).text))
            raise
        finally:
            terminal.close()


def run_failed_command(binary):
    global step
    step = 0
    original_tools = TOOLS[:]
    TOOLS[:] = [('run_command', {'command': "printf 'first_output\\nsecond_output\\nthird_output\\nHIDDEN_ERROR_DETAIL\\n'; exit 1"})]
    with tempfile.TemporaryDirectory(prefix='takiza-failed-command-') as directory:
        workspace = Path(directory)
        (workspace / '.takiza').mkdir()
        (workspace / '.takiza/config.json').write_text(json.dumps({
            'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Nord', 'mode': 'manual',
            'model': 'test-model', 'api_key': 'test-only',
            'base_url': f'http://127.0.0.1:{server.server_port}/v1',
        }))
        terminal = Terminal(binary, workspace, 60, 34)
        def text():
            return '\n'.join(Screen(terminal.output, 60, 34).text)
        try:
            terminal.wait(lambda: b'Permission' in terminal.output, 'approval missing')
            terminal.send('y')
            terminal.wait(lambda: b'Tool sequence finished.' in terminal.output, 'failed command stalled')
            terminal.pump(.2)
            assert 'failed' in text() and 'to expand' in text(), text()
            # The command header contains the marker too, so inspect output rows only.
            assert not any('HIDDEN_ERROR_DETAIL' in line for line in text().splitlines() if 'printf' not in line), text()
            terminal.send('\x0f')
            assert any(line.strip() == 'HIDDEN_ERROR_DETAIL' for line in text().splitlines()), text()
            assert sum(line.strip() == 'first_output' for line in text().splitlines()) == 1, text()
            terminal.send('\x0f')
            assert not any(line.strip() == 'HIDDEN_ERROR_DETAIL' for line in text().splitlines()), text()
            print('PASS failed command: collapsed error, Ctrl+O expand/collapse, no duplicated output')
        finally:
            terminal.close()
            TOOLS[:] = original_tools


def run_step_limit(binary, limit):
    global step
    step = 0
    original_tools = TOOLS[:]
    original_limit = os.environ.get('TAKIZA_MAX_STEPS')
    TOOLS[:] = [('read_file', {'path': 'example.txt'})] * 20
    if limit is None:
        os.environ.pop('TAKIZA_MAX_STEPS', None)
    else:
        os.environ['TAKIZA_MAX_STEPS'] = str(limit)
    try:
        with tempfile.TemporaryDirectory(prefix='takiza-steps-') as directory:
            workspace = Path(directory)
            (workspace / '.takiza').mkdir()
            (workspace / '.takiza/config.json').write_text(json.dumps({
                'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Nord', 'mode': 'manual',
                'model': 'test-model', 'api_key': 'test-only',
                'base_url': f'http://127.0.0.1:{server.server_port}/v1',
            }))
            (workspace / 'example.txt').write_text('step regression')
            # Local .env takes precedence over any fallback environment file.
            (workspace / '.env').write_text(f'TAKIZA_MAX_STEPS={100 if limit is None else limit}\n')
            terminal = Terminal(binary, workspace, 80, 26)
            try:
                expected = b'maximum step limit (2 steps)' if limit == 2 else b'Tool sequence finished.'
                terminal.wait(lambda: expected in terminal.output, 'step limit behavior incorrect', timeout=15)
                assert step == (2 if limit == 2 else 21), step
                terminal.send('/exit\r')
                terminal.wait(lambda: terminal.process.poll() is not None, 'exit failed')
                print(f'PASS step limit {limit}: {step} model requests')
            finally:
                terminal.close()
    finally:
        TOOLS[:] = original_tools
        if original_limit is None:
            os.environ.pop('TAKIZA_MAX_STEPS', None)
        else:
            os.environ['TAKIZA_MAX_STEPS'] = original_limit


def run_empty_response(binary, mode):
    global step, empty_mode, title_attempts
    step = 0
    title_attempts = 0
    empty_mode = mode
    empty_requests.clear()
    with tempfile.TemporaryDirectory(prefix='takiza-empty-') as directory:
        workspace = Path(directory)
        (workspace / '.takiza').mkdir()
        (workspace / '.takiza/config.json').write_text(json.dumps({
            'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Nord', 'mode': 'manual',
            'model': 'test-model', 'api_key': 'test-only',
            'base_url': f'http://127.0.0.1:{server.server_port}/v1',
        }))
        (workspace / '.env').write_text('TAKIZA_MAX_STEPS=1\n')
        terminal = Terminal(binary, workspace, 100, 26, {'TAKIZA_MAX_STEPS': '1'})
        try:
            if mode == 'cancel':
                terminal.wait(lambda: step >= 1, 'empty response did not start')
                terminal.send('\x1b')
                terminal.wait(lambda: b'Stopped by user' in terminal.output, 'empty retry ignored cancellation')
                assert step < 3, 'cancellation exhausted retries'
            else:
                expected = ('Ответ восстановлен'.encode() if mode == 'eof' else
                    b'no answer after 3 attempts' if mode == 'fail' else b'RECOVERED_RESPONSE')
                terminal.wait(lambda: expected in terminal.output, 'empty response handling failed', timeout=8)
                terminal.pump(.2)
                assert step == (3 if mode == 'fail' else 1 if mode in ['eof', 'long_reasoning', 'untitled', 'title_retry'] else 2), step
            if mode == 'long_reasoning':
                compact = '\n'.join(Screen(terminal.output, 100, 26).text)
                assert 'Reasoning' in compact and '│ Thought line 3' in compact, compact
                assert 'Thought line 8' not in compact and '+5 reasoning lines' in compact, compact
                terminal.send('\x0f')
                terminal.pump(.2)
                expanded = '\n'.join(Screen(terminal.output, 100, 26).text)
                assert '│ Thought line 8' in expanded, expanded
                terminal.send('\x0f')
                terminal.pump(.2)
                compact = '\n'.join(Screen(terminal.output, 100, 26).text)
                assert 'Thought line 8' not in compact, compact
                terminal.wait(lambda: 'Tools test' in '\n'.join(Screen(terminal.output, 100, 26).text), 'late title missing in idle chat')
            if mode in ['long_reasoning', 'title_retry', 'untitled']:
                if mode == 'untitled':
                    terminal.wait(lambda: b'Chat title unavailable:' in terminal.output, 'title failure was silent')
                else:
                    terminal.wait(lambda: 'Tools test' in '\n'.join(Screen(terminal.output, 100, 26).text), 'generated title did not appear')
                plain_output = re.sub(rb'\x1b\[[0-?]*[ -/]*[@-~]', b'', terminal.output)
                assert '💬 first'.encode() not in plain_output, 'first prompt used as title'
                if mode == 'untitled':
                    assert '💬'.encode() not in plain_output, 'empty title replaced with fabricated title'
                    assert title_attempts == 2, title_attempts
                else:
                    if mode == 'long_reasoning':
                        assert plain_output.index(b'RECOVERED_RESPONSE') < plain_output.index('💬 Tools test'.encode()), 'response waited for title generation'
                    assert title_attempts == (2 if mode == 'title_retry' else 1), title_attempts
            assert b'(Empty response)' not in terminal.output
            assert '�'.encode() not in terminal.output, 'split UTF-8 corrupted'
            assert b'maximum step limit' not in terminal.output, 'retry consumed step limit'
            assert all(request['messages'] == empty_requests[0]['messages'] for request in empty_requests), 'retry polluted conversation'
            if mode == 'untitled':
                terminal.send('second prompt\r')
                terminal.wait(lambda: step == 2, 'second prompt did not start')
                terminal.pump(.5)
                assert title_attempts == 2, 'title generation repeated on subsequent prompt'
            terminal.send('/exit\r')
            terminal.wait(lambda: terminal.process.poll() is not None, 'empty test exit failed')
            for path in (workspace / '.takiza/sessions').glob('*.json'):
                session = json.loads(path.read_text())
                assistants = [message for message in session['messages'] if message['role'] == 'assistant']
                assert all(message.get('content', '').strip() for message in assistants), 'empty assistant saved'
                if mode in ['fail', 'cancel']:
                    assert not assistants, 'failed response saved as assistant'
            print(f'PASS empty response {mode}: {step} requests, no placeholder/history pollution')
        finally:
            terminal.close()
            empty_mode = None


def run_rewind(binary, with_git=True):
    global step, rewind_pause
    original_tools = TOOLS[:]
    TOOLS[:] = [('write_file', {'path': 'example.txt', 'content': 'agent edit'}),
                ('write_file', {'path': 'agent-created.txt', 'content': 'agent new file'})]
    step = 0
    rewind_pause = not with_git
    rewind_waiting.clear()
    rewind_release.clear()
    with tempfile.TemporaryDirectory(prefix='takiza-rewind-') as directory:
        workspace = Path(directory)
        def git(*args):
            return subprocess.run(['git', *args], cwd=workspace, check=True, capture_output=True)
        if with_git:
            git('init', '-q')
            git('config', 'user.name', 'Test')
            git('config', 'user.email', 'test@example.invalid')
            (workspace / '.gitignore').write_text('.takiza/\n')
            (workspace / 'example.txt').write_text('committed baseline')
            git('add', '.')
            git('commit', '-qm', 'baseline')
            (workspace / 'example.txt').write_text('user staged')
            git('add', 'example.txt')
        (workspace / 'example.txt').write_text('user unstaged')
        (workspace / '.takiza').mkdir()
        (workspace / '.takiza/config.json').write_text(json.dumps({
            'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Nord', 'mode': 'manual',
            'model': 'test-model', 'api_key': 'test-only',
            'base_url': f'http://127.0.0.1:{server.server_port}/v1',
        }))
        empty_path = workspace / '.takiza/empty-bin'
        empty_path.mkdir()
        terminal = Terminal(binary, workspace, 100, 26, {} if with_git else {'PATH': str(empty_path)})
        try:
            if with_git:
                terminal.wait(lambda: b'Tool sequence finished.' in terminal.output, 'rewind fixture response failed')
                terminal.pump(.2)
            else:
                terminal.wait(rewind_waiting.is_set, 'rewind fixture did not block active response')
                terminal.send('queued prompt\r')
                terminal.wait(lambda: b'1 queued' in terminal.output, 'busy rewind fixture did not queue prompt')
            assert (workspace / 'example.txt').read_text() == 'agent edit'
            assert (workspace / 'agent-created.txt').exists()
            terminal.send('/rewind\r')
            terminal.wait(lambda: 'Restore workspace checkpoint' in '\n'.join(Screen(terminal.output, 100, 26).text), 'rewind menu missing')
            lines = Screen(terminal.output, 100, 26).text
            assert lines[-1].startswith('╰') and any(line.startswith('╭') and 'Restore workspace' in line for line in lines[-6:]), '\n'.join(lines)
            terminal.send('\r')
            terminal.wait(lambda: 'Review restore' in '\n'.join(Screen(terminal.output, 100, 26).text), 'restore preview missing')
            screen = '\n'.join(Screen(terminal.output, 100, 26).text)
            assert 'example.txt' in screen and 'agent-created.txt' in screen, screen
            assert Screen(terminal.output, 100, 26).text[-1].startswith('╰'), 'review was not framed at bottom'
            # Enter defaults to cancel, so merely opening a checkpoint cannot change files.
            terminal.send('\r')
            terminal.pump(.2)
            assert (workspace / 'example.txt').read_text() == 'agent edit'
            terminal.send('/restore\r')
            terminal.wait(lambda: 'Restore workspace checkpoint' in '\n'.join(Screen(terminal.output, 100, 26).text), 'restore alias missing')
            terminal.send('\r')
            terminal.wait(lambda: 'Review restore' in '\n'.join(Screen(terminal.output, 100, 26).text), 'second preview missing')
            terminal.send('\x1b[B\r')
            terminal.wait(lambda: (workspace / 'example.txt').read_text() == 'user unstaged' and not (workspace / 'agent-created.txt').exists(), 'restore did not complete')
            assert (workspace / 'example.txt').read_text() == 'user unstaged'
            assert not (workspace / 'agent-created.txt').exists()
            terminal.pump(.2)
            restored_screen = '\n'.join(Screen(terminal.output, 100, 26).text)
            assert 'Workspace restored' not in restored_screen and 'safety checkpoint' not in restored_screen, 'restore status polluted chat'
            assert 'Tool sequence finished.' not in restored_screen, 'rewind left the later answer visible'
            assert 'agent-created.txt' not in restored_screen, 'rewind left later tool results visible'
            point = json.loads(next((workspace / '.takiza/checkpoints').glob('*.json')).read_text())
            session_path = workspace / '.takiza/sessions' / (point['session_id'] + '.json')
            restored_session = json.loads(session_path.read_text())
            assert all(not any(key in item for key in ['AssistantMessage', 'Thought', 'ToolStart', 'ToolEnd']) for item in restored_session['history']), 'rewind did not save truncated history'
            assert all(message['role'] == 'system' for message in restored_session['messages']), 'rewind kept obsolete model context'
            assert '> first' in restored_screen, 'rewind did not put original prompt in the editor'
            if with_git:
                assert git('show', ':example.txt').stdout == b'user staged'
            # The newest point is the automatic backup; restoring it undoes the rewind.
            terminal.send('\x15/rewind\r')
            terminal.wait(lambda: 'Before restore' in '\n'.join(Screen(terminal.output, 100, 26).text), 'safety checkpoint missing')
            terminal.send('\r')
            terminal.wait(lambda: 'Review restore' in '\n'.join(Screen(terminal.output, 100, 26).text), 'undo preview missing')
            terminal.send('\x1b[B\r')
            terminal.wait(lambda: (workspace / 'agent-created.txt').exists(), 'rewind undo failed')
            assert (workspace / 'example.txt').read_text() == 'agent edit'
            if with_git:
                terminal.wait(lambda: 'Tool sequence finished.' in '\n'.join(Screen(terminal.output, 100, 26).text), 'undo did not restore chat answer')
            if not with_git:
                assert step == 2, 'queued prompt ran after rewind'
            if with_git:
                saved = list((workspace / '.takiza/checkpoints').glob('*.json'))
                original_chat = json.loads(saved[0].read_text())['session_id']
                assert len(saved) >= 3, 'multiple checkpoints were not retained in one chat'
                terminal.send('/new\r')
                terminal.pump(.2)
                terminal.send('/restore\r')
                terminal.wait(lambda: 'No checkpoints in this chat.' in '\n'.join(Screen(terminal.output, 100, 26).text), 'empty chat menu missing')
                empty = Screen(terminal.output, 100, 26).text
                assert empty[-1].startswith('╰') and 'Esc Back' in empty[-1], '\n'.join(empty)
                terminal.send('\r\x1b[B')
                assert 'No checkpoints in this chat.' in '\n'.join(Screen(terminal.output, 100, 26).text), 'Enter selected nonexistent checkpoint'
                terminal.resize(40, 16)
                terminal.resize(100, 26)
                terminal.send('\x1b')
                after = '\n'.join(Screen(terminal.output, 100, 26).text)
                assert 'No checkpoints in this chat.' not in after and 'Restore unavailable:' not in after, 'empty menu polluted chat history'
                assert len(list((workspace / '.takiza/checkpoints').glob('*.json'))) == len(saved), 'empty chat created checkpoints'
                previous_step = step
                terminal.send('fresh scope prompt\r')
                terminal.wait(lambda: step > previous_step, 'new chat prompt did not run')
                terminal.pump(.3)
                terminal.send('/rewind\r')
                terminal.wait(lambda: 'Restore workspace checkpoint' in '\n'.join(Screen(terminal.output, 100, 26).text), 'new chat checkpoint missing')
                screen = '\n'.join(Screen(terminal.output, 100, 26).text)
                assert 'fresh scope prompt' in screen and 'Before restore' not in screen, screen
                terminal.send('\x1b')
                terminal.send(f'/resume {original_chat}\r')
                terminal.pump(.2)
                terminal.send('/rewind\r')
                terminal.wait(lambda: 'Before restore' in '\n'.join(Screen(terminal.output, 100, 26).text), 'resumed chat lost its checkpoints')
                assert 'fresh scope prompt' not in '\n'.join(Screen(terminal.output, 100, 26).text), 'resumed chat exposed new chat checkpoint'
                terminal.send('\x1b')
            terminal.send('/exit\r')
            terminal.wait(lambda: terminal.process.poll() is not None, 'rewind exit failed')
            print(f'PASS rewind git={with_git}: preview/cancel, alias, dirty baseline, created files, safety backup/undo')
        except Exception:
            print('Rewind terminal state:\n' + '\n'.join(Screen(terminal.output, 100, 26).text))
            print('Checkpoint files:', [path.name for path in (workspace / '.takiza/checkpoints').glob('*')])
            raise
        finally:
            rewind_release.set()
            rewind_pause = False
            terminal.close()
            TOOLS[:] = original_tools


if __name__ == '__main__':
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else 'target/release/takiza').resolve()
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), API)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        run(binary, 100, 34)
        run(binary, 40, 24)
        run_failed_command(binary)
        run_rewind(binary)
        run_rewind(binary, with_git=False)
        for limit in [None, 2, 0]:
            run_step_limit(binary, limit)
        for mode in ['recover', 'reasoning', 'eof', 'fail', 'cancel', 'long_reasoning', 'untitled', 'title_retry']:
            run_empty_response(binary, mode)
    finally:
        server.shutdown()
