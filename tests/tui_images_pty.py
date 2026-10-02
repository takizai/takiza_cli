"""Clipboard images and file drops, with a fake clipboard and local API."""
import base64
import http.server
import json
import os
from pathlib import Path
import shlex
import sys
import tempfile
import threading
from tui_queue_pty import Terminal, Screen

PNG = base64.b64decode('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=')
requests = []
gates = [threading.Event() for _ in range(4)]


class API(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        self.send_response(200)
        if not body.get('stream'):
            self.send_header('Content-Type', 'application/json')
            self.end_headers()
            self.wfile.write(b'{"choices":[{"message":{"content":"Image test"}}]}')
            return
        index = len(requests)
        requests.append(body)
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()
        try:
            chunk = {'choices': [{'delta': {'content': f'RESPONSE_{index}'}}]}
            self.wfile.write(('data: ' + json.dumps(chunk) + '\n\n').encode())
            self.wfile.flush()
            gates[index].wait(20)
            self.wfile.write(b'data: [DONE]\n\n')
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass


def run(binary):
    with tempfile.TemporaryDirectory(prefix='takiza-images-') as directory:
        workspace = Path(directory)
        (workspace / '.takiza').mkdir()
        (workspace / '.takiza/config.json').write_text(json.dumps({
            'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Monochrome',
            'mode': 'manual', 'model': 'test-model', 'api_key': 'test-only',
            'base_url': f'http://127.0.0.1:{server.server_port}/v1',
        }))
        types = workspace / 'clipboard-types'
        files = workspace / 'clipboard-files'
        text = workspace / 'clipboard-text'
        image = workspace / 'image with spaces.png'
        image.write_bytes(PNG)
        document = workspace / 'document with spaces.pdf'
        document.write_text('%PDF DOCUMENT_CONTENT_MUST_STAY_LOCAL')
        tools = workspace / 'test-bin'
        tools.mkdir()
        paste = tools / 'wl-paste'
        paste.write_text('''#!/bin/sh
if [ "$1" = "--list-types" ]; then cat "$TAKIZA_CLIP_TYPES"; exit; fi
case "$3" in
image/*) cat "$TAKIZA_CLIP_IMAGE" ;;
text/uri-list|x-special/gnome-copied-files) cat "$TAKIZA_CLIP_FILES" ;;
*) cat "$TAKIZA_CLIP_TEXT" ;;
esac
''')
        paste.chmod(0o755)
        terminal = Terminal(binary, workspace, 100, 34, {
            'PATH': str(tools) + os.pathsep + os.environ['PATH'],
            'TAKIZA_CLIP_TYPES': str(types), 'TAKIZA_CLIP_FILES': str(files),
            'TAKIZA_CLIP_IMAGE': str(image), 'TAKIZA_CLIP_TEXT': str(text),
        })
        def screen():
            return '\n'.join(Screen(terminal.output, 100, 34).text)
        def queued(count):
            terminal.send('\r')
            terminal.wait(lambda: f'{count} queued' in screen(), 'image/file draft did not queue')
        def latest_user(index):
            return next(message for message in reversed(requests[index]['messages']) if message['role'] == 'user')
        def check_image(message):
            content = message['content']
            assert isinstance(content, list) and len(content) == 2, content
            assert content[0]['type'] == 'text'
            assert content[1]['type'] == 'image_url'
            url = content[1]['image_url']['url']
            assert url.startswith('data:image/png;base64,')
            assert base64.b64decode(url.split(',', 1)[1]) == PNG
        try:
            terminal.wait(lambda: len(requests) == 1, 'initial request missing')
            types.write_text('image/png\ntext/plain\n')
            text.write_text('fallback')
            terminal.send('describe \x16')  # Ctrl+V reads the binary image clipboard.
            terminal.wait(lambda: '[Image:' in screen(), 'clipboard image not attached')
            assert len(requests) == 1, 'paste sent the request automatically'
            queued(1)
            # A document clipboard may also advertise an image thumbnail.
            types.write_text('image/png\ntext/uri-list\n')
            files.write_text(document.as_uri() + '\n')
            terminal.send('\x1b[<2;10;33M')
            assert 'document with spaces.pdf' in screen(), 'document path not pasted'
            assert 'DOCUMENT_CONTENT_MUST_STAY_LOCAL' not in screen()
            queued(2)
            # Bracketed file drops with shell quoting preserve paths with spaces.
            paths = shlex.quote(str(image)) + ' ' + shlex.quote(str(document))
            terminal.send('\x1b[200~' + paths + '\x1b[201~')
            terminal.wait(lambda: '[Image:' in screen(), 'dropped image not attached')
            queued(3)
            gates[0].set()
            terminal.wait(lambda: len(requests) == 2, 'queued image request missing')
            check_image(latest_user(1))
            gates[1].set()
            terminal.wait(lambda: len(requests) == 3, 'document request missing')
            assert isinstance(latest_user(2)['content'], str), 'document sent as a binary attachment'
            assert str(document) in latest_user(2)['content']
            gates[2].set()
            terminal.wait(lambda: len(requests) == 4, 'mixed file drop request missing')
            check_image(latest_user(3))
            assert str(document) in latest_user(3)['content'][0]['text']
            for request in requests:
                encoded = json.dumps(request)
                assert 'DOCUMENT_CONTENT_MUST_STAY_LOCAL' not in encoded, 'document body embedded'
                assert all('image_urls' not in message for message in request['messages']), 'session metadata leaked to provider'
            gates[3].set()
            def saved():
                sessions = list((workspace / '.takiza/sessions').glob('*.json'))
                return next((json.loads(path.read_text()) for path in sessions
                    if len([message for message in json.loads(path.read_text())['messages'] if message['role'] == 'user']) == 4), None)
            terminal.wait(lambda: saved() is not None, 'session not saved')
            users = [message for message in saved()['messages'] if message['role'] == 'user']
            assert len(users[1]['image_urls']) == 1 and len(users[3]['image_urls']) == 1
            assert 'image_urls' not in users[2]
            assert len(list((workspace / '.takiza/attachments').glob('*.png'))) == 2
            assert image.read_bytes() == PNG and 'DOCUMENT_CONTENT_MUST_STAY_LOCAL' in document.read_text()
            terminal.send('/exit\r')
            terminal.wait(lambda: terminal.process.poll() is not None, 'exit failed')
            print('PASS images: binary Ctrl+V, URI file clipboard over thumbnails, quoted drops, queued multimodal API, document paths only, persistence')
        finally:
            for gate in gates:
                gate.set()
            terminal.close()


if __name__ == '__main__':
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/takiza').resolve()
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), API)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        run(binary)
    finally:
        server.shutdown()
