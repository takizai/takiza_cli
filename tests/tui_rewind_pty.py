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
            assert not any('AssistantMessage' in item for item in session['history'])
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


if __name__ == '__main__':
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/takiza').resolve()
    tools.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), tools.API)
    threading.Thread(target=tools.server.serve_forever, daemon=True).start()
    try:
        tools.run_rewind(binary)
        tools.run_rewind(binary, with_git=False)
        run_chat_only(binary)
        run_edit_prompt(binary)
    finally:
        tools.server.shutdown()
