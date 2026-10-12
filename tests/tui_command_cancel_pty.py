"""Esc cancels process trees and restores input: python3 tests/tui_command_cancel_pty.py [binary]."""
import http.server
import json
from pathlib import Path
import sys
import tempfile
import threading
from tui_queue_pty import Terminal

COMMAND = 'sleep 30 & echo $! > child.pid; echo COMMAND_RUNNING; wait'


class API(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_): pass
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        self.send_response(200); self.end_headers()
        if not body.get('stream'):
            self.wfile.write(b'{"choices":[{"message":{"content":"Cancel test"}}]}'); return
        self.server.calls += 1
        if self.server.calls == 1:
            delta = {'tool_calls': [{'index': 0, 'id': 'cmd', 'function': {
                'name': 'run_command', 'arguments': json.dumps({'command': COMMAND})}}]}
            reason = 'tool_calls'
        else:
            delta = {'content': 'RECOVERED_INPUT'}; reason = 'stop'
        try:
            self.wfile.write(('data: ' + json.dumps({'choices': [{'delta': delta, 'finish_reason': reason}]}) + '\n\ndata: [DONE]\n\n').encode())
        except (BrokenPipeError, ConnectionResetError): pass


def run(binary, direct):
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), API); server.calls = 0
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix='takiza-command-stop-') as directory:
            workspace = Path(directory); (workspace / '.takiza').mkdir()
            (workspace / '.takiza/config.json').write_text(json.dumps({
                'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Amber', 'mode': 'manual',
                'model': 'fixture-model', 'api_key': 'fixture-only', 'auto_approve': True,
                'base_url': f'http://127.0.0.1:{server.server_port}/v1',
            }))
            terminal = Terminal(binary, workspace, 100, 35, env_overrides={'TAKIZA_AUTO_APPROVE': 'true'},
                                initial_prompt=None if direct else 'run the command')
            try:
                if direct:
                    terminal.wait(lambda: b'Type your prompt' in terminal.output, 'editor missing')
                    terminal.send('!' + COMMAND + '\r')
                terminal.wait(lambda: (workspace / 'child.pid').exists(), 'command did not start')
                pid = int((workspace / 'child.pid').read_text().strip())
                terminal.send('\x1b')
                def stopped():
                    path = Path(f'/proc/{pid}/stat')
                    return not path.exists() or path.read_text().split(') ', 1)[1].startswith('Z')
                terminal.wait(stopped, 'Esc left the command descendant running', timeout=3)
                terminal.send('!echo RECOVERED_INPUT\r' if direct else 'next prompt\r')
                terminal.wait(lambda: b'RECOVERED_INPUT' in terminal.output, 'TUI input stayed blocked', timeout=5)
                terminal.send('/exit\r')
                terminal.wait(lambda: terminal.process.poll() is not None, 'exit stayed blocked', timeout=5)
                print(f'PASS command cancellation: {"direct !" if direct else "agent tool"}, descendants stop and input resumes')
            except Exception:
                Path('/tmp/takiza-command-cancel-pty.log').write_bytes(terminal.output); raise
            finally: terminal.close()
    finally: server.shutdown(); server.server_close()


if __name__ == '__main__':
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/takiza').resolve()
    for direct in [False, True]: run(binary, direct)
