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
        if not body.get('stream') and body.get('tools'):
            self.server.requests.append(body)
            self.send_header('Content-Type', 'application/json')
            self.end_headers()
            self.wfile.write(json.dumps({'choices': [{'message': {'tool_calls': [{'id': 'fixed', 'type': 'function', 'function': {'name': 'find_files', 'arguments': '{"pattern":"file.txt"}'}}]}}], 'usage': {'total_tokens': 42}}).encode())
            return
        if not body.get('stream'):
            self.send_header('Content-Type', 'application/json')
            self.end_headers()
            self.wfile.write(b'{"choices":[{"message":{"content":"Parser test"}}]}')
            return
        self.server.requests.append(body)
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()
        if self.server.mode == 'missing_find' and len(self.server.requests) <= 2:
            deltas = [{'tool_calls': [{'index': 0, 'id': 'broken', 'function': {'name': 'find_files'}}]}]
        elif len(self.server.requests) > 1:
            deltas = [{'content': 'PARSER_OK'}]
        elif self.server.mode == 'empty':
            deltas = [{'tool_calls': [{'index': 0, 'id': 'broken', 'function': {'name': 'web_search', 'arguments': ''}}]}]
        elif self.server.mode == 'object_parallel':
            deltas = [{'tool_calls': [
                {'id': 'first', 'function': {'name': 'read_file', 'arguments': {'path': 'file.txt'}}},
                {'id': 'second', 'function': {'name': 'list_dir', 'arguments': {}}},
            ]}]
        elif self.server.mode in ('wrapped_write', 'conflicting_write'):
            inner_path = 'other.txt' if self.server.mode == 'conflicting_write' else 'written.txt'
            args = {'path': 'written.txt', 'content': {'path': inner_path, 'content': 'exact "quotes"\nпривет\n'}}
            deltas = [{'tool_calls': [{'index': 0, 'id': 'write', 'function': {
                'name': 'write_file', 'arguments': json.dumps(args)}}]}]
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
                if mode in ('empty', 'missing_find', 'conflicting_write'):
                    terminal.wait(lambda: b'PARSER_OK' in terminal.output, 'invalid call recovery did not complete', timeout=10)
                    assert len(server.requests) == (4 if mode == 'missing_find' else 2)
                    repair = server.requests[1]['messages'][-1]
                    assert repair['role'] == 'tool'
                    report = json.loads(repair['content'])
                    assert report['executed'] is False and report['batch_executed'] is False
                    expected = 'pattern' if mode == 'missing_find' else 'content' if mode == 'conflicting_write' else 'query'
                    assert expected in report['parameters_schema']['required']
                    if mode == 'conflicting_write':
                        assert not (workspace / 'written.txt').exists()
                        assert not (workspace / 'other.txt').exists()
                        assert 'path: string, content: string' in report['error']
                    if mode == 'missing_find':
                        assert not server.requests[2].get('stream')
                        result = server.requests[3]['messages'][-1]
                        assert result['role'] == 'tool' and 'file.txt' in result['content']
                    assert b'EOF while parsing' not in terminal.output, 'raw JSON errors reached execution'
                else:
                    terminal.wait(lambda: b'PARSER_OK' in terminal.output, 'valid arguments did not complete', timeout=8)
                    assert len(server.requests) == 2
                    tools = [message for message in server.requests[1]['messages'] if message['role'] == 'tool']
                    assert len(tools) == (2 if mode == 'object_parallel' else 1)
                    if mode == 'wrapped_write':
                        expected_text = 'exact "quotes"\nпривет\n'
                        assert (workspace / 'written.txt').read_text() == expected_text
                        call = next(message for message in server.requests[1]['messages'] if message.get('tool_calls'))
                        assert json.loads(call['tool_calls'][0]['function']['arguments']) == {
                            'path': 'written.txt', 'content': expected_text}
                    else:
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
    for mode in ['empty', 'missing_find', 'object_parallel', 'fragmented', 'wrapped_write', 'conflicting_write']:
        run(binary, mode)
