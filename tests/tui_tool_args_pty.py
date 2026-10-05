"""Tool-call argument regressions: python3 tests/tui_tool_args_pty.py [binary].
Uses an isolated workspace and localhost API; no real keys or searches.
"""
import datetime
import http.server
import json
from pathlib import Path
import sys
import tempfile
import threading
from tui_queue_pty import Terminal

class API(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        self.send_response(200)
        if not body.get('stream'):
            self.send_header('Content-Type', 'application/json')
            self.end_headers()
            self.wfile.write(b'{"choices":[{"message":{"content":"Parser test"}}]}')
            return
        self.server.requests.append(body)
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()
        if len(self.server.requests) > 1:
            deltas = [{'content': 'PARSER_OK'}]
        elif self.server.mode == 'empty':
            deltas = [{'tool_calls': [{'index': 0, 'id': 'broken', 'function': {'name': 'web_search', 'arguments': ''}}]}]
        elif self.server.mode == 'object_parallel':
            deltas = [{'tool_calls': [
                {'id': 'first', 'function': {'name': 'read_file', 'arguments': {'path': 'file.txt'}}},
                {'id': 'second', 'function': {'name': 'list_dir', 'arguments': {}}},
            ]}]
        else:
            deltas = [
                {'tool_calls': [{'index': 0, 'id': 'first', 'function': {'name': 'read_file', 'arguments': ''}}]},
                {'tool_calls': [{'index': 0, 'function': {'arguments': '{"path":'}}]},
                {'tool_calls': [{'index': 0, 'function': {'arguments': '"file.txt"}'}}]},
            ]
        try:
            for delta in deltas:
                self.wfile.write(('data: ' + json.dumps({'choices': [{'delta': delta}]}) + '\n\n').encode())
                self.wfile.flush()
            self.wfile.write(b'data: [DONE]\n\n')
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass


def run(binary, mode):
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), API)
    server.mode = mode
    server.requests = []
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        with tempfile.TemporaryDirectory(prefix='takiza-tool-arguments-') as directory:
            workspace = Path(directory)
            (workspace / '.takiza').mkdir()
            (workspace / '.takiza/config.json').write_text(json.dumps({
                'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Amber', 'mode': 'manual',
                'model': 'fixture-model', 'api_key': 'fixture-only', 'auto_approve': False,
                'base_url': f'http://127.0.0.1:{server.server_port}/v1',
            }))
            (workspace / 'file.txt').write_text('fixture evidence\n')
            terminal = Terminal(binary, workspace, 100, 30, initial_prompt='search latest news')
            try:
                if mode == 'empty':
                    terminal.wait(lambda: b'missing' in terminal.output and b'JSON arguments' in terminal.output,
                                  'incomplete tool call was not rejected', timeout=8)
                    terminal.pump(.4)
                    assert len(server.requests) == 1, 'invalid calls triggered more model requests'
                    assert b'EOF while parsing' not in terminal.output, 'raw JSON errors still reached tool execution'
                else:
                    terminal.wait(lambda: b'PARSER_OK' in terminal.output, 'valid arguments did not complete', timeout=8)
                    assert len(server.requests) == 2
                    tools = [message for message in server.requests[1]['messages'] if message['role'] == 'tool']
                    assert len(tools) == (2 if mode == 'object_parallel' else 1)
                    assert 'fixture evidence' in tools[0]['content'], 'object/fragment arguments were lost'
                    assert all('Invalid JSON' not in message['content'] for message in tools)
                prompt = server.requests[0]['messages'][0]['content']
                assert str(datetime.date.today()) in prompt, 'model was not given the actual date'
                print(f'PASS {mode}: TUI argument handling and current date')
            except Exception:
                Path(f'/tmp/takiza-tool-arguments-{mode}.log').write_bytes(terminal.output)
                raise
            finally:
                terminal.close()
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)

if __name__ == '__main__':
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/takiza').resolve()
    for mode in ['empty', 'object_parallel', 'fragmented']:
        run(binary, mode)
