"""Shift+Tab, real Plan restrictions, live switching, persistence and legacy -y."""
import http.server
import json
from pathlib import Path
import sys
import tempfile
import threading

from tui_queue_pty import Terminal, Screen

requests = []
busy_release = threading.Event()


class API(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        self.send_response(200)
        if not body.get('stream'):
            self.send_header('Content-Type', 'application/json')
            self.end_headers()
            self.wfile.write(b'{"choices":[{"message":{"content":"Execution modes"}}]}')
            return
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()
        messages = body['messages']
        index = max(i for i, item in enumerate(messages) if item['role'] == 'user')
        phase = messages[index]['content']
        requests.append((phase, body))
        has_results = any(item['role'] == 'tool' for item in messages[index + 1:])
        if has_results:
            delta, finish = {'content': 'MODE_DONE_' + phase}, 'stop'
        else:
            if phase == 'busy':
                busy_release.wait(15)
            tools = [('read_file', {'path': 'input.txt'})] if phase == 'plan' else [
                ('write_file', {'path': phase + '-file.txt', 'content': 'changed'}),
                ('run_command', {'command': 'touch ' + phase + '-command.txt'}),
            ]
            delta = {'tool_calls': [{'index': i, 'id': f'{phase}_{i}', 'type': 'function',
                'function': {'name': name, 'arguments': json.dumps(args)}} for i, (name, args) in enumerate(tools)]}
            finish = 'tool_calls'
        try:
            self.wfile.write(('data: ' + json.dumps({'choices': [{'delta': delta, 'finish_reason': finish}]})
                              + '\n\ndata: [DONE]\n\n').encode())
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass


def run(binary):
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), API)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix='takiza-modes-') as directory:
            workspace = Path(directory)
            (workspace / '.takiza').mkdir()
            (workspace / 'input.txt').write_text('original')
            config = workspace / '.takiza/config.json'
            config.write_text(json.dumps({
                'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Amber', 'language': 'en',
                'mode': 'manual', 'model': 'test-model', 'api_key': 'test-only',
                'base_url': f'http://127.0.0.1:{server.server_port}/v1',
            }))

            def mode():
                return json.loads(config.read_text()).get('execution_mode', 'auto')

            terminal = Terminal(binary, workspace, 100, 30, initial_prompt=None, controlling_tty=True)
            try:
                terminal.wait(lambda: b'Shift+Tab' in terminal.output, 'mode indicator missing')
                terminal.send('draft kept')
                for expected in ['plan', 'allow-all', 'auto']:
                    terminal.send('\x1b[Z')
                    terminal.wait(lambda: mode() == expected, 'Shift+Tab did not persist ' + expected)
                    assert 'draft kept' in '\n'.join(Screen(terminal.output, 100, 30).text), 'mode switch lost draft'
                terminal.send('\x15/permissions plan\r')
                terminal.wait(lambda: mode() == 'plan', 'explicit Plan command failed')
                terminal.send('plan\r')
                terminal.wait(lambda: b'MODE_DONE_plan' in terminal.output, 'Plan did not finish', timeout=10)
                plan_body = next(body for phase, body in requests if phase == 'plan')
                names = {tool['function']['name'] for tool in plan_body['tools']}
                assert {'read_file', 'ask_question'} <= names
                assert not {'run_command', 'write_file', 'edit_file'} & names
                assert 'PLAN MODE' in plan_body['messages'][0]['content']
                terminal.send('!touch plan-shell.txt\r')
                terminal.wait(lambda: b'Plan mode blocks shell' in terminal.output, 'direct shell bypassed Plan')
                assert not (workspace / 'plan-shell.txt').exists()

                terminal.send('/permissions auto\r')
                terminal.wait(lambda: mode() == 'auto', 'Auto command failed')
                terminal.send('busy\r')
                terminal.wait(lambda: any(phase == 'busy' for phase, _ in requests), 'busy request missing')
                terminal.send('busy draft\x1b[Z')
                terminal.wait(lambda: mode() == 'plan', 'busy Shift+Tab did not switch to Plan')
                assert 'busy draft' in '\n'.join(Screen(terminal.output, 100, 30).text), 'busy mode switch lost draft'
                busy_release.set()
                terminal.wait(lambda: b'MODE_DONE_busy' in terminal.output, 'busy request did not finish', timeout=10)
                assert not (workspace / 'busy-file.txt').exists(), 'in-flight tool call bypassed Plan'
                assert not (workspace / 'busy-command.txt').exists(), 'in-flight command bypassed Plan'

                terminal.send('\x15\x1b[Z')
                terminal.wait(lambda: mode() == 'allow-all', 'Allow-all cycle failed')
                terminal.send('allow\r')
                terminal.wait(lambda: b'MODE_DONE_allow' in terminal.output, 'Allow-all waited for approval', timeout=10)
                assert (workspace / 'allow-file.txt').read_text() == 'changed'
                assert (workspace / 'allow-command.txt').exists()
                terminal.send('/permissions plan\r')
                terminal.wait(lambda: mode() == 'plan', 'final Plan save failed')
            finally:
                busy_release.set()
                terminal.close()

            terminal = Terminal(binary, workspace, 100, 30, initial_prompt=None, controlling_tty=True)
            try:
                terminal.wait(lambda: b'Plan' in terminal.output and b'Shift+Tab' in terminal.output, 'Plan not restored')
                terminal.send('!touch restart-shell.txt\r')
                terminal.wait(lambda: b'Plan mode blocks shell' in terminal.output, 'restarted Plan allowed shell')
                assert not (workspace / 'restart-shell.txt').exists()
            finally:
                terminal.close()
            terminal = Terminal(binary, workspace, 100, 30, initial_prompt=None, controlling_tty=True, cli_args=['-y'])
            try:
                terminal.wait(lambda: b'Allow-all' in terminal.output and b'Shift+Tab' in terminal.output, '-y did not override saved Plan')
            finally:
                terminal.close()
    finally:
        server.shutdown()
        server.server_close()
    print('execution modes TUI: OK')


if __name__ == '__main__':
    run(Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/takiza').resolve())
