"""Russian/Chinese UI regression, including stable queue markers and mode identifiers."""
import http.server
import json
from pathlib import Path
import sys
import tempfile
import threading

from tui_queue_pty import Terminal, Screen

started = threading.Event()
release = threading.Event()


class API(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        self.send_response(200)
        if not body.get('stream'):
            self.send_header('Content-Type', 'application/json')
            self.end_headers()
            self.wfile.write(b'{"choices":[{"message":{"content":"Localization test"}}]}')
            return
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()
        started.set()
        release.wait(15)
        try:
            self.wfile.write(b'data: {"choices":[{"delta":{"content":"DONE"},"finish_reason":"stop"}]}\n\ndata: [DONE]\n\n')
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass


def run(binary, language):
    expected = {
        'ru': ['Авто', '[активно]', 'В этом чате нет контрольных точек.',
               'Контрольные точки сохраняются перед запросами.', '(настроен)',
               'Сессия missing не найдена.', 'Неизвестный сервер MCP',
               'Неизвестный сервер LSP', 'План', '⏳ В очереди #1: queued-text'],
        'zh': ['自动', '[当前]', '此聊天中没有检查点。',
               '检查点会在提交提示前保存。', '（已配置）',
               '未找到会话 missing。', '未知 MCP 服务器',
               '未知 LSP 服务器', '计划', '⏳ 排队中 #1：queued-text'],
    }[language]
    started.clear()
    release.clear()
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), API)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix='takiza-localization-') as directory:
            workspace = Path(directory)
            (workspace / '.takiza').mkdir()
            config = workspace / '.takiza/config.json'
            config.write_text(json.dumps({
                'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Amber',
                'language': language, 'mode': 'manual', 'model': 'test-model',
                'api_key': 'test', 'base_url': f'http://127.0.0.1:{server.server_port}/v1',
            }))
            terminal = Terminal(binary, workspace, 120, 36, initial_prompt=None, controlling_tty=True)
            def screen():
                return '\n'.join(Screen(terminal.output, 120, 36).text)
            def see(text):
                terminal.wait(lambda: text in screen(), f'{language}: missing {text!r}\n{screen()}')
            try:
                see(expected[0])
                terminal.send('/theme\r')
                see(expected[1])
                assert '[active]' not in screen(), screen()
                terminal.send('\x1b')
                # The fixture model has no effort capabilities. The CLI must
                # translate the unavailable notice instead of inventing levels.
                terminal.send('/effort\r')
                see('Модель не поддерживает смену effort' if language == 'ru' else '此模型不支持调整推理强度')
                terminal.send('/mode\r')
                see('сторонний провайдер' if language == 'ru' else '第三方提供商')
                terminal.send('/usage\r')
                see('Актуальные расход' if language == 'ru' else '实际用量')
                terminal.send('\x1b')
                terminal.send('/rewind\r')
                see(expected[2])
                see(expected[3])
                terminal.send('\x1b')
                terminal.send('/status\r')
                see(expected[4])
                terminal.send('\x1b')
                terminal.send('/resume missing\r')
                see(expected[5])
                terminal.send('/mcp connect missing\r')
                see(expected[6])
                terminal.send('/lsp connect missing\r')
                see(expected[7])
                terminal.send('/permissions plan\r')
                see(expected[8])
                assert json.loads(config.read_text())['execution_mode'] == 'plan'
                terminal.send('hold\r')
                terminal.wait(started.is_set, 'mock request missing')
                terminal.send('queued-text\r')
                see(expected[9])
                assert 'Queued #' not in screen(), screen()
                # Redraw must translate the marker again without changing stored queue identity.
                terminal.resize(100, 30)
                terminal.wait(lambda: expected[9] in '\n'.join(Screen(terminal.output, 100, 30).text),
                              'queue translation lost after resize')
                terminal.send('/clear\r')
                terminal.wait(lambda: 'queued-text' not in '\n'.join(Screen(terminal.output, 100, 30).text),
                              'queue marker remained after clearing the session')
                saved = list((workspace / '.takiza/sessions').glob('*.json'))
                assert saved, 'no session saved'
                assert any('⏳ Queued #1: queued-text' in path.read_text() for path in saved), \
                    'stored queue marker must stay independent of the display language'
                print(f'PASS {language}: badges, checkpoints, API key, sessions, integrations, mode, queue redraw/clear')
            finally:
                release.set()
                terminal.close()
    finally:
        server.shutdown()
        server.server_close()


if __name__ == '__main__':
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/takiza').resolve()
    for language in ['ru', 'zh']:
        run(binary, language)
