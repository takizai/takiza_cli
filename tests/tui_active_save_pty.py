"""Active transcript survives a hard restart: python3 tests/tui_active_save_pty.py [binary]."""
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
        self.end_headers()
        if not body.get('stream'):
            self.wfile.write(b'{"choices":[{"message":{"content":"Active save test"}}]}')
            return
        self.server.calls += 1
        index = self.server.calls
        try:
            if index == 1:
                delta = {'tool_calls': [{'index': 0, 'id': 'w', 'function': {
                    'name': 'write_file', 'arguments': json.dumps({'path': 'result.txt', 'content': 'written\n'})}}]}
                finish = 'tool_calls'
            elif index == 2:
                delta = {'reasoning_content': 'SAVED_REASONING', 'content': 'SAVED_PARTIAL_REPLY'}
                finish = None
            else:
                delta = {'content': 'FINAL_REPLY'}
                finish = 'stop'
            self.wfile.write(('data: ' + json.dumps({'choices': [{'delta': delta, 'finish_reason': finish}]}) + '\n\n').encode())
            self.wfile.flush()
            if index == 2:
                self.server.release.wait(20)
            self.wfile.write(b'data: [DONE]\n\n')
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass


def run(binary):
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), API)
    server.calls = 0
    server.release = threading.Event()
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix='takiza-active-save-') as directory:
            workspace = Path(directory)
            (workspace / '.takiza').mkdir()
            (workspace / '.takiza/config.json').write_text(json.dumps({
                'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Amber', 'mode': 'manual',
                'model': 'fixture-model', 'api_key': 'fixture-only',
                'base_url': f'http://127.0.0.1:{server.server_port}/v1',
            }))
            terminal = Terminal(binary, workspace, 100, 35, initial_prompt='FIRST_PROMPT')
            def saved():
                sid = (workspace / '.takiza/latest_session').read_text().strip()
                return json.loads((workspace / '.takiza/sessions' / f'{sid}.json').read_text())
            try:
                terminal.wait(lambda: b'SAVED_PARTIAL_REPLY' in terminal.output, 'active output missing')
                terminal.wait(lambda: any(item.get('AssistantMessage') == 'SAVED_PARTIAL_REPLY'
                                         for item in saved()['history']), 'active output not saved')
                assert b'\x1b]0;Active save test' in terminal.output
                assert 'Active save test — Takiza Code'.encode() in terminal.output
                assert (workspace / 'result.txt').read_text() == 'written\n'
                assert any('ToolEnd' in item for item in saved()['history'])
                assert any(item.get('Thought') == 'SAVED_REASONING' for item in saved()['history'])
                # No /exit and no Finished event: simulate a crash during generation.
                terminal.process.kill()
                terminal.process.wait(timeout=3)
            finally:
                terminal.close()
                server.release.set()
            resumed = Terminal(binary, workspace, 100, 35, initial_prompt=None, cli_args=['--continue'])
            try:
                resumed.wait(lambda: b'SAVED_PARTIAL_REPLY' in resumed.output, 'partial reply lost on restart')
                assert b'SAVED_REASONING' in resumed.output
                assert 'Active save test — Takiza Code'.encode() in resumed.output
                resumed.send('SECOND_PROMPT\r')
                resumed.wait(lambda: b'FINAL_REPLY' in resumed.output, 'resumed response missing')
                resumed.send('/exit\r')
                resumed.wait(lambda: resumed.process.poll() is not None, 'exit stalled')
                history = saved()['history']
                first = next(i for i, item in enumerate(history) if item.get('UserPrompt') == 'FIRST_PROMPT')
                second = next(i for i, item in enumerate(history) if item.get('UserPrompt') == 'SECOND_PROMPT')
                between = history[first + 1:second]
                assert any('ToolEnd' in item for item in between)
                assert any(item.get('AssistantMessage') == 'SAVED_PARTIAL_REPLY' for item in between)
                print('PASS active save: tool activity, reasoning and partial reply survive hard restart between prompts')
            finally:
                resumed.close()
    finally:
        server.release.set()
        server.shutdown()
        server.server_close()


if __name__ == '__main__':
    run(Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/takiza').resolve())
