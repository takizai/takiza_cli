"""Custom banner art: paste, persistence, failed replacement, restart and reset."""
import json
import base64
import re
from pathlib import Path
import struct
import sys
import tempfile
import zlib

from tui_queue_pty import Terminal, Screen


def png():
    def chunk(kind, data):
        return struct.pack('!I', len(data)) + kind + data + struct.pack('!I', zlib.crc32(kind + data))
    return (b'\x89PNG\r\n\x1a\n'
            + chunk(b'IHDR', struct.pack('!IIBBBBB', 64, 64, 8, 2, 0, 0, 0))
            + chunk(b'IDAT', zlib.compress((b'\x00' + bytes([120, 45, 210]) * 64) * 64))
            + chunk(b'IEND', b''))


def run(binary, name='art', terminal_env=None):
    with tempfile.TemporaryDirectory(prefix='takiza-logo-') as directory:
        workspace = Path(directory)
        (workspace / '.takiza').mkdir()
        config = workspace / '.takiza/config.json'
        config.write_text(json.dumps({
            'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Amber',
            'language': 'en', 'model': 'test-model', 'mode': 'manual', 'api_key': 'test-only',
            'base_url': 'http://127.0.0.1:1/v1',
        }))
        art = workspace / 'art with spaces.png'
        art.write_bytes(png())
        env = dict(TERM='xterm-256color', TERM_PROGRAM='', KITTY_WINDOW_ID='', WEZTERM_PANE='',
                   KONSOLE_VERSION='', TMUX='', STY='', TAKIZA_LOGO_RENDER='auto')
        env.update(terminal_env or {})

        def has_art(output):
            if name == 'kitty':
                return b'\x1b_Ga=T,f=100' in output
            if name == 'iterm':
                return b'\x1b]1337;File=inline=1' in output
            if name == 'sixel':
                return b'\x1bP0;1;0q' in output
            return b'38;2;120;45;210' in output

        def screen(output):
            clean = re.sub(rb'\x1b_G.*?\x1b\\|\x1bP.*?\x1b\\|\x1b\]1337;.*?\x07', b'', output, flags=re.S)
            return '\n'.join(Screen(clean, 100, 30).text)

        terminal = Terminal(binary, workspace, 100, 30, env_overrides=env, initial_prompt=None, controlling_tty=True)
        try:
            terminal.wait(lambda: b'TAKIZA' in terminal.output, 'banner missing')
            if name in ('kitty', 'iterm', 'sixel'):
                terminal.wait(lambda: has_art(terminal.output), 'stock image missing')
                assert b'38;2;93;93;93' not in terminal.output, 'stock ASCII drawn with native image'
            else:
                terminal.wait(lambda: b'38;2;93;93;93' in terminal.output, 'stock ASCII missing')
            terminal.output = b''
            # A separate file paste is transformed into an image attachment marker.
            terminal.send('/logo ')
            terminal.send('\x1b[200~"' + str(art) + '"\x1b[201~\r')
            terminal.wait(lambda: has_art(terminal.output), f'{name}: custom art missing')
            assert 'TAKIZA' in screen(terminal.output), 'image protocol damaged the text banner'
            saved = json.loads(config.read_text())
            assert len(saved['custom_logo']['rgba']) < 2500, 'full-size image persisted'
            photo = base64.b64decode(saved['custom_logo']['png'])
            assert struct.unpack('!II', photo[16:24]) == (320, 320), 'photo was reduced to ASCII resolution'
            terminal.send('/logo /missing/image.png\r')
            terminal.pump(.3)
            assert json.loads(config.read_text())['custom_logo'] == saved['custom_logo'], 'failure replaced art'
        finally:
            terminal.close()
        art.unlink()
        terminal = Terminal(binary, workspace, 100, 30, env_overrides=env, initial_prompt=None, controlling_tty=True)
        try:
            terminal.wait(lambda: has_art(terminal.output), f'{name}: saved art lost after restart')
            # Initialize the terminal event reader before delivering resize signals.
            terminal.send('x\x7f')
            terminal.output = b''
            terminal.resize(60, 30)
            terminal.wait(lambda: b'Model:' in terminal.output, 'narrow terminal did not redraw')
            assert not has_art(terminal.output), 'hidden logo was painted over the compact layout'
            terminal.output = b''
            terminal.resize(100, 30)
            terminal.wait(lambda: has_art(terminal.output), 'logo not restored after resize')
            terminal.output = b''
            terminal.send('/theme\r')
            terminal.wait(lambda: b'Select Visual Theme' in terminal.output, 'theme menu did not open')
            assert 'Select Visual Theme' in screen(terminal.output), 'image corrupted the menu text'
            terminal.output = b''
            terminal.send('\x1b')
            terminal.wait(lambda: has_art(terminal.output), 'logo not restored after menu')
            terminal.output = b''
            terminal.resize(15, 7)
            terminal.wait(lambda: b'Resize termin' in terminal.output, 'tiny terminal did not redraw')
            assert not has_art(terminal.output), 'image painted over tiny-terminal message'
            terminal.output = b''
            terminal.resize(100, 30)
            terminal.wait(lambda: has_art(terminal.output), 'logo not restored after tiny terminal')
            terminal.output = b''
            terminal.send('/logo reset\r')
            terminal.wait(lambda: json.loads(config.read_text()).get('custom_logo') is None, 'reset not saved')
            terminal.wait(lambda: has_art(terminal.output) if name in ('kitty', 'iterm', 'sixel')
                          else b'38;2;93;93;93' in terminal.output, 'stock logo not restored')
            if name in ('kitty', 'iterm', 'sixel'):
                assert b'38;2;93;93;93' not in terminal.output, 'reset restored ASCII instead of image'
        finally:
            terminal.close()
    print(f'custom logo TUI ({name}): OK')


if __name__ == '__main__':
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/takiza').resolve()
    for name, env in [('art', {}), ('kitty', {'TERM': 'xterm-kitty'}),
                      ('iterm', {'TERM_PROGRAM': 'iTerm.app'}), ('sixel', {'TERM': 'foot'}),
                      ('art', {'TERM': 'xterm-kitty', 'TMUX': 'test-session'})]:
        run(binary, name, env)
