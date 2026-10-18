"""Unavailable MoA and the hidden theme: real menu input and persisted settings."""
import json
from pathlib import Path
import sys
import tempfile

from tui_queue_pty import Terminal, Screen


def run(binary):
    with tempfile.TemporaryDirectory(prefix='takiza-hidden-options-') as directory:
        workspace = Path(directory)
        (workspace / '.takiza').mkdir()
        config = workspace / '.takiza/config.json'
        config.write_text(json.dumps({
            'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Amber',
            'language': 'en', 'model': 'test-model', 'mode': 'moa',
            'api_key': 'test-only', 'base_url': 'http://127.0.0.1:1/v1',
        }))
        terminal = Terminal(binary, workspace, 120, 36, initial_prompt=None, controlling_tty=True)

        def screen():
            return '\n'.join(Screen(terminal.output, 120, 36).text)

        def see(text):
            terminal.wait(lambda: text in screen(), f'missing {text!r}')

        try:
            see('TAKIZA')
            for command in ['/mode moa', '/mode manual changed-model', '/mode']:
                terminal.send(command + '\r')
                see('A third-party provider is active. Takiza mode selection is unavailable.')
                assert 'Select Execution Mode' not in screen(), screen()
                assert json.loads(config.read_text())['model'] == 'test-model'
            for command in ['/usage', '/quota']:
                terminal.send(command + '\r')
                see('Check your provider dashboard for current usage, balance and limits.')
                assert '[0m' not in screen(), screen()
                assert '520,000' not in screen() and '[Mock]' not in screen(), screen()
                terminal.send('\x1b')
                terminal.pump(.15)
            terminal.send('/status\r')
            see('Third-party provider')
            terminal.send('o')
            see('Takiza mode selection is unavailable.')
            assert '[0m' not in screen(), screen()
            terminal.send('\x1b')
            terminal.pump(.15)
            terminal.send('/status\r')
            see('Third-party provider')
            terminal.send('u')
            see('Check your provider dashboard for current usage, balance and limits.')
            assert '[0m' not in screen(), screen()
            terminal.send('\x1b')
            terminal.pump(.15)
            terminal.send('/theme evening-irkutsk\r')
            see('Unknown theme:')
            assert json.loads(config.read_text())['theme'] == 'Amber'
            terminal.send('/theme\r')
            see('Select Visual Theme')
            assert 'Irkutsk' not in screen(), screen()
            # A partial sequence does not unlock; Esc cancels any live preview.
            terminal.send('\x1b[A\x1b[A\x1b[B\x1b[B\x1b[D')
            terminal.pump(.15)
            assert json.loads(config.read_text())['theme'] == 'Amber'
            terminal.send('\x1b')
            terminal.pump(.15)
            terminal.send('/theme\r')
            see('Select Visual Theme')
            terminal.send('\x1b[A\x1b[A\x1b[B\x1b[B\x1b[D\x1b[C\x1b[D\x1b[Cba')
            terminal.wait(lambda: json.loads(config.read_text())['theme'] == 'EveningIrkutsk', 'secret theme not saved')
        finally:
            terminal.close()

        terminal = Terminal(binary, workspace, 120, 36, initial_prompt=None, controlling_tty=True)
        try:
            terminal.wait(lambda: b'TAKIZA' in terminal.output, 'restart failed')
            assert json.loads(config.read_text())['theme'] == 'EveningIrkutsk'
            terminal.send('/theme\r')
            see('Select Visual Theme')
            assert 'Irkutsk' not in screen(), screen()
            terminal.send('\x1b')
            terminal.pump(.15)
            assert json.loads(config.read_text())['theme'] == 'EveningIrkutsk'
            terminal.send('/theme ocean\r')
            terminal.wait(lambda: json.loads(config.read_text())['theme'] == 'Ocean', 'could not leave secret theme')
        finally:
            terminal.close()
        print('PASS: third-party mode/usage/status, hidden theme unlock/cancel/restart/exit')


if __name__ == '__main__':
    run(Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/takiza').resolve())
