"""Restore files and chat together, including a chat-only rollback and undo.

Run: python3 tests/tui_rewind_pty.py [binary]
"""
import http.server
import json
from pathlib import Path
import sys
import tempfile
import threading
import tui_tools_pty as tools
from tui_queue_pty import Terminal, Screen


def run_chat_only(binary):
    tools.TOOLS.clear()
    tools.step = 0
    with tempfile.TemporaryDirectory(prefix='takiza-chat-rewind-') as directory:
        workspace = Path(directory)
        (workspace / '.takiza').mkdir()
        (workspace / '.takiza/config.json').write_text(json.dumps({
            'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Amber',
            'mode': 'manual', 'model': 'test-model', 'api_key': 'test-only',
            'base_url': f'http://127.0.0.1:{tools.server.server_port}/v1',
        }))
        terminal = Terminal(binary, workspace, 100, 26)
        def screen():
            return '\n'.join(Screen(terminal.output, 100, 26).text)
        try:
            terminal.wait(lambda: 'Tool sequence finished.' in screen(), 'fixture answer missing')
            terminal.pump(.2)
            point = json.loads(next((workspace / '.takiza/checkpoints').glob('*.json')).read_text())
            session_path = workspace / '.takiza/sessions' / (point['session_id'] + '.json')
            terminal.send('/restore\r')
            terminal.wait(lambda: 'Restore workspace checkpoint' in screen(), 'chat-only menu missing')
            terminal.send('\r')
            terminal.wait(lambda: 'Review restore' in screen(), 'chat-only restore unavailable without file changes')
            terminal.send('\x1b[B\r')
            terminal.wait(lambda: 'Review restore' not in screen() and 'Tool sequence finished.' not in screen(), 'chat-only restore failed')
            assert 'Workspace restored' not in screen() and 'safety checkpoint' not in screen()
            assert 'Tool sequence finished.' not in screen(), 'chat-only restore left answer visible'
            assert '> first' in screen(), 'chat-only rewind did not restore editable prompt'
            session = json.loads(session_path.read_text())
            assert not any(item.get('type') == 'AssistantMessage' for item in session['history'])
            # Reloading the session must not bring the removed answer back.
            terminal.send(f'\x15/resume {point["session_id"]}\r')
            assert 'Tool sequence finished.' not in screen(), 'resume resurrected removed answer'
            terminal.send('/rewind\r')
            terminal.wait(lambda: 'Before restore' in screen(), 'chat safety snapshot missing')
            terminal.send('\r')
            terminal.wait(lambda: 'Review restore' in screen(), 'chat-only undo unavailable')
            terminal.send('\x1b[B\r')
            terminal.wait(lambda: 'Tool sequence finished.' in screen(), 'chat-only undo did not recover answer')
            print('PASS chat-only rewind: visible history, saved context, resume, safety undo')
        finally:
            terminal.close()


def run_edit_prompt(binary):
    tools.TOOLS.clear()
    tools.step = 0
    prompt = 'длинный исходный запрос ' * 8 + '\nпоследняя строка 🦀'
    with tempfile.TemporaryDirectory(prefix='takiza-edit-rewind-') as directory:
        workspace = Path(directory)
        (workspace / '.takiza').mkdir()
        (workspace / '.takiza/config.json').write_text(json.dumps({
            'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Amber',
            'mode': 'manual', 'model': 'test-model', 'api_key': 'test-only',
            'base_url': f'http://127.0.0.1:{tools.server.server_port}/v1',
        }))
        terminal = Terminal(binary, workspace, 100, 26, initial_prompt=prompt)
        def screen():
            return '\n'.join(Screen(terminal.output, 100, 26).text)
        try:
            terminal.wait(lambda: 'Tool sequence finished.' in screen(), 'initial reply missing')
            terminal.pump(.3)
            terminal.send('/rewind\r')
            terminal.wait(lambda: 'Restore workspace checkpoint' in screen(), 'rewind menu missing')
            terminal.send('\r')
            terminal.wait(lambda: 'Review restore' in screen(), 'rewind preview missing')
            terminal.send('\x1b[B\r')
            terminal.wait(lambda: 'Review restore' not in screen() and 'последняя строка 🦀' in screen(), 'multiline prompt missing from editor')
            previous = tools.step
            terminal.pump(.2)
            assert tools.step == previous, 'rewind automatically submitted the restored prompt'
            terminal.send(' исправлено\r')
            terminal.wait(lambda: tools.step > previous and 'Tool sequence finished.' in screen(), 'edited prompt did not run')
            points = [json.loads(path.read_text()) for path in (workspace / '.takiza/checkpoints').glob('*.json')]
            assert any(point.get('prompt') == prompt + ' исправлено' for point in points), 'prompt was truncated or cursor was not at the end'
            print('PASS rewind prompt editing: full Unicode/multiline text, caret, explicit resubmission')
        finally:
            terminal.close()


def run_gui_checkpoint(binary):
    """Import a GUI v1 checkpoint and exercise the actual CLI restore/undo path."""
    with tempfile.TemporaryDirectory(prefix='takiza-gui-checkpoint-') as directory:
        workspace = Path(directory)
        storage = workspace / '.takiza'
        (storage / 'sessions').mkdir(parents=True)
        (storage / 'config.json').write_text(json.dumps({
            'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Amber',
            'mode': 'manual', 'model': 'test-model', 'api_key': 'test-only',
            'base_url': f'http://127.0.0.1:{tools.server.server_port}/v1',
        }))
        session_id = '20261026_000000_000000000'
        checkpoint_id = '20261026_000000_000000001'
        def message(role, content):
            return {'role': role, 'content': content}
        def history(kind, text):
            return {'type': kind, 'payload': text}
        before = {
            'id': session_id, 'created_at': '2026-10-26 00:00:00', 'model': 'before-model',
            'title': 'Before title', 'messages': [message('user', 'earlier'), message('assistant', 'before answer')],
            'history': [history('UserPrompt', 'earlier'), history('AssistantMessage', 'before answer')],
            'context_tokens': 1000, 'gui_extra': {'value': 'before'},
        }
        prompt = 'later prompt\nextra line'
        current = {**before, 'model': 'after-model', 'title': 'After title', 'context_tokens': 9000,
                   'token_usage': {'total': 900, 'complete': True},
                   'gui_extra': {'value': 'after'}, 'future_extra': True,
                   'messages': before['messages'] + [message('user', prompt), message('assistant', 'later answer')],
                   'history': before['history'] + [history('UserPrompt', prompt), history('AssistantMessage', 'later answer')]}
        session_path = storage / 'sessions' / f'{session_id}.json'
        session_path.write_text(json.dumps(current))
        snapshot = storage / 'checkpoints' / checkpoint_id / 'files'
        snapshot.mkdir(parents=True)
        (snapshot / 'file.txt').write_text('before')
        (workspace / 'file.txt').write_text('after')
        (storage / 'checkpoints' / f'{checkpoint_id}.json').write_text(json.dumps({
            'format_version': 1, 'id': checkpoint_id, 'created_at': '2026-10-26 00:00:01',
            'label': 'later prompt', 'session_id': session_id, 'conversation': before,
            'entries': {'file.txt': {'File': {'mode': 420, 'readonly': False}}},
        }))
        terminal = Terminal(binary, workspace, 100, 30, initial_prompt=None, cli_args=['--session', session_id])
        def screen():
            return '\n'.join(Screen(terminal.output, 100, 30).text)
        def saved_is(title):
            return json.loads(session_path.read_text()).get('title') == title
        try:
            terminal.wait(lambda: 'later answer' in screen(), 'GUI session did not resume')
            terminal.send('/rewind\r')
            terminal.wait(lambda: 'Restore workspace checkpoint' in screen(), 'GUI checkpoint menu missing')
            terminal.send('\r')
            terminal.wait(lambda: 'Review restore' in screen(), 'GUI checkpoint preview missing')
            terminal.send('\x1b[B\r')
            terminal.wait(lambda: saved_is('Before title'), 'restore did not replace GUI session metadata')
            restored = json.loads(session_path.read_text())
            assert restored['model'] == 'before-model' and restored['context_tokens'] == 1000
            assert restored['gui_extra'] == {'value': 'before'} and 'future_extra' not in restored
            assert restored['token_usage'] == {'total': 900, 'complete': True}, 'rewind erased spent tokens'
            assert restored['started'] and len(restored['history']) == 2
            assert (workspace / 'file.txt').read_text() == 'before'
            terminal.wait(lambda: 'extra line' in screen(), 'GUI checkpoint did not recover full editable prompt')
            terminal.send('\x15/rewind\r')
            terminal.wait(lambda: 'Before restore' in screen(), 'GUI restore safety snapshot missing')
            terminal.send('\r')
            terminal.wait(lambda: 'Review restore' in screen(), 'GUI undo preview missing')
            terminal.send('\x1b[B\r')
            terminal.wait(lambda: saved_is('After title'), 'undo did not recover later session metadata')
            undone = json.loads(session_path.read_text())
            assert undone['context_tokens'] == 9000 and undone['gui_extra'] == {'value': 'after'}
            assert undone['future_extra'] and len(undone['history']) == 4
            assert undone['token_usage']['total'] == 900
            assert (workspace / 'file.txt').read_text() == 'after'
            print('PASS GUI checkpoint in CLI: v1 import, full session metadata/context, cumulative billing, prompt editing, safety undo')
        finally:
            terminal.close()


if __name__ == '__main__':
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/takiza').resolve()
    tools.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), tools.API)
    threading.Thread(target=tools.server.serve_forever, daemon=True).start()
    try:
        tools.run_rewind(binary)
        tools.run_rewind(binary, with_git=False)
        run_chat_only(binary)
        run_edit_prompt(binary)
        run_gui_checkpoint(binary)
    finally:
        tools.server.shutdown()
