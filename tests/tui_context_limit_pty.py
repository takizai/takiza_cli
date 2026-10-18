"""Global context overrides, proactive compaction and API-discovered caps."""
import http.server
import json
from pathlib import Path
import sys
import tempfile
import threading
from tui_queue_pty import Terminal, Screen


def run(binary, scenario):
    chat_requests = []
    summaries = []
    order = []
    class API(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_): pass
        def reply(self, body, status=200):
            data=json.dumps(body).encode()
            self.send_response(status)
            self.send_header('Content-Type','application/json')
            self.send_header('Content-Length',str(len(data)))
            self.end_headers();self.wfile.write(data)
        def do_GET(self):
            model={'id':'test-model'}
            if scenario=='proactive': model['context_length']=1000000
            if scenario=='clamp': model['context_length']=262144
            self.reply({'data':[model]} if self.path=='/v1/models' else model if self.path.startswith('/v1/models/') else {})
        def do_POST(self):
            body=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            if not body.get('tools'):
                summary='Summarize conversation history' in body['messages'][0].get('content','')
                if summary: summaries.append(body);order.append('summary')
                self.reply({'choices':[{'message':{'content':'Concise summary of earlier work.' if summary else 'Context test'}}], 'usage':{'total_tokens':50}})
                return
            chat_requests.append(body);order.append('chat')
            if scenario=='learn' and len(chat_requests)==2:
                self.reply({'error':{'code':'context_length_exceeded','message':'maximum context length is 65536 tokens'}},400)
                return
            text='FIRST_DONE\n' + 'Earlier answer details. '*600 if len(chat_requests)==1 else 'SECOND_DONE'
            tokens=130000 if scenario=='proactive' and len(chat_requests)==1 else 1000
            events=[{'choices':[{'delta':{'content':text},'finish_reason':'stop'}]}, {'choices':[],'usage':{'total_tokens':tokens}}]
            data=(''.join('data: '+json.dumps(event)+'\n\n' for event in events)+'data: [DONE]\n\n').encode()
            self.send_response(200);self.send_header('Content-Type','text/event-stream');self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
    server=http.server.ThreadingHTTPServer(('127.0.0.1',0),API)
    threading.Thread(target=server.serve_forever,daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix='takiza-context-cap-') as directory:
            workspace=Path(directory);(workspace/'.takiza').mkdir()
            configured=131072 if scenario=='proactive' else 1000000
            config={'agreed_to_terms':True,'terms_version':'1.0.0','theme':'Monochrome','mode':'manual',
                    'model':'test-model','api_key':'test-only','base_url':f'http://127.0.0.1:{server.server_port}/v1',
                    'context_window':configured}
            path=workspace/'.takiza/config.json';path.write_text(json.dumps(config))
            terminal=Terminal(binary,workspace,130,32,initial_prompt=None)
            def screen():return '\n'.join(Screen(terminal.output,130,32).text)
            try:
                expected='0/256K' if scenario=='clamp' else '0/128K' if scenario=='proactive' else '0/1M'
                terminal.wait(lambda:expected in screen(),'configured/effective context not displayed')
                if scenario=='clamp':
                    terminal.wait(lambda:'config.json is unchanged.' in ' '.join(screen().split()),'unsupported context warning missing')
                    assert json.loads(path.read_text())['context_window']==1000000
                    terminal.send('/exit\r')
                else:
                    terminal.send('first prompt\r')
                    terminal.wait(lambda:'1K/1M' in screen() if scenario=='learn' else '130K/128K' in screen(),'first usage missing',timeout=15)
                    terminal.send('second prompt must remain intact\r')
                    terminal.wait(lambda:'SECOND_DONE' in screen(),'automatic compaction did not continue the turn',timeout=20)
                    assert len(summaries)==1, len(summaries)
                    assert order==(['chat','summary','chat'] if scenario=='proactive' else ['chat','chat','summary','chat']),order
                    assert chat_requests[-1]['messages'][-1]['content']=='second prompt must remain intact'
                    if scenario=='learn':
                        terminal.wait(lambda:'1K/64K' in screen(),'API cap was not applied to this chat')
                        terminal.wait(lambda:'config.json is unchanged.' in ' '.join(screen().split()),'learned-cap warning missing')
                    assert json.loads(path.read_text())['context_window']==configured,'runtime cap rewrote preferences'
                    terminal.send('/exit\r')
                terminal.wait(lambda:terminal.process.poll() is not None,'exit stalled')
                assert terminal.process.returncode==0
                print('PASS context limit:',scenario)
            except Exception:
                print('Order:',order,'Screen:',screen());raise
            finally:terminal.close()
    finally:server.shutdown();server.server_close()

if __name__=='__main__':
    binary=Path(sys.argv[1] if len(sys.argv)>1 else 'target/debug/takiza').resolve()
    for scenario in ['proactive','clamp','learn']:run(binary,scenario)
