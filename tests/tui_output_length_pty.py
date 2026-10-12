"""Reasoning/output truncation gets one changed retry, never an unsafe write."""
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import sys
import tempfile
import threading
from tui_queue_pty import Terminal, Screen

requests = []
recover = True


class API(BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        self.send_response(200)
        self.end_headers()
        if not request.get('tools'):
            self.wfile.write(b'{"choices":[{"message":{"content":"Length test"}}]}')
            return
        requests.append(request)
        count = len(requests)
        if count == 1 or not recover:
            delta = {'reasoning_content': 'Long project plan', 'tool_calls': [{
                'index': 0, 'id': 'truncated', 'function': {'name': 'write_file',
                'arguments': '{"path":"unsafe.txt","content":"unfinished'}}]}
            reason = 'length'
        elif count == 2:
            delta = {'tool_calls': [{'index': 0, 'id': 'safe', 'function': {'name': 'write_file',
                'arguments': json.dumps({'path': 'result.txt', 'content': 'recovered'})}}]}
            reason = 'tool_calls'
        else:
            delta = {'content': 'TASK_RECOVERED'}
            reason = 'stop'
        packet = {'choices': [{'delta': delta, 'finish_reason': reason}], 'usage': {'total_tokens': 8192}}
        self.wfile.write(('data: ' + json.dumps(packet) + '\n\ndata: [DONE]\n\n').encode())


if __name__ == '__main__':
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/takiza').resolve()
    server = ThreadingHTTPServer(('127.0.0.1', 0), API)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        for recover in [True, False]:
            requests.clear()
            with tempfile.TemporaryDirectory(prefix='takiza-output-length-') as directory:
                workspace = Path(directory)
                (workspace / '.takiza').mkdir()
                (workspace / '.takiza/config.json').write_text(json.dumps({
                    'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Monochrome',
                    'mode': 'manual', 'model': 'test', 'api_key': 'test-only',
                    'base_url': f'http://127.0.0.1:{server.server_port}/v1'}))
                terminal = Terminal(binary, workspace, 100, 30, initial_prompt='build project')

                def screen():
                    return '\n'.join(Screen(terminal.output, 100, 30).text)

                try:
                    if recover:
                        terminal.wait(lambda: 'TASK_RECOVERED' in screen(), 'task did not recover')
                        assert (workspace / 'result.txt').read_text() == 'recovered'
                        assert len(requests) == 3
                    else:
                        terminal.wait(lambda: 'finish_reason: length' in screen(), 'precise output-limit error missing')
                        terminal.pump(.3)
                        assert len(requests) == 2, 'agent repeated exhausted output requests'
                    assert not (workspace / 'unsafe.txt').exists(), 'truncated file was written'
                    assert 'one small file or edit at a time' in requests[1]['messages'][-1]['content']
                    terminal.send('/exit\r')
                    terminal.wait(lambda: terminal.process.poll() is not None, 'exit stalled')
                    saved = [json.loads(path.read_text()) for path in (workspace / '.takiza/sessions').glob('*.json')]
                    assert saved
                    assert all('one small file or edit at a time' not in json.dumps(item['messages']) for item in saved), \
                        'temporary recovery instruction replaced chat history'
                except Exception:
                    print(screen())
                    raise
                finally:
                    terminal.close()
        print('PASS output length: changed retry, safe tool execution, bounded failure, original history')
    finally:
        server.shutdown()
        server.server_close()
