"""Resume a large chat; typing, scrolling and menus must remain responsive."""
import json
from pathlib import Path
import sys
import tempfile
from tui_queue_pty import Terminal, Screen

if __name__ == '__main__':
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/takiza').resolve()
    with tempfile.TemporaryDirectory(prefix='takiza-large-history-') as directory:
        workspace = Path(directory)
        sessions = workspace / '.takiza/sessions'
        sessions.mkdir(parents=True)
        (workspace / '.takiza/config.json').write_text(json.dumps({
            'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Monochrome',
            'mode': 'manual', 'model': 'test-model', 'api_key': 'test-only', 'base_url': 'http://127.0.0.1:1/v1'}))
        history = [{'UserPrompt': 'inspect project'}]
        for i in range(3000):
            history.append({'AssistantMessage': f'MESSAGE{i:04d}\n' + 'A **formatted** paragraph with `code` and Russian текст. ' * 20})
        history.append({'AssistantMessage': 'LASTMESSAGE'})
        (sessions / '20261006_000000.json').write_text(json.dumps({
            'id': '20261006_000000', 'created_at': '2026-10-06 00:00:00', 'model': 'test-model',
            'title': 'Large chat', 'started': True, 'messages': [{'role': 'user', 'content': 'inspect project'}], 'history': history}))
        t = Terminal(binary, workspace, 100, 30, initial_prompt=None, cli_args=['--session', '20261006_000000'], controlling_tty=True)
        def screen(): return '\n'.join(Screen(t.output, 100, 30).text)
        try:
            t.wait(lambda: 'LASTMESSAGE' in screen(), 'large chat failed to render', timeout=15)
            t.send('draft')
            t.wait(lambda: 'draft' in screen(), 'typing stalled')
            t.send('\x1b[1;5H')
            t.wait(lambda: 'MESSAGE0000' in screen(), 'scroll to oldest message stalled')
            t.send('\x1b[1;5F')
            t.wait(lambda: 'LASTMESSAGE' in screen(), 'scroll to latest stalled')
            # Wait for each resize frame before injecting more terminal input.
            for cols, rows in [(90, 28), (100, 30)]:
                t.resize(cols, rows)
                t.wait(lambda: Screen(t.output, cols, rows).text[-1].endswith('╯')
                       and 'draft' in Screen(t.output, cols, rows).text[-2],
                       'resize frame did not finish', timeout=15)
            t.wait(lambda: 'LASTMESSAGE' in screen() and 'draft' in screen(), 'resize lost chat or editor')
            t.send('\x15/tools\r')
            t.wait(lambda: 'Autonomous Agent Tools' in screen(), 'tools menu stalled')
            t.send('\x1b')
            t.wait(lambda: 'LASTMESSAGE' in screen(), 'menu did not restore large history')
            t.send('/exit\r')
            t.wait(lambda: t.process.poll() is not None, 'large chat exit stalled', timeout=10)
            assert t.process.returncode == 0
            print('PASS large history: 3000 Markdown answers, typing, scrolling, resize, tools menu, exit')
        except Exception:
            Path('/tmp/takiza-large-history-screen.txt').write_text(screen())
            Path('/tmp/takiza-large-history-pty.log').write_bytes(t.output)
            raise
        finally: t.close()
