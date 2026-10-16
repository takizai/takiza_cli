"""Workspace trust permission prompt: python3 tests/tui_workspace_trust_pty.py [binary].

Verifies:
1. When opening Takiza in a new directory, it prompts for permission to access files in that directory.
2. If permission is denied (Esc or selecting 'Deny and exit'), the agent terminates with:
   'Directory access was declined. Goodbye!'
3. If permission is allowed (Enter on 'Allow access to this directory'), Takiza continues to the interactive prompt.
4. Subsequent launches in the same directory do not ask again.
5. Launching with --auto-approve (-y) bypasses the prompt immediately.
"""
import json
import os
from pathlib import Path
import shutil
import sys
import tempfile
import time
from tui_queue_pty import Terminal, Screen


def run(binary):
    with tempfile.TemporaryDirectory(prefix='takiza-trust-pty-') as temp_home:
        home_path = Path(temp_home)
        global_config_dir = home_path / '.config/takiza'
        global_config_dir.mkdir(parents=True)
        # Pre-agree to terms in global config so terms dialog doesn't interfere
        (global_config_dir / 'config.json').write_text(json.dumps({
            'agreed_to_terms': True,
            'terms_version': '1.0.0',
            'theme': 'Monochrome',
            'mode': 'manual',
            'model': 'test-model',
            'api_key': 'test-only',
            'base_url': 'http://127.0.0.1:9/v1',
            'trusted_directories': [],
        }))

        env = {'HOME': str(home_path), 'TAKIZA_AUTO_APPROVE': 'false'}

        def screen(terminal):
            return '\n'.join(Screen(terminal.output, 80, 26).text)

        # 1. Deny via Esc
        workspace_1 = Path(tempfile.mkdtemp(prefix='takiza-ws1-'))
        try:
            terminal = Terminal(binary, workspace_1, 80, 26, env_overrides=env, initial_prompt=None, controlling_tty=True)
            terminal.wait(lambda: 'Directory access permission' in screen(terminal), 'permission prompt not shown')
            assert 'Allow Takiza Code access to files in this directory?' in screen(terminal)
            assert 'Allow access to this directory' in screen(terminal)
            assert 'Deny and exit' in screen(terminal)

            # Press Escape to deny
            terminal.send('\x1b')
            terminal.wait(lambda: terminal.process.poll() is not None, 'process did not exit on deny')
            assert terminal.process.returncode == 0
            terminal.pump(0.1)
            raw_out = terminal.output.decode('utf-8', errors='replace')
            assert 'Directory access was declined. Goodbye!' in raw_out
            terminal.close()
        finally:
            shutil.rmtree(workspace_1, ignore_errors=True)

        # 2. Deny via Down arrow -> Enter
        workspace_2 = Path(tempfile.mkdtemp(prefix='takiza-ws2-'))
        try:
            terminal = Terminal(binary, workspace_2, 80, 26, env_overrides=env, initial_prompt=None, controlling_tty=True)
            terminal.wait(lambda: 'Directory access permission' in screen(terminal), 'permission prompt not shown')

            # Move selection down to 'Deny and exit' and press Enter
            terminal.send('\x1b[B')  # Down arrow
            time.sleep(0.05)
            terminal.send('\r')     # Enter
            terminal.wait(lambda: terminal.process.poll() is not None, 'process did not exit on deny')
            assert terminal.process.returncode == 0
            terminal.pump(0.1)
            raw_out = terminal.output.decode('utf-8', errors='replace')
            assert 'Directory access was declined. Goodbye!' in raw_out
            terminal.close()
        finally:
            shutil.rmtree(workspace_2, ignore_errors=True)

        # 3. Allow via Enter, verify persistence and subsequent runs
        workspace_3 = Path(tempfile.mkdtemp(prefix='takiza-ws3-'))
        try:
            terminal = Terminal(binary, workspace_3, 80, 26, env_overrides=env, initial_prompt=None, controlling_tty=True)
            terminal.wait(lambda: 'Directory access permission' in screen(terminal), 'permission prompt not shown')

            # Press Enter on default selection ('Allow access to this directory')
            terminal.send('\r')

            # Expect Takiza main UI to load ("You" or input prompt)
            terminal.wait(lambda: 'You' in screen(terminal), 'interactive prompt did not appear after allowing')
            terminal.send('/exit\r')
            terminal.wait(lambda: terminal.process.poll() is not None, 'did not exit on /exit')
            terminal.close()

            # Verify global config now contains workspace_3
            saved_config = json.loads((global_config_dir / 'config.json').read_text())
            canonical_ws3 = str(workspace_3.resolve())
            assert any(d == canonical_ws3 or Path(d) == workspace_3.resolve() for d in saved_config.get('trusted_directories', [])), (
                f"workspace {canonical_ws3} not in trusted_directories: {saved_config.get('trusted_directories')}"
            )

            # Relaunch in workspace_3 - should NOT prompt for permission
            terminal2 = Terminal(binary, workspace_3, 80, 26, env_overrides=env, initial_prompt=None, controlling_tty=True)
            terminal2.wait(lambda: 'You' in screen(terminal2), 'interactive prompt did not appear')
            assert 'Directory access permission' not in screen(terminal2)
            terminal2.send('/exit\r')
            terminal2.wait(lambda: terminal2.process.poll() is not None, 'did not exit on /exit')
            terminal2.close()
        finally:
            shutil.rmtree(workspace_3, ignore_errors=True)

        # 4. Auto-approve bypass
        workspace_4 = Path(tempfile.mkdtemp(prefix='takiza-ws4-'))
        try:
            auto_env = {'HOME': str(home_path), 'TAKIZA_AUTO_APPROVE': 'true'}
            terminal = Terminal(binary, workspace_4, 80, 26, env_overrides=auto_env, initial_prompt=None, controlling_tty=True)
            terminal.wait(lambda: 'You' in screen(terminal), 'interactive prompt did not appear with auto-approve')
            assert 'Directory access permission' not in screen(terminal)
            terminal.send('/exit\r')
            terminal.wait(lambda: terminal.process.poll() is not None, 'did not exit on /exit')
            terminal.close()
        finally:
            shutil.rmtree(workspace_4, ignore_errors=True)

    print('PASS workspace trust: prompt on new directory, deny exits cleanly, allow remembers workspace, auto-approve bypasses')


if __name__ == '__main__':
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/takiza').resolve()
    run(binary)
