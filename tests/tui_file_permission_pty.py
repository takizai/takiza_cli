"""Workspace writes are automatic; external writes require a reviewed approval."""
import http.server
import json
from pathlib import Path
import sys
import tempfile
import threading
from tui_queue_pty import Terminal, Screen

outside = None


class API(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        data = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        self.send_response(200)
        self.end_headers()
        if not data.get('stream'):
            self.wfile.write(b'{"choices":[{"message":{"content":"File approval test"}}]}')
            return
        messages = data['messages']
        prompt = next(m['content'] for m in reversed(messages) if m['role'] == 'user')
        last = messages[-1]
        if last['role'] == 'tool':
            delta = {'content': f'DONE_{prompt}'}
            finish = 'stop'
        else:
            name = 'write_file'
            if prompt == 'inside':
                args = {'path': 'inside.txt', 'content': 'INSIDE'}
            elif prompt == 'broken':
                args = {'path': 'broken', 'content': 'MUST_NOT_WRITE'}
            elif prompt == 'edit':
                name = 'edit_file'
                args = {'path': str(outside), 'target_content': 'FIRST', 'replacement_content': 'SECOND'}
            else:
                args = {'path': '../external.txt', 'content': 'FIRST'}
            delta = {'tool_calls': [{'index': 0, 'id': f'call_{prompt}', 'type': 'function',
                'function': {'name': name, 'arguments': json.dumps(args)}}]}
            finish = 'tool_calls'
        self.wfile.write(('data: ' + json.dumps({'choices': [{'delta': delta,
            'finish_reason': finish}]}) + '\n\ndata: [DONE]\n\n').encode())
        self.wfile.flush()


if __name__ == '__main__':
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/takiza').resolve()
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), API)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix='takiza-file-permission-') as directory:
            root = Path(directory)
            outside = root / 'external.txt'
            workspace = root / 'workspace'
            (workspace / '.takiza').mkdir(parents=True)
            (workspace / '.takiza/config.json').write_text(json.dumps({
                'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Amber',
                'mode': 'manual', 'model': 'test-model', 'api_key': 'test-only',
                'base_url': f'http://127.0.0.1:{server.server_port}/v1', 'auto_approve': False,
            }))
            terminal = Terminal(binary, workspace, 100, 40, initial_prompt='inside')

            def screen():
                return '\n'.join(Screen(terminal.output, 100, 40).text)

            def approval(prompt):
                terminal.send(prompt + '\r')
                terminal.wait(lambda: 'File write permission' in screen(), 'external approval missing')
                text = screen()
                assert 'Requested path' in text and 'Resolved path' in text, text
                assert 'Allow once' in text and 'Deny' in text and 'Trust' not in text, text

            def done(prompt):
                terminal.wait(lambda: f'DONE_{prompt}' in screen(), f'{prompt} did not finish')

            def show_diff(*markers):
                for _ in range(4):
                    if all(marker in screen() for marker in markers):
                        return
                    terminal.send('\x1b[6~')
                raise AssertionError(f'diff not visible after scrolling: {screen()}')

            try:
                done('inside')
                assert (workspace / 'inside.txt').read_text() == 'INSIDE'
                assert b'File write permission' not in terminal.output
                approval('deny')
                assert not outside.exists()
                show_diff('+FIRST')
                terminal.send('\x1b')
                done('deny')
                assert not outside.exists()
                assert 'Declined by you. Nothing was executed.' in screen()
                approval('allow')
                assert not outside.exists()
                terminal.send('\r')
                done('allow')
                assert outside.read_text() == 'FIRST'
                approval('edit')
                show_diff('-FIRST', '+SECOND')
                assert outside.read_text() == 'FIRST'
                terminal.send('\r')
                done('edit')
                assert outside.read_text() == 'SECOND'
                approval('race')
                outside.write_text('CONCURRENT')
                terminal.send('\r')
                done('race')
                assert outside.read_text() == 'CONCURRENT'
                assert 'changed after review' in screen()
                # A failed preparation must also release the tool-output channel.
                (workspace / 'broken').symlink_to(root / 'missing')
                terminal.send('broken\r')
                done('broken')
                assert not (root / 'missing').exists()
                terminal.send('/exit\r')
                terminal.wait(lambda: terminal.process.poll() is not None, 'exit stalled')
                assert terminal.process.returncode == 0
            except Exception:
                Path('/tmp/takiza-file-permission-pty.log').write_bytes(terminal.output)
                raise
            finally:
                terminal.close()
            auto = Terminal(binary, workspace, 100, 40, initial_prompt='auto', cli_args=['-y'])
            try:
                auto.wait(lambda: 'DONE_auto' in '\n'.join(Screen(auto.output, 100, 40).text), 'auto-approve failed')
                assert outside.read_text() == 'FIRST'
                assert b'File write permission' not in auto.output
                auto.send('/exit\r')
                auto.wait(lambda: auto.process.poll() is not None, 'auto exit stalled')
            finally:
                auto.close()
        print('PASS file approvals: workspace, deny, allow once, edit diff, concurrent changes, invalid destination, -y')
    finally:
        server.shutdown()
