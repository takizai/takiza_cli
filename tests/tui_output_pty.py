"""Long command output previews three rows; model replies remain complete; Ctrl+O expands and collapses."""
import http.server
import json
from pathlib import Path
import sys
import tempfile
import threading
from tui_queue_pty import Terminal, Screen

BODY = '\n'.join(f'RESPONSE_ROW_{i:02d}' for i in range(12)) + '\nFULL_RESPONSE_TAIL'

class API(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_): pass
    def do_POST(self):
        data = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        self.send_response(200); self.end_headers()
        if not data.get('stream'):
            self.wfile.write(b'{"choices":[{"message":{"content":"Output test"}}]}'); return
        event = {'choices':[{'delta':{'content':BODY},'finish_reason':'stop'}]}
        self.wfile.write(('data: '+json.dumps(event)+'\n\ndata: [DONE]\n\n').encode())


def run(binary):
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), API)
    threading.Thread(target=server.serve_forever,daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix='takiza-output-') as directory:
            workspace = Path(directory); (workspace/'.takiza').mkdir()
            fixture = Path(__file__).parent/'fixtures/integration_server.py'
            (workspace/'.takiza/config.json').write_text(json.dumps({
                'agreed_to_terms':True,'terms_version':'1.0.0','theme':'Amber','mode':'manual',
                'model':'test-model','api_key':'test-only','base_url':f'http://127.0.0.1:{server.server_port}/v1',
                'lspServers':{'rust':{'command':sys.executable,'args':[str(fixture.resolve()),'lsp'],
                                     'languages':['rust'],'rootMarkers':[]}}}))
            t=Terminal(binary,workspace,40,30,initial_prompt=None)
            def screen(): return '\n'.join(Screen(t.output,40,30).text)
            try:
                t.wait(lambda:'You' in screen(),'editor missing')
                t.send('/lsp connect rust\r')
                t.wait(lambda:'Connected' in screen() and 'Ctrl+O' in screen(),'connection preview missing')
                assert 'workspaceSymbolProvider' not in ''.join(screen().split()),screen()
                t.send('\x0f')
                t.wait(lambda:'workspaceSymbolProvider' in ''.join(screen().split()),'LSP status did not expand')
                t.send('\x0f')
                t.wait(lambda:'workspaceSymbolProvider' not in ''.join(screen().split()),'LSP status did not collapse')
                t.send('show long response\r')
                t.wait(lambda:'Completed in' in screen(),'response missing')
                assert 'RESPONSE_ROW_00' in screen() and 'RESPONSE_ROW_02' in screen(),screen()
                assert 'FULL_RESPONSE_TAIL' in screen(),screen()
                t.send('\x0f');t.wait(lambda:'FULL_RESPONSE_TAIL' in screen(),'expansion hid model reply')
                t.send('\x0f');t.wait(lambda:'FULL_RESPONSE_TAIL' in screen(),'collapse hid model reply')
                t.send('/help\r');t.wait(lambda:'Type while the agent' in screen(),'help preview missing')
                assert 'CLI Flags' not in screen(),screen()
                t.send('\x0f');t.wait(lambda:'CLI Flags' in screen(),'help did not expand')
                t.send('\x0f');t.wait(lambda:'CLI Flags' not in screen(),'help did not collapse')
                t.send('/exit\r');t.wait(lambda:t.process.poll() is not None,'exit stalled')
                sid=(workspace/'.takiza/latest_session').read_text().strip()
                saved=json.loads((workspace/'.takiza/sessions'/f'{sid}.json').read_text())
                assert any(item.get('AssistantMessage')==BODY for item in saved['history']), 'full response was truncated on disk'
                assert any('workspaceSymbolProvider' in item.get('ToolLog','') for item in saved['history'])
                print('PASS output: LSP status, assistant reply, help, Ctrl+O both directions, complete saved history')
            except Exception:
                Path('/tmp/takiza-output-pty.log').write_bytes(t.output); raise
            finally:t.close()
    finally:server.shutdown();server.server_close()

if __name__=='__main__':run(Path(sys.argv[1] if len(sys.argv)>1 else 'target/debug/takiza').resolve())
