"""Question panel placement and readable transcript, with an isolated mock API."""
import http.server
import json
from pathlib import Path
import sys
import tempfile
import threading
from tui_queue_pty import Terminal, Screen

QUESTIONS = [{'question': f'Prompt {i}', 'header': 'Preferences',
              'options': [f'Choice {i}', 'Alternative'], 'allow_custom': True}
             for i in range(5)]


class API(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        self.send_response(200)
        if not body.get('stream'):
            self.send_header('Content-Type', 'application/json')
            self.end_headers()
            self.wfile.write(b'{"choices":[{"message":{"content":"Questions test"}}]}')
            return
        if any(message.get('role') == 'tool' for message in body['messages']):
            delta, finish = {'content': 'QUESTIONS_DONE'}, 'stop'
        else:
            delta = {'tool_calls': [{'index': 0, 'id': 'questions', 'type': 'function',
                                    'function': {'name': 'ask_question',
                                                 'arguments': json.dumps({'questions': QUESTIONS})}}]}
            finish = 'tool_calls'
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()
        chunk = {'choices': [{'delta': delta, 'finish_reason': finish}]}
        self.wfile.write(('data: ' + json.dumps(chunk) + '\n\ndata: [DONE]\n\n').encode())


def run(binary, cols, rows, cancel=False):
    with tempfile.TemporaryDirectory(prefix='takiza-questions-') as directory:
        workspace = Path(directory)
        (workspace / '.takiza').mkdir()
        (workspace / '.takiza/config.json').write_text(json.dumps({
            'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Nord', 'mode': 'manual',
            'model': 'test-model', 'api_key': 'test-only',
            'base_url': f'http://127.0.0.1:{server.server_port}/v1',
        }))
        terminal = Terminal(binary, workspace, cols, rows)
        def screen():
            return Screen(terminal.output, cols, rows).text
        def panel(title):
            terminal.wait(lambda: any(title in line for line in screen()), f'{title} missing')
            assert screen()[-1].startswith('╰'), '\n'.join(screen())
        try:
            panel('Question [1/5]')
            if cancel:
                terminal.send('\x03')
                terminal.wait(lambda: b'QUESTIONS_DONE' in terminal.output, 'agent did not resume after dismissing questions')
                terminal.pump(.2)
                assert not any('Question [' in line for line in screen()), '\n'.join(screen())
                print(f'PASS questions {cols}x{rows}: cancel removes panel')
                return
            terminal.resize(cols - 3, rows - 2)
            cols, rows = cols - 3, rows - 2
            panel('Question [1/5]')
            for i in range(5):
                panel(f'Question [{i + 1}/5]')
                terminal.send('\r')
            panel('Review answers')
            terminal.send('y')
            terminal.wait(lambda: b'QUESTIONS_DONE' in terminal.output, 'questionnaire did not finish')
            terminal.pump(.2)
            text = '\n'.join(screen())
            for i in range(5):
                assert f'Prompt {i}' in text and f'Choice {i}' in text, text
            for bad in ['{"questions"', '📋', '+7 lines', 'Question:', 'Answer:', 'Review answers']:
                assert bad not in text, text
            before = [line.rstrip() for line in screen() if 'question' in line or 'answer' in line]
            terminal.send('\x0f')
            after = [line.rstrip() for line in screen() if 'question' in line or 'answer' in line]
            assert before == after, (before, after)
            print(f'PASS questions {cols}x{rows}: bottom anchor, resize, review, all answers visible, stable redraw')
        finally:
            terminal.close()


if __name__ == '__main__':
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/takiza').resolve()
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), API)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        run(binary, 100, 36)
        run(binary, 60, 30)
        run(binary, 80, 24, cancel=True)
    finally:
        server.shutdown()
